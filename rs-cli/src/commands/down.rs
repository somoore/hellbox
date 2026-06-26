//! Terminate a capsule and clear runtime state.

use anyhow::{Context, Result};

use crate::aws::Aws;
use crate::config::Config;
use crate::state::State;

pub async fn run(name: &str) -> Result<()> {
    let cfg = Config::load()?;
    let mut state = State::load()?;
    let microvm_id = state
        .require(name)?
        .microvm_id
        .clone()
        .with_context(|| format!("capsule '{name}' has no running microvm"))?;

    let aws = Aws::new(&cfg).await?;
    aws.microvm
        .terminate_microvm()
        .microvm_identifier(&microvm_id)
        .send()
        .await
        .context("terminate_microvm")?;
    tracing::info!(target: "hellbox::down", "terminated {microvm_id}");

    state.upsert(name, |c| {
        c.microvm_id = None;
        c.endpoint = None;
        c.state = Some("TERMINATED".to_string());
    })?;

    println!("down '{name}': {microvm_id} terminated");
    Ok(())
}
