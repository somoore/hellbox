//! One-command install: prerequisites stack -> config -> build -> up -> open.
//!
//! The CloudFormation template and the capsule build context are embedded in
//! the binary, so this works from a brew/winget install with no repo clone.

use anyhow::{Context, Result, bail};
use aws_sdk_cloudformation::Client as CfnClient;
use aws_sdk_cloudformation::error::ProvideErrorMetadata;
use aws_sdk_cloudformation::types::{Capability, Parameter};

use crate::aws;
use crate::config::{Config, DEFAULT_REGION, hellbox_dir};
use crate::embedded::STACK_TEMPLATE;
use crate::poll::{PollOpts, poll_until};

fn custom_template_path() -> Result<std::path::PathBuf> {
    Ok(hellbox_dir()?.join("stack.yaml"))
}

/// The template deploys use: the user's edited copy when one exists
/// (`hellbox deploy edit`), else the one baked into the binary.
fn template_body() -> Result<(String, bool)> {
    let path = custom_template_path()?;
    if path.is_file() {
        let body = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        return Ok((body, true));
    }
    Ok((STACK_TEMPLATE.to_string(), false))
}

/// `hellbox deploy edit`: materialize the template and open it in $EDITOR.
pub fn edit() -> Result<()> {
    let path = custom_template_path()?;
    if !path.is_file() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&path, STACK_TEMPLATE)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    let fallback_editor = if cfg!(windows) { "notepad" } else { "vi" };
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| fallback_editor.to_string());
    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("launching editor '{editor}' (set $EDITOR)"))?;
    if !status.success() {
        bail!("editor exited with {status}");
    }
    println!(
        "saved {p} — `hellbox deploy` now uses this template; delete {p} to return to the built-in one",
        p = path.display()
    );
    Ok(())
}

fn parse_parameters(raw: &[String]) -> Result<Vec<Parameter>> {
    raw.iter()
        .map(|kv| {
            let (k, v) = kv.split_once('=').with_context(|| {
                format!("--parameter '{kv}' is not KEY=VALUE (e.g. -p BuildServicePrincipal=lambda.amazonaws.com)")
            })?;
            Ok(Parameter::builder()
                .parameter_key(k)
                .parameter_value(v)
                .build())
        })
        .collect()
}

pub async fn run(name: &str, region_flag: Option<&str>, parameters: &[String]) -> Result<()> {
    let region = resolve_region(region_flag).await;
    let stack = std::env::var("HELLBOX_STACK")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Hellbox".to_string());
    let (template, customized) = template_body()?;
    let params = parse_parameters(parameters)?;

    let (sdk, identity) = aws::resolve(&region).await?;
    println!(
        "==> AWS identity: {} (account {})",
        identity.arn, identity.account
    );
    let cfn = CfnClient::new(&sdk);

    println!("==> Deploying AWS prerequisites  (stack: {stack}, region: {region})");
    if customized {
        println!(
            "    using customized template {}",
            custom_template_path()?.display()
        );
    }
    ensure_stack(&cfn, &stack, &template, &params).await?;

    let cfg = write_config(&cfn, &stack, &region, &identity).await?;
    println!("==> Wrote {}", Config::path()?.display());

    // Reconcile local state from AWS by name. Covers the reinstall / new-machine
    // case: the stack (and maybe the image and a running MicroVM) still exist,
    // but ~/.hellbox was wiped. Rediscover them so build/up/play/rm all see
    // reality instead of erroring on an image the local state doesn't know about.
    reconcile_state(&sdk, name, &identity, &region).await?;

    // Idempotent rerun: if this capsule's image already exists and is active,
    // reuse it instead of failing the build step. `hellbox rm` first to rebuild.
    if existing_image_active(&sdk, name).await {
        println!(
            "==> Image for '{name}' already exists — reusing it (run `hellbox rm` first to rebuild)"
        );
    } else {
        println!(
            "==> Building the DOOM MicroVM image  (compiles the engine + fetches the WAD; a few minutes)"
        );
        super::build::run(name, None, None).await?;
    }
    let _ = cfg; // config is read from disk by the subcommands
    // strict: deploy only succeeds once the page and every stream channel
    // (video/audio/input) verify end to end through the proxy into the VM.
    // play handles launch-if-needed, so a rerun never starts a second machine.
    println!("==> Launching DOOM  (http://127.0.0.1:6080)");
    super::play::run_with_verify(name, true).await
}

