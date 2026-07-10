//! The "I just want to play DOOM" command — also what bare `hellbox` runs.
//!
//! Reconciles local state with AWS and does whatever is needed to get a tab
//! open: RUNNING opens straight away, SUSPENDED opens the paused page (the
//! Resume click is the user's deliberate restart of billing), a terminated or
//! missing MicroVM is relaunched from the image (suspended machines only
//! persist ~8h, so a returning user usually lands here), and no image at all
//! points to `hellbox deploy`.

use anyhow::{Context, Result, bail};

use crate::aws::Aws;
use crate::config::Config;
use crate::state::State;

pub async fn run(name: &str) -> Result<()> {
    let cfg = Config::load()?;
    let mut state = State::load()?;

    let capsule = match state.get(name) {
        Some(c) => c.clone(),
        None => bail!("no capsule named '{name}' — run `hellbox deploy` first"),
    };

    // Friendly credential check + wrong-account guard before touching anything.
    let sdk = crate::aws::sdk_config(&cfg.region).await;
    let identity = crate::aws::preflight_identity(&sdk).await?;
    crate::aws::require_same_account(&cfg, &identity)?;
    let aws = Aws::from_sdk_config(&sdk);

    // Reconcile with AWS; this is also the credentials check.
    let live_state = match &capsule.microvm_id {
        Some(id) => match aws
            .microvm
            .get_microvm()
            .microvm_identifier(id)
            .send()
            .await
        {
            Ok(out) => Some(out.state().as_str().to_string()),
            Err(e) => {
                let msg = format!("{e:?}");
                if msg.contains("ResourceNotFound") || msg.contains("NotFound") {
                    None // terminated long enough ago that AWS forgot it
                } else {
                    return Err(e).context(
                        "could not reach AWS — are your credentials current? \
                         (aws sso login / assume / aws configure)",
                    );
                }
            }
        },
        None => None,
    };

    match live_state.as_deref() {
        Some("RUNNING") | Some("SUSPENDED") | Some("SUSPENDING") => {
            // open handles both: a suspended capsule gets the paused page with
            // the Resume button (resuming is the user's billing decision).
            println!("==> '{name}' is {}", live_state.as_deref().unwrap_or("?"));
            state.upsert(name, |c| c.state = live_state.clone())?;
        }
        Some("PENDING") => {
            println!("==> '{name}' is starting");
            let id = capsule.microvm_id.clone().unwrap_or_default();
            crate::lifecycle::await_running(&aws.microvm, &mut state, name, &id).await?;
        }
        _ => {
            // TERMINATED, FAILED, or gone entirely: relaunch from the image.
            if capsule.image_arn.is_none() && capsule.image_version.is_none() {
                bail!("capsule '{name}' has no image — run `hellbox deploy`");
            }
            println!(
                "==> '{name}' is not running (suspended MicroVMs only persist ~8h) — relaunching"
            );
            state.upsert(name, |c| {
                c.microvm_id = None;
                c.endpoint = None;
            })?;
            super::up::run(name).await?;
        }
    }

    super::open::run(name, false).await
}
