//! Delete a capsule's MicroVM image and local state.

use std::time::Duration;

use anyhow::{Context, Result};

use crate::aws::Aws;
use crate::config::Config;
use crate::poll::{PollOpts, poll_until};
use crate::state::State;

pub async fn run(name: &str) -> Result<()> {
    let cfg = Config::load()?;
    let mut state = State::load()?;
    // Do NOT hard-require local state: a failed build (or a fresh machine, e.g.
    // a Windows install that never saw the earlier attempt) can leave an image
    // in AWS that local state never recorded. `rm` must still be able to clear
    // it, or the account wedges: `deploy` says "image exists, rm first" and
    // `rm` says "not in state". The image ARN is deterministic, so reconstruct
    // it when state is missing.
    let cap = state.get(name).cloned().unwrap_or_default();
    let (sdk, identity) =
        crate::aws::resolve_with_profile(&cfg.region, cfg.aws_profile.as_deref()).await?;
    let aws = Aws::from_sdk_config(&sdk);
    let image_arn = cap
        .image_arn
        .clone()
        .unwrap_or_else(|| crate::discover::image_arn(&cfg.region, &identity.account, name));

    // DeleteMicrovmImage fails while a MicroVM is live.
    if let Some(microvm_id) = cap.microvm_id.clone() {
        let _ = aws
            .microvm
            .terminate_microvm()
            .microvm_identifier(&microvm_id)
            .send()
            .await;
        tracing::info!(target: "hellbox::rm", "terminating {microvm_id}");

        // Wait out async termination; a get error means it is already gone.
        let _ = poll_until(
            &format!("microvm {name}"),
            &["TERMINATED"],
            PollOpts {
                interval: Duration::from_secs(3),
                timeout: Duration::from_secs(180),
            },
            || async {
                match aws
                    .microvm
                    .get_microvm()
                    .microvm_identifier(&microvm_id)
                    .send()
                    .await
                {
                    Ok(o) => Ok(o.state().as_str().to_string()),
                    Err(_) => Ok("TERMINATED".to_string()),
                }
            },
        )
        .await;
    }

    // DeleteMicrovmImage also fails on a MicroVM the local state never recorded
    // (a stale deploy, a MicroVM the platform re-created, or state that drifted).
    // Terminating only `cap.microvm_id` above leaves those orphans attached and
    // the delete loops until timeout. Enumerate every MicroVM built from this
    // image and terminate the live ones first.
    terminate_image_microvms(&aws, name, &image_arn).await?;
    // delete_image_with_retry treats a not-found image as success (idempotent),
    // so a fresh machine clearing an already-gone orphan still succeeds.
    delete_image_with_retry(&aws, &image_arn).await?;

    state.remove(name)?;
    println!("rm '{name}': image deleted, capsule removed from state");
    Ok(())
}

/// Terminate every non-terminated MicroVM built from `image_arn` and wait it
/// out, so a delete of that image isn't blocked by a MicroVM local state forgot.
async fn terminate_image_microvms(aws: &Aws, name: &str, image_arn: &str) -> Result<()> {
    let live = aws
        .microvm
        .list_microvms()
        .send()
        .await
        .context("list_microvms")?;
    let ids: Vec<String> = live
        .items()
        .iter()
        .filter(|m| m.image_arn() == image_arn && m.state().as_str() != "TERMINATED")
        .map(|m| m.microvm_id().to_string())
        .collect();

    for id in &ids {
        tracing::info!(target: "hellbox::rm", "terminating microvm {id} (built from the image being removed)");
        let _ = aws
            .microvm
            .terminate_microvm()
            .microvm_identifier(id)
            .send()
            .await;
    }
    for id in &ids {
        // A get error means it is already gone; treat that as TERMINATED.
        let _ = poll_until(
            &format!("microvm {name}"),
            &["TERMINATED"],
            PollOpts {
                interval: Duration::from_secs(3),
                timeout: Duration::from_secs(180),
            },
            || async {
                match aws
                    .microvm
                    .get_microvm()
                    .microvm_identifier(id)
                    .send()
                    .await
                {
                    Ok(o) => Ok(o.state().as_str().to_string()),
                    Err(_) => Ok("TERMINATED".to_string()),
                }
            },
        )
        .await;
    }
    Ok(())
}

async fn delete_image_with_retry(aws: &Aws, image_arn: &str) -> Result<()> {
    use aws_sdk_lambdamicrovms::error::ProvideErrorMetadata;

    let deadline = Duration::from_secs(180);
    let interval = Duration::from_secs(3);
    let start = std::time::Instant::now();
    loop {
        match aws
            .microvm
            .delete_microvm_image()
            .image_identifier(image_arn)
            .send()
            .await
        {
            Ok(_) => return Ok(()),
            Err(e) => {
                // Not found = already gone. Deleting a nonexistent image is the
                // desired end state (idempotent), so report success. This is the
                // fresh-machine case: rm targets a deterministic ARN that may no
                // longer exist.
                if e.code() == Some("ResourceNotFoundException") {
                    tracing::info!(target: "hellbox::rm", "image {image_arn} already gone");
                    return Ok(());
                }
                let transient = e
                    .message()
                    .map(|m| m.contains("running MicroVM"))
                    .unwrap_or(false);
                if transient && start.elapsed() < deadline {
                    tracing::info!(
                        target: "hellbox::rm",
                        "image still has a terminating microvm; retrying delete in {interval:?}"
                    );
                    tokio::time::sleep(interval).await;
                    continue;
                }
                return Err(e).context("delete_microvm_image");
            }
        }
    }
}
