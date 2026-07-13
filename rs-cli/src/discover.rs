//! Discover and adopt AWS MicroVMs that local state isn't tracking.
//!
//! A live MicroVM that `~/.hellbox/state.json` doesn't know about usually means
//! one of two things: this is a fresh machine (a new laptop, a wiped
//! `~/.hellbox`) reusing the same AWS account, or local state drifted from
//! reality. Either way, silently ignoring it — or, on teardown, silently
//! terminating it — is wrong. Import it into local state and let the caller
//! decide what to do with it.

use std::io::{self, IsTerminal, Write};

use anyhow::Result;

use crate::aws::Aws;
use crate::lifecycle::{host_of, microvm_endpoint};
use crate::state::State;

/// A MicroVM found live in AWS and imported into local state.
pub struct Imported {
    pub name: String,
    pub state: String,
}

/// hellbox's deterministic image ARN for a capsule `name`. Adoption is scoped to
/// this exact ARN so we never adopt some unrelated MicroVM in the account.
fn image_arn(region: &str, account: &str, name: &str) -> String {
    format!("arn:aws:lambda:{region}:{account}:microvm-image:{name}")
}

/// Look for a live (non-terminated) MicroVM built from `name`'s hellbox image
/// that local state isn't already tracking, and import it into `state`. Returns
/// the imported machine, or None when there's nothing new to adopt.
pub async fn adopt_untracked(
    aws: &Aws,
    state: &mut State,
    region: &str,
    account: &str,
    name: &str,
) -> Result<Option<Imported>> {
    let arn = image_arn(region, account, name);
    let live = aws.microvm.list_microvms().send().await?;
    let Some(item) = live
        .items()
        .iter()
        .find(|m| m.image_arn() == arn && m.state().as_str() != "TERMINATED")
    else {
        return Ok(None);
    };
    let id = item.microvm_id().to_string();
    let st = item.state().as_str().to_string();

    // Already tracking this exact machine? Then it isn't untracked.
    if state.get(name).and_then(|c| c.microvm_id.as_deref()) == Some(id.as_str()) {
        return Ok(None);
    }

    let endpoint = microvm_endpoint(&aws.microvm, &id)
        .await
        .ok()
        .map(|e| host_of(&e));
    state.upsert(name, |c| {
        c.image_arn = Some(arn.clone());
        c.microvm_id = Some(id.clone());
        c.state = Some(st.clone());
        c.endpoint = endpoint.clone();
    })?;
    Ok(Some(Imported {
        name: name.to_string(),
        state: st,
    }))
}

/// Ask a single-letter question and return the chosen lowercase char. A
/// non-interactive stdin (piped, cron, CI) returns `default` without blocking,
/// so scripts never hang — and, since callers pass a non-destructive default,
/// never auto-destroy.
pub fn ask(prompt: &str, default: char) -> char {
    if !io::stdin().is_terminal() {
        return default;
    }
    print!("{prompt} ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return default;
    }
    line.trim()
        .chars()
        .next()
        .map(|c| c.to_ascii_lowercase())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_returns_default_when_not_a_terminal() {
        // Under `cargo test` stdin is not a TTY, so this must return the
        // non-destructive default rather than block or read — the guarantee
        // that scripts/cron never hang and never auto-destroy.
        assert_eq!(ask("proceed?", 'k'), 'k');
        assert_eq!(ask("proceed?", 'p'), 'p');
    }

    #[test]
    fn image_arn_is_the_hellbox_deterministic_form() {
        assert_eq!(
            image_arn("us-east-1", "123456789012", "doom"),
            "arn:aws:lambda:us-east-1:123456789012:microvm-image:doom"
        );
    }
}