async fn resolve_region(flag: Option<&str>) -> String {
    let env = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());
    if let Some(r) = flag
        .map(str::to_string)
        .or_else(|| env("AWS_REGION"))
        .or_else(|| env("AWS_DEFAULT_REGION"))
        .or_else(|| Config::load().ok().map(|c| c.region))
    {
        return r;
    }
    // The active AWS profile's `region =` (~/.aws/config), like the AWS CLI uses.
    // ProfileFileRegionProvider only, NOT the default chain: the default chain
    // includes an IMDS probe that times out with a noisy WARN off-EC2 (a laptop),
    // and we already fall back to us-east-1 below.
    use aws_config::meta::region::ProvideRegion;
    if let Some(r) = aws_config::profile::region::ProfileFileRegionProvider::default()
        .region()
        .await
    {
        return r.to_string();
    }
    DEFAULT_REGION.to_string()
}

/// Create the stack, or update it if it already exists; idempotent reruns
/// ("no updates are to be performed") are fine.
async fn ensure_stack(
    cfn: &CfnClient,
    stack: &str,
    template: &str,
    params: &[Parameter],
) -> Result<()> {
    let create = cfn
        .create_stack()
        .stack_name(stack)
        .template_body(template)
        .set_parameters(Some(params.to_vec()))
        .capabilities(Capability::CapabilityIam)
        .send()
        .await;

    let wait_terminal: &[&str] = &[
        "CREATE_COMPLETE",
        "UPDATE_COMPLETE",
        "CREATE_FAILED",
        "ROLLBACK_COMPLETE",
        "ROLLBACK_FAILED",
        "UPDATE_ROLLBACK_COMPLETE",
        "UPDATE_ROLLBACK_FAILED",
    ];

    match create {
        Ok(_) => {}
        Err(e) if error_says(&e.message(), "already exists") => {
            let update = cfn
                .update_stack()
                .stack_name(stack)
                .template_body(template)
                .set_parameters(Some(params.to_vec()))
                .capabilities(Capability::CapabilityIam)
                .send()
                .await;
            match update {
                Ok(_) => {}
                // Stack already matches the template — nothing to wait for.
                Err(e) if error_says(&e.message(), "No updates are to be performed") => {
                    return Ok(());
                }
                Err(e) => return Err(e).context("update_stack"),
            }
        }
        Err(e) => return Err(e).context("create_stack"),
    }

    let status = poll_until(
        &format!("stack {stack}"),
        wait_terminal,
        PollOpts {
            interval: std::time::Duration::from_secs(5),
            timeout: std::time::Duration::from_secs(600),
        },
        || async {
            let out = cfn
                .describe_stacks()
                .stack_name(stack)
                .send()
                .await
                .context("describe_stacks")?;
            Ok(out
                .stacks()
                .first()
                .and_then(|s| s.stack_status())
                .map(|s| s.as_str().to_string())
                .unwrap_or_else(|| "UNKNOWN".to_string()))
        },
    )
    .await?;

    if !matches!(status.as_str(), "CREATE_COMPLETE" | "UPDATE_COMPLETE") {
        bail!(
            "stack '{stack}' did not deploy cleanly (status {status}) — check the CloudFormation console"
        );
    }
    Ok(())
}

fn error_says(msg: &Option<&str>, needle: &str) -> bool {
    msg.map(|m| m.contains(needle)).unwrap_or(false)
}

/// Rediscover an existing image and MicroVM for `name` from AWS and write them
/// into local state. No-op when nothing exists (fresh account) or when local
/// state already has them. The image ARN is deterministic from name, so a
/// wiped ~/.hellbox can still find the image the stack's account owns.
async fn reconcile_state(
    sdk: &aws_config::SdkConfig,
    name: &str,
    identity: &aws::Identity,
    region: &str,
) -> Result<()> {
    use crate::state::State;
    let microvm = aws_sdk_lambdamicrovms::Client::new(sdk);

    // Deterministic image ARN: arn:aws:lambda:<region>:<account>:microvm-image:<name>
    let image_arn = format!(
        "arn:aws:lambda:{region}:{}:microvm-image:{name}",
        identity.account
    );
    let image = microvm
        .get_microvm_image()
        .image_identifier(&image_arn)
        .send()
        .await
        .ok()
        .filter(|o| o.state().as_str() == "CREATED");
    let Some(image) = image else {
        return Ok(()); // no image in AWS — a normal fresh deploy will build one
    };
    let image_version = image.latest_active_image_version().map(str::to_string);

    // Is a MicroVM from this image still around? list_microvms carries image_arn.
    let live_microvm = microvm.list_microvms().send().await.ok().and_then(|out| {
        out.items()
            .iter()
            .find(|m| m.image_arn() == image_arn && !matches!(m.state().as_str(), "TERMINATED"))
            .map(|m| (m.microvm_id().to_string(), m.state().as_str().to_string()))
    });

    // Endpoint only exists for a live MicroVM; fetch it via get_microvm.
    let endpoint = if let Some((id, _)) = &live_microvm {
        crate::lifecycle::microvm_endpoint(&microvm, id).await.ok()
    } else {
        None
    };

    let mut state = State::load().unwrap_or_default();
    let already = state
        .get(name)
        .map(|c| c.image_arn.is_some())
        .unwrap_or(false);
    state.upsert(name, |c| {
        c.image_arn = Some(image_arn.clone());
        c.image_version = image_version.clone();
        if let Some((id, st)) = &live_microvm {
            c.microvm_id = Some(id.clone());
            c.state = Some(st.clone());
            c.endpoint = endpoint.as_deref().map(crate::lifecycle::host_of);
        }
    })?;
    if !already {
        match &live_microvm {
            Some((_, st)) => {
                println!(
                    "==> Found an existing '{name}' image and MicroVM ({st}) in AWS — imported it \
                     (likely from another computer on this AWS account)"
                );
                if crate::discover::ask(
                    "Keep and reuse it, or Terminate it and start fresh? [K/t]:",
                    'k',
                ) == 't'
                {
                    super::down::run(name).await.ok();
                    println!(
                        "terminated the existing '{name}' machine; deploy will launch a fresh one"
                    );
                }
            }
            None => println!("==> Found an existing '{name}' image in AWS — adopting it"),
        }
    }
    Ok(())
}

