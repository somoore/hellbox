use anyhow::{Context, Result};
use aws_sdk_lambdamicrovms::Client as MicrovmClient;

use crate::poll::{PollOpts, poll_until};

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

pub fn host_of(endpoint: &str) -> String {
    endpoint
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("wss://")
        .trim_end_matches('/')
        .to_string()
}
