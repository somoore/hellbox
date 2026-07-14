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
    run_with_verify(name, false).await
}

/// `strict` makes the final end-to-end stream verification fatal (deploy uses
/// this); plain `hellbox play` treats a failed channel as a warning.
pub async fn run_with_verify(name: &str, strict: bool) -> Result<()> {
    let cfg = Config::load()?;
    let mut state = State::load()?;

    // Friendly credential check + wrong-account guard before touching anything.
    let (sdk, identity) =
        crate::aws::resolve_with_profile(&cfg.region, cfg.aws_profile.as_deref()).await?;
    crate::aws::require_same_account(&cfg, &identity)?;
    let aws = Aws::from_sdk_config(&sdk);

    // A live MicroVM this machine wasn't tracking (a new computer on the same
    // AWS account, or drifted state). Import it, then let the user decide.
    // `deploy` (strict) runs its own adopt prompt, so don't double-ask there.
    if !strict
        && let Some(imp) =
            crate::discover::adopt_untracked(&aws, &mut state, &cfg.region, &identity.account, name)
                .await?
    {
        println!(
            "==> Found a '{name}' machine in AWS ({}) that this system wasn't tracking — \
             likely from another computer on this AWS account. Imported it locally.",
            imp.state
        );
        match crate::discover::ask("Play it now, Terminate it, or Keep and quit? [P/t/k]:", 'p') {
            't' => {
                super::down::run(name).await?;
                println!("terminated '{name}'. Run `hellbox` to launch a fresh machine.");
                return Ok(());
            }
            'k' => {
                println!("kept '{name}' tracked locally. Run `hellbox` to play it.");
                return Ok(());
            }
            _ => {} // play it: fall through
        }
    }

    let capsule = match state.get(name) {
        Some(c) => c.clone(),
        None => bail!("no capsule named '{name}' — run `hellbox deploy` first"),
    };

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
            if capsule.microvm_id.is_some() {
                println!(
                    "==> '{name}' is not running (suspended MicroVMs only persist ~8h) — relaunching"
                );
            } else {
                println!("==> '{name}' has no running machine — launching");
            }
            state.upsert(name, |c| {
                c.microvm_id = None;
                c.endpoint = None;
            })?;
            super::up::run(name).await?;
        }
    }

    super::open::run_with_verify(name, false, strict).await
}
