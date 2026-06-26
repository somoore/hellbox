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

    let aws = Aws::new(&cfg).await?;
    aws.microvm
        .resume_microvm()
        .microvm_identifier(&microvm_id)
        .send()
        .await
        .context("resume_microvm")?;
    tracing::info!(target: "ldoom::resume", "resuming {microvm_id}");

    await_running(&aws.microvm, &mut state, name, &microvm_id).await?;

    println!("resumed '{name}' — RUNNING");
    Ok(())
}
