//! Resume a capsule and wait for RUNNING.

use anyhow::{Context, Result};

use crate::aws::Aws;
use crate::config::Config;
use crate::lifecycle::await_running;
use crate::state::State;

pub async fn run(name: &str) -> Result<()> {
    let cfg = Config::load()?;
    let mut state = State::load()?;
    let microvm_id = state
        .require(name)?
        .microvm_id
        .clone()
        .with_context(|| format!("capsule '{name}' has no microvm to resume"))?;

    let (sdk, identity) =
        crate::aws::resolve_with_profile(&cfg.region, cfg.aws_profile.as_deref()).await?;
    crate::aws::require_same_account(&cfg, &identity)?;
    let aws = Aws::from_sdk_config(&sdk);
    aws.microvm
        .resume_microvm()
        .microvm_identifier(&microvm_id)
        .send()
        .await
        .context("resume_microvm")?;
    tracing::info!(target: "hellbox::resume", "resuming {microvm_id}");

    await_running(&aws.microvm, &mut state, name, &microvm_id).await?;

    println!("resumed '{name}' — RUNNING");
    Ok(())
}
