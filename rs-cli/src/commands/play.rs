//! The "I just want to play DOOM" command — also what bare `hellbox` runs.
//!
//! Reconciles local state with AWS and does whatever is needed to get a tab
//! open: RUNNING opens straight away, SUSPENDED opens the paused page (the
//! Resume click is the user's deliberate restart of billing), a terminated or
//! missing MicroVM is relaunched from the image (a MicroVM lives at most 8h
//! total before AWS terminates it, so a returning user usually lands here), and
//! no image at all points to `hellbox deploy`.

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
            // The image ARN may still be recorded locally while the image itself
            // has no launchable active version (its version aged out or was
            // deleted alongside the MicroVM). Launching that would die with a raw
            // `No active version found` error, so check first and offer a rebuild
            // instead of dumping a stack trace.
            if !super::deploy::existing_image_active(&sdk, name).await {
                // A hollow image (no launchable version), or drift deeper than
                // that (missing bucket/stack). `deploy` reconciles every layer
                // and ends by launching and opening the tab, so we return here
                // rather than fall through to our own open below. Only from a
                // plain `play`: `deploy` guarantees an active image before it
                // calls play(strict), so this branch can't recurse into deploy.
                if strict {
                    bail!(
                        "the '{name}' image has no launchable version after deploy built it; \
                         this is unexpected, try `hellbox rm --name {name}` then `hellbox deploy`"
                    );
                }
                return offer_rebuild(name).await;
            }
            if capsule.microvm_id.is_some() {
                println!(
                    "==> '{name}' is not running (a MicroVM lives at most 8h total before AWS terminates it), relaunching"
                );
            } else {
                println!("==> '{name}' has no running machine, launching");
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

/// The recorded image has no launchable active version (hollow), so a plain
/// relaunch would die with a raw `No active version found`. Offer a rebuild via
/// `deploy`, which reconciles the whole stack (not just the image, so it also
/// heals a missing bucket or stack) and then launches and opens the tab. A
/// rebuild is a ~5-minute, resource-creating action, so we never do it silently:
/// a non-interactive shell falls through to the command hint instead.
async fn offer_rebuild(name: &str) -> Result<()> {
    println!("==> The '{name}' image has no launchable version (it needs rebuilding).");
    let answer = crate::discover::ask("Rebuild it now? This takes about 5 minutes. [y/N]:", 'n');
    if answer != 'y' {
        bail!("run `hellbox deploy` to rebuild the '{name}' image, then try again");
    }
    // Box the call: deploy -> play -> offer_rebuild -> deploy is a type-level
    // cycle. It cannot loop at runtime (deploy guarantees an active image before
    // calling play(strict), and offer_rebuild is unreachable under strict), but
    // the async fn still needs boxing to have a finite size.
    Box::pin(super::deploy::run(name, None, &[])).await
}