/// True when local state records an image for this capsule and the service
/// reports it CREATED with an active version.
async fn existing_image_active(sdk: &aws_config::SdkConfig, name: &str) -> bool {
    let Some(arn) = crate::state::State::load()
        .ok()
        .and_then(|s| s.get(name).and_then(|c| c.image_arn.clone()))
    else {
        return false;
    };
    let client = aws_sdk_lambdamicrovms::Client::new(sdk);
    match client
        .get_microvm_image()
        .image_identifier(&arn)
        .send()
        .await
    {
        Ok(out) => out.state().as_str() == "CREATED" && out.latest_active_image_version().is_some(),
        Err(_) => false,
    }
}

/// Read stack Outputs and write ~/.hellbox/config.toml, preserving any
/// existing display/idle/port settings. Records the account (and profile,
/// best effort) so later commands can catch a wrong-profile mixup.
async fn write_config(
    cfn: &CfnClient,
    stack: &str,
    region: &str,
    identity: &aws::Identity,
) -> Result<Config> {
    let out = cfn
        .describe_stacks()
        .stack_name(stack)
        .send()
        .await
        .context("describe_stacks (outputs)")?;
    let outputs = out
        .stacks()
        .first()
        .map(|s| s.outputs())
        .unwrap_or_default();
    let get = |key: &str| {
        outputs
            .iter()
            .find(|o| o.output_key() == Some(key))
            .and_then(|o| o.output_value())
            .map(str::to_string)
            .with_context(|| format!("stack '{stack}' has no '{key}' output"))
    };

    let existing = Config::load().ok();
    let cfg = Config {
        region: region.to_string(),
        artifact_bucket: get("ArtifactBucket")?,
        build_role_arn: get("BuildRoleArn")?,
        execution_role_arn: Some(get("ExecutionRoleArn")?),
        ingress_connector_arn: existing
            .as_ref()
            .map(|c| c.ingress_connector_arn.clone())
            .unwrap_or_default(),
        egress_connector_arn: existing
            .as_ref()
            .map(|c| c.egress_connector_arn.clone())
            .unwrap_or_default(),
        base_image_arn: format!("arn:aws:lambda:{region}:aws:microvm-image:al2023-1"),
        port: existing
            .as_ref()
            .map(|c| c.port)
            .unwrap_or(crate::config::DEFAULT_PORT),
        audio_port: existing
            .as_ref()
            .map(|c| c.audio_port)
            .unwrap_or(crate::config::DEFAULT_AUDIO_PORT),
        video_port: existing
            .as_ref()
            .map(|c| c.video_port)
            .unwrap_or(crate::config::DEFAULT_VIDEO_PORT),
        input_port: existing
            .as_ref()
            .map(|c| c.input_port)
            .unwrap_or(crate::config::DEFAULT_INPUT_PORT),
        display: existing
            .as_ref()
            .and_then(|c| c.display.clone())
            .or_else(|| Some("h264".to_string())),
        idle_suspend_minutes: existing.and_then(|c| c.idle_suspend_minutes),
        aws_account_id: Some(identity.account.clone()),
        aws_profile: std::env::var("AWS_PROFILE")
            .ok()
            .filter(|p| !p.trim().is_empty()),
    };
    cfg.save()?;
    Ok(cfg)
}
