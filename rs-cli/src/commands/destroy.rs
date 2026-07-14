//! Full teardown: microvm + image, artifact bucket contents, the
//! prerequisites stack, and local config/state. The inverse of `deploy`.
//!
//! SAFETY: destroy must never touch anything that Hellbox did not create.
//! Every AWS deletion is gated on proof of ownership — the stack must carry
//! the Hellbox template markers, and the bucket is only emptied if it is the
//! exact bucket that stack reports as its own output (the local config alone
//! is never trusted; it could be stale or hand-edited). A mismatch aborts the
//! whole operation rather than guessing.

use anyhow::{Context, Result, bail};
use aws_sdk_cloudformation::types::Stack;

use crate::aws::Aws;
use crate::config::Config;
use crate::poll::{PollOpts, poll_until};
use crate::state::State;

pub async fn run(name: &str, yes: bool) -> Result<()> {
    let cfg = Config::load()?;
    let state = State::load()?;
    let stack_name = std::env::var("HELLBOX_STACK")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Hellbox".to_string());

    // Credentials must work AND point at the account this config was written
    // for; destroying "Hellbox" in some other account would be someone else's.
    let (sdk, identity) =
        crate::aws::resolve_with_profile(&cfg.region, cfg.aws_profile.as_deref()).await?;
    crate::aws::require_same_account(&cfg, &identity)?;
    let aws = Aws::from_sdk_config(&sdk);
    let capsule = state.get(name).cloned();

    // Build the exact plan first, verifying ownership of everything on it.
    let stack = describe_stack(&aws, &stack_name).await?;
    let stack_bucket = match &stack {
        Some(s) => {
            if !is_hellbox_stack(s) {
                bail!(
                    "stack '{stack_name}' in {region} does not look like a Hellbox \
                     prerequisites stack (missing the Hellbox template markers) — \
                     refusing to delete it. If you deployed with a custom stack name, \
                     set HELLBOX_STACK; otherwise remove the stack manually.",
                    region = cfg.region
                );
            }
            stack_output(s, "ArtifactBucket")
        }
        None => None,
    };
    if let Some(b) = &stack_bucket
        && *b != cfg.artifact_bucket
    {
        bail!(
            "the '{stack_name}' stack owns bucket '{b}' but ~/.hellbox/config.toml says \
             '{cfg_bucket}' — refusing to touch either until that mismatch is resolved \
             (stale config from an older deploy?). Run `hellbox deploy` to rewrite the \
             config, or fix it by hand.",
            cfg_bucket = cfg.artifact_bucket
        );
    }

    // Show exactly what goes away, and why.
    println!("hellbox destroy will remove exactly these Hellbox-created resources:");
    match &capsule {
        Some(c) => {
            if let Some(id) = &c.microvm_id {
                println!("  • MicroVM  {id}  (the running DOOM machine)");
            }
            if let Some(arn) = &c.image_arn {
                println!("  • Image    {arn}  (the baked DOOM snapshot)");
            }
            if c.microvm_id.is_none() && c.image_arn.is_none() {
                println!("  • MicroVM/image: none recorded — skipped");
            }
        }
        None => println!("  • MicroVM/image: none recorded — skipped"),
    }
    match (&stack, &stack_bucket) {
        (Some(_), Some(b)) => {
            println!(
                "  • Bucket   s3://{b}  (build contexts only; emptied so CloudFormation can delete it)"
            );
            println!(
                "  • Stack    '{stack_name}' in {}  (verified Hellbox prerequisites: the bucket + two IAM roles)",
                cfg.region
            );
        }
        (Some(_), None) => println!(
            "  • Stack    '{stack_name}' in {}  (verified Hellbox prerequisites)",
            cfg.region
        ),
        (None, _) => println!(
            "  • Stack    '{stack_name}': not found in {} — skipped",
            cfg.region
        ),
    }
    println!("  • Local    ~/.hellbox/config.toml and ~/.hellbox/state.json");
    println!("Nothing else in your AWS account is touched.");

    if !yes {
        confirm_interactive()?;
    }

    // MicroVM + image first: DeleteMicrovmImage fails while one is live, and
    // the stack can't go while the bucket still has build contexts.
    if capsule
        .as_ref()
        .map(|c| c.microvm_id.is_some() || c.image_arn.is_some())
        .unwrap_or(false)
    {
        println!("==> Removing the '{name}' microvm and image");
        super::rm::run(name).await?;
    }

    if stack.is_some() {
        if let Some(bucket) = &stack_bucket {
            println!("==> Emptying s3://{bucket}");
            empty_bucket(&aws, bucket).await?;
        }
        println!("==> Deleting CloudFormation stack: {stack_name}");
        delete_stack_and_wait(&aws, &stack_name).await?;
    }

    // Local state last, so a failed AWS teardown stays retryable.
    for file in [Config::path()?, State::path()?] {
        if file.exists() {
            std::fs::remove_file(&file).with_context(|| format!("removing {}", file.display()))?;
        }
    }
    println!(
        "destroyed. (~/.hellbox itself is left in place for any cached binary; \
         delete it whenever you like)"
    );
    Ok(())
}

