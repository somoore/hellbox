//! Resume a capsule and wait for RUNNING.

use anyhow::{Context, Result};

use crate::aws::Aws;
use crate::config::Config;
use crate::lifecycle::{microvm_endpoint, poll_microvm_state};
use crate::poll::PollOpts;
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

    let final_state = poll_microvm_state(
        &aws.microvm,
        &format!("microvm {name}"),
        &microvm_id,
        &["RUNNING", "TERMINATED", "FAILED"],
        PollOpts::default(),
    )
    .await?;

    let endpoint = microvm_endpoint(&aws.microvm, &microvm_id).await.ok();

    state.upsert(name, |c| {
        c.state = Some(final_state.clone());
        if endpoint.is_some() {
            c.endpoint = endpoint.clone();
        }
    })?;

    if final_state != "RUNNING" {
        anyhow::bail!("'{name}' did not resume (state {final_state})");
    }
    println!("resumed '{name}' — RUNNING");
    Ok(())
}
