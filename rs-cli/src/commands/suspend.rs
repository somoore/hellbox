//! Suspend a capsule and wait for SUSPENDED.

use anyhow::{Context, Result};

use crate::aws::Aws;
use crate::config::Config;
use crate::lifecycle::poll_microvm_state;
use crate::poll::PollOpts;
use crate::state::State;

pub async fn run(name: &str) -> Result<()> {
    let cfg = Config::load()?;
    let mut state = State::load()?;
    let microvm_id = state
        .require(name)?
        .microvm_id
        .clone()
        .with_context(|| format!("capsule '{name}' isn't running"))?;

    let aws = Aws::new(&cfg).await?;
    aws.microvm
        .suspend_microvm()
        .microvm_identifier(&microvm_id)
        .send()
        .await
        .context("suspend_microvm")?;
    tracing::info!(target: "hellbox::suspend", "suspending {microvm_id}");

    let final_state = poll_microvm_state(
        &aws.microvm,
        &format!("microvm {name}"),
        &microvm_id,
        &["SUSPENDED", "TERMINATED", "FAILED"],
        PollOpts::default(),
    )
    .await?;

    state.upsert(name, |c| c.state = Some(final_state.clone()))?;

    if final_state != "SUSPENDED" {
        anyhow::bail!("'{name}' did not suspend (state {final_state})");
    }
    println!("suspended '{name}'");
    Ok(())
}
