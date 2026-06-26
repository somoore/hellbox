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
    let cap = state.require(name)?.clone();
    let aws = Aws::new(&cfg).await?;

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

    if let Some(image_arn) = cap.image_arn.clone() {
        delete_image_with_retry(&aws, &image_arn).await?;
        tracing::info!(target: "hellbox::rm", "deleted image {image_arn}");
    }

    state.remove(name)?;
    println!("rm '{name}': image deleted, capsule removed from state");
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
