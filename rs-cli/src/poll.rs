//! Async state polling helper.

use std::collections::HashSet;
use std::future::Future;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug)]
pub struct PollOpts {
    pub interval: Duration,
    pub timeout: Duration,
}

impl Default for PollOpts {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(10),
            timeout: Duration::from_secs(15 * 60),
        }
    }
}

/// Poll until `getter` returns a terminal state.
pub async fn poll_until<F, Fut>(
    label: &str,
    terminal: &[&str],
    opts: PollOpts,
    mut getter: F,
) -> Result<String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<String>>,
{
    let terminal: HashSet<&str> = terminal.iter().copied().collect();
    let start = Instant::now();
    let mut last: Option<String> = None;

    loop {
        let state = getter().await?;
        if last.as_deref() != Some(state.as_str()) {
            tracing::info!(target: "hellbox::poll", "{label}: {state}");
            last = Some(state.clone());
        }
        if terminal.contains(state.as_str()) {
            return Ok(state);
        }
        if start.elapsed() >= opts.timeout {
            bail!(
                "timed out after {:?} waiting for {label} to reach one of {:?} (last: {state})",
                opts.timeout,
                terminal
            );
        }
        tokio::time::sleep(opts.interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[tokio::test]
    async fn stops_at_terminal_state() {
        let n = Cell::new(0u8);
        let opts = PollOpts {
            interval: Duration::from_millis(1),
            timeout: Duration::from_secs(5),
        };
        let got = poll_until("test", &["CREATED", "CREATE_FAILED"], opts, || async {
            let v = n.get();
            n.set(v + 1);
            Ok(if v < 2 {
                "CREATING".into()
            } else {
                "CREATED".into()
            })
        })
        .await
        .unwrap();
        assert_eq!(got, "CREATED");
    }
}