/// Interactive confirmation: the user must type `destroy`.
fn confirm_interactive() -> Result<()> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        bail!("not an interactive terminal — rerun with --yes to confirm the plan above");
    }
    print!("Type \"destroy\" to confirm (anything else aborts): ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("reading confirmation")?;
    if line.trim() != "destroy" {
        bail!("aborted — nothing was deleted");
    }
    Ok(())
}

async fn describe_stack(aws: &Aws, stack_name: &str) -> Result<Option<Stack>> {
    match aws
        .cloudformation
        .describe_stacks()
        .stack_name(stack_name)
        .send()
        .await
    {
        Ok(out) => Ok(out.stacks().first().cloned()),
        Err(e) => {
            let msg = format!("{e:?}");
            if msg.contains("does not exist") {
                Ok(None)
            } else {
                Err(e).context("describe_stacks")
            }
        }
    }
}

/// A stack is only deletable if it carries the Hellbox template's markers:
/// the description and the well-known outputs.
fn is_hellbox_stack(stack: &Stack) -> bool {
    let description_ok = stack
        .description()
        .map(|d| d.contains("Hellbox prerequisites"))
        .unwrap_or(false);
    let outputs_ok = stack_output(stack, "ArtifactBucket").is_some()
        && stack_output(stack, "BuildRoleArn").is_some();
    description_ok && outputs_ok
}

fn stack_output(stack: &Stack, key: &str) -> Option<String> {
    stack
        .outputs()
        .iter()
        .find(|o| o.output_key() == Some(key))
        .and_then(|o| o.output_value())
        .map(str::to_string)
}

async fn delete_stack_and_wait(aws: &Aws, stack_name: &str) -> Result<()> {
    aws.cloudformation
        .delete_stack()
        .stack_name(stack_name)
        .send()
        .await
        .context("delete_stack")?;
    let status = poll_until(
        &format!("stack {stack_name}"),
        &["DELETE_COMPLETE", "DELETE_FAILED"],
        PollOpts {
            interval: std::time::Duration::from_secs(5),
            timeout: std::time::Duration::from_secs(600),
        },
        || async {
            match aws
                .cloudformation
                .describe_stacks()
                .stack_name(stack_name)
                .send()
                .await
            {
                Ok(out) => Ok(out
                    .stacks()
                    .first()
                    .and_then(|s| s.stack_status())
                    .map(|s| s.as_str().to_string())
                    // A deleted stack disappears from describe_stacks.
                    .unwrap_or_else(|| "DELETE_COMPLETE".to_string())),
                Err(_) => Ok("DELETE_COMPLETE".to_string()),
            }
        },
    )
    .await?;
    if status != "DELETE_COMPLETE" {
        bail!(
            "stack '{stack_name}' did not finish deleting (status {status}) — check the \
             CloudFormation console; resources may remain and still bill"
        );
    }
    Ok(())
}

async fn empty_bucket(aws: &Aws, bucket: &str) -> Result<()> {
    loop {
        let listed = match aws.s3.list_objects_v2().bucket(bucket).send().await {
            Ok(l) => l,
            // Bucket already gone (e.g. a previous partial destroy): done.
            Err(e) if format!("{e:?}").contains("NoSuchBucket") => return Ok(()),
            Err(e) => return Err(e).context("list_objects_v2"),
        };
        let keys: Vec<String> = listed
            .contents()
            .iter()
            .filter_map(|o| o.key().map(str::to_string))
            .collect();
        if keys.is_empty() {
            return Ok(());
        }
        for key in keys {
            aws.s3
                .delete_object()
                .bucket(bucket)
                .key(&key)
                .send()
                .await
                .with_context(|| format!("deleting s3://{bucket}/{key}"))?;
        }
        if listed.is_truncated() != Some(true) {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_cloudformation::types::Output;

    fn stack(desc: Option<&str>, outputs: &[(&str, &str)]) -> Stack {
        let mut b = Stack::builder()
            .stack_name("Hellbox")
            .stack_status(aws_sdk_cloudformation::types::StackStatus::CreateComplete);
        if let Some(d) = desc {
            b = b.description(d);
        }
        for (k, v) in outputs {
            b = b.outputs(Output::builder().output_key(*k).output_value(*v).build());
        }
        b.build()
    }

    #[test]
    fn hellbox_stack_markers_required_for_delete() {
        // The real stack: description + outputs present.
        assert!(is_hellbox_stack(&stack(
            Some("Hellbox prerequisites: build artifact bucket and IAM roles for Lambda MicroVMs."),
            &[
                ("ArtifactBucket", "b"),
                ("BuildRoleArn", "arn"),
                ("ExecutionRoleArn", "arn")
            ],
        )));
        // Somebody else's stack that happens to be named Hellbox: refused.
        assert!(!is_hellbox_stack(&stack(Some("My production stack"), &[])));
        assert!(!is_hellbox_stack(&stack(None, &[("ArtifactBucket", "b")])));
        // Right description but no outputs (half-created?): refused.
        assert!(!is_hellbox_stack(&stack(
            Some("Hellbox prerequisites"),
            &[]
        )));
    }
}
