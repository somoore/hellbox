//! Full teardown: microvm + image, artifact bucket contents, the
//! prerequisites stack, and local config/state. The inverse of `deploy`.

use anyhow::{Context, Result, bail};

use crate::aws::Aws;
use crate::config::Config;
use crate::poll::{PollOpts, poll_until};
use crate::state::State;

pub async fn run(name: &str, yes: bool) -> Result<()> {
    let cfg = Config::load()?;
    let stack = std::env::var("HELLBOX_STACK")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Hellbox".to_string());

    if !yes {
        bail!(
            "destroy removes the '{name}' microvm + image, empties s3://{bucket}, deletes \
             the '{stack}' CloudFormation stack in {region}, and clears local config/state. \
             Rerun with --yes to proceed.",
            bucket = cfg.artifact_bucket,
            region = cfg.region,
        );
    }

    // MicroVM + image first: DeleteMicrovmImage fails while one is live, and
    // the stack can't go while the bucket still has build contexts.
    if State::load()?.get(name).is_some() {
        println!("==> Removing the '{name}' microvm and image");
        super::rm::run(name).await?;
    } else {
        println!("==> No '{name}' capsule in local state, skipping microvm/image");
    }

    let aws = Aws::new(&cfg).await?;

    println!("==> Emptying s3://{}", cfg.artifact_bucket);
    empty_bucket(&aws, &cfg.artifact_bucket).await?;

    println!("==> Deleting CloudFormation stack: {stack}");
    aws.cloudformation
        .delete_stack()
        .stack_name(&stack)
        .send()
        .await
        .context("delete_stack")?;
    let status = poll_until(
        &format!("stack {stack}"),
        &["DELETE_COMPLETE", "DELETE_FAILED"],
        PollOpts {
            interval: std::time::Duration::from_secs(5),
            timeout: std::time::Duration::from_secs(600),
        },
        || async {
            match aws
                .cloudformation
                .describe_stacks()
                .stack_name(&stack)
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
            "stack '{stack}' did not finish deleting (status {status}) — check the CloudFormation console; resources may still bill"
        );
    }

    // Local state last, so a failed AWS teardown stays retryable.
    for file in [Config::path()?, State::path()?] {
        if file.exists() {
            std::fs::remove_file(&file).with_context(|| format!("removing {}", file.display()))?;
        }
    }
    println!(
        "destroyed: microvm, image, bucket contents, stack '{stack}', and local config/state \
         (the ~/.hellbox directory itself is left for any cached binary)"
    );
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
