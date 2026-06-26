use anyhow::{Context, Result, bail};
use aws_sdk_lambdamicrovms::Client as MicrovmClient;

use crate::poll::{PollOpts, poll_until};
use crate::state::State;

pub async fn microvm_state(client: &MicrovmClient, id: &str) -> Result<String> {
    let out = client
        .get_microvm()
        .microvm_identifier(id)
        .send()
        .await
        .context("get_microvm")?;
    Ok(out.state().as_str().to_string())
}

pub async fn microvm_endpoint(client: &MicrovmClient, id: &str) -> Result<String> {
    let out = client
        .get_microvm()
        .microvm_identifier(id)
        .send()
        .await
        .context("get_microvm")?;
    Ok(out.endpoint().to_string())
}

pub async fn poll_microvm_state(
    client: &MicrovmClient,
    label: &str,
    id: &str,
    terminal: &[&str],
    opts: PollOpts,
) -> Result<String> {
    poll_until(label, terminal, opts, || async {
        microvm_state(client, id).await
    })
    .await
}

/// Poll a just-launched/resumed microvm to RUNNING, refresh its endpoint + state in
/// local state, and error if it didn't reach RUNNING. Shared by `up` and `resume`.
pub async fn await_running(
    aws_microvm: &MicrovmClient,
    state: &mut State,
    name: &str,
    microvm_id: &str,
) -> Result<()> {
    let final_state = poll_microvm_state(
        aws_microvm,
        &format!("microvm {name}"),
        microvm_id,
        &["RUNNING", "TERMINATED", "FAILED"],
        PollOpts::default(),
    )
    .await?;

    let endpoint = microvm_endpoint(aws_microvm, microvm_id).await.ok();

    state.upsert(name, |c| {
        c.state = Some(final_state.clone());
        if endpoint.is_some() {
            c.endpoint = endpoint.clone();
        }
    })?;

    if final_state != "RUNNING" {
        bail!("microvm '{name}' did not reach RUNNING (state {final_state})");
    }
    Ok(())
}

#[cfg(any(feature = "proxy", test))]
pub fn host_of(endpoint: &str) -> String {
    endpoint
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("wss://")
        .trim_end_matches('/')
        .to_string()
}
