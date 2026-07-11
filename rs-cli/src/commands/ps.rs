//! Print the local capsule table.

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::aws::Aws;
use crate::config::Config;
use crate::state::State;

pub async fn run(refresh: bool) -> Result<()> {
    let mut state = State::load()?;

    // Refresh is best-effort: if AWS is unreachable (no creds, offline), fall
    // back to the local cache rather than erroring, so `ps` always prints
    // something. A stale-but-shown table beats a hard failure.
    if refresh && let Err(e) = reconcile(&mut state).await {
        tracing::debug!(target: "hellbox::ps", "state refresh failed, showing cache: {e:#}");
        eprintln!("warning: could not reach AWS to refresh state; showing last-known cache");
    }

    print_table(&state);
    Ok(())
}

/// Reconcile local capsule state against live AWS. A MicroVM the platform
/// suspended (or terminated) while hellbox wasn't running shows up here.
async fn reconcile(state: &mut State) -> Result<()> {
    let cfg = Config::load()?;
    let aws = Aws::new(&cfg).await?;
    let live = aws
        .microvm
        .list_microvms()
        .send()
        .await
        .context("list_microvms")?;
    let by_id: HashMap<&str, &_> = live.items().iter().map(|m| (m.microvm_id(), m)).collect();

    let names: Vec<String> = state.capsules.keys().cloned().collect();
    for name in names {
        let id = state.get(&name).and_then(|c| c.microvm_id.clone());
        if let Some(id) = id {
            match by_id.get(id.as_str()) {
                Some(item) => {
                    let st = item.state().as_str().to_string();
                    state.upsert(&name, |c| {
                        c.state = Some(st.clone());
                    })?;
                }
                None => {
                    state.upsert(&name, |c| {
                        c.microvm_id = None;
                        c.endpoint = None;
                        c.state = Some("TERMINATED".to_string());
                    })?;
                }
            }
        }
    }
    Ok(())
}

fn print_table(state: &State) {
    if state.capsules.is_empty() {
        println!("no capsules yet — `hellbox build --name <name>`");
        return;
    }
    println!("{:<16} {:<12} {:<22} ENDPOINT", "NAME", "STATE", "MICROVM");
    for (name, c) in &state.capsules {
        println!(
            "{:<16} {:<12} {:<22} {}",
            name,
            c.state.as_deref().unwrap_or("-"),
            c.microvm_id.as_deref().unwrap_or("-"),
            c.endpoint.as_deref().unwrap_or("-"),
        );
    }
}
