//! Open a capsule through the loopback auth proxy.

use anyhow::{Context, Result};
use aws_sdk_lambdamicrovms::types::PortSpecification;

use crate::aws::Aws;
#[cfg(feature = "proxy")]
use crate::browser;
use crate::config::Config;
#[cfg(any(feature = "proxy", test))]
use crate::lifecycle::host_of;
use crate::state::State;

const AUTH_HEADER: &str = "X-aws-proxy-auth";
const TOKEN_TTL_MINUTES: i32 = 30;

pub async fn run(name: &str, no_open: bool) -> Result<()> {
    run_with_verify(name, no_open, false).await
}

/// `strict` (used by `hellbox deploy`) turns a failed end-to-end stream
/// verification into an error instead of a warning.
pub async fn run_with_verify(name: &str, no_open: bool, strict: bool) -> Result<()> {
    let cfg = Config::load()?;
    let state = State::load()?;
    let capsule = state.require(name)?;

    let microvm_id = capsule.microvm_id.clone().with_context(|| {
        format!("capsule '{name}' isn't running — `hellbox up --name {name}` first")
    })?;
    let endpoint = capsule
        .endpoint
        .clone()
        .with_context(|| format!("capsule '{name}' has no endpoint yet — is it RUNNING?"))?;

    let port = cfg.port;
    let audio_port = cfg.audio_port;
    let video_port = cfg.video_port;
    let input_port = cfg.input_port;
    let h264 = cfg.display.as_deref() == Some("h264");
    let aws = Aws::new(&cfg).await?;

    let mut tok_req = aws
        .microvm
        .create_microvm_auth_token()
        .microvm_identifier(&microvm_id)
        .expiration_in_minutes(TOKEN_TTL_MINUTES);
    // SECURITY: the token's allowedPorts scoping is the load-bearing control that
    // keeps the in-VM services (which self-authenticate nothing) off the public
    // internet. Scope it to the display+stream ports ONLY. Never add the internal
    // readiness hook (9000) or the raw VNC port (5901) here.
    for p in [port, audio_port, video_port, input_port] {
        tok_req = tok_req.allowed_ports(PortSpecification::Port(p));
    }
    // Suspended capsules cannot mint; the local Resume UI can still start.
    let jwe = match tok_req.send().await {
        Ok(out) => out
            .auth_token()
            .get(AUTH_HEADER)
            .cloned()
            .unwrap_or_default(),
        Err(e) => {
            // Expected when the capsule is suspended (can't mint against a frozen
            // machine). Not a warning: we serve the Resume page and thaw on click.
            println!("==> '{name}' is suspended — opening the Resume page");
            tracing::debug!(target: "hellbox::open", "auth token mint failed (capsule suspended): {e:#}");
            String::new()
        }
    };

    let idle_minutes = cfg.idle_suspend_minutes.unwrap_or(0);
    open_fork_b(OpenForkBArgs {
        endpoint: &endpoint,
        port,
        audio_port,
        video_port,
        input_port,
        h264,
        jwe: &jwe,
        no_open,
        aws: &aws,
        microvm_id: &microvm_id,
        name,
        idle_minutes,
        strict,
    })
    .await
}

struct OpenForkBArgs<'a> {
    endpoint: &'a str,
    port: i32,
    audio_port: i32,
    video_port: i32,
    input_port: i32,
    h264: bool,
    jwe: &'a str,
    no_open: bool,
    aws: &'a Aws,
    microvm_id: &'a str,
    name: &'a str,
    idle_minutes: u64,
    strict: bool,
}

#[cfg(feature = "proxy")]
async fn open_fork_b(args: OpenForkBArgs<'_>) -> Result<()> {
    use std::sync::Arc;

    use crate::proxy::{self, ProxyActivity, ProxyConfig, ProxyControl, Upstream};

    let OpenForkBArgs {
        endpoint,
        port,
        audio_port,
        video_port,
        input_port,
        h264,
        jwe,
        no_open,
        aws,
        microvm_id,
        name,
        idle_minutes,
        strict,
    } = args;

    let activity = Arc::new(ProxyActivity::default());
    let upstream = Upstream::new(host_of(endpoint), jwe.to_string());
    let control_secret = generate_control_secret()?;
    let control_secret_copy = control_secret.clone();
    let control = Arc::new(ProxyControl {
        microvm: aws.microvm.clone(),
        microvm_id: microvm_id.to_string(),
        name: name.to_string(),
        token_ports: vec![port, audio_port, video_port, input_port],
        upstream: upstream.clone(),
        control_secret,
    });
    let base = ProxyConfig {
        upstream,
        upstream_port: port,
        local_port: 6080,
        routes: vec![
            ("/hellbox/audio".to_string(), audio_port),
            ("/hellbox/video".to_string(), video_port),
            ("/hellbox/input".to_string(), input_port),
            ("/ldoom/audio".to_string(), audio_port),
            ("/ldoom/video".to_string(), video_port),
            ("/ldoom/input".to_string(), input_port),
        ],
        activity: Some(activity.clone()),
        control: Some(control),
    };
    let base_url = match proxy::start(base.clone()).await {
        Ok(u) => u,
        Err(_) => {
            proxy::start(ProxyConfig {
                local_port: 0,
                ..base
            })
            .await?
        }
    };
    // The opened URL carries the per-session secret as a one-time entry token.
    // The first top-level navigation presents it, the proxy validates it and
    // sets the HttpOnly cookie, and every request after rides the cookie. A
    // different local user can open 127.0.0.1:PORT but cannot know this token
    // (128-bit CSPRNG), so their forged-loopback-Host requests are rejected:
    // this closes cross-user data-plane injection on a shared host. (A same-uid
    // process can read the token from argv/config, but it already owns your
    // shell and credentials, so the proxy was never its boundary.)
    let sep = if h264 { "&" } else { "?" };
    let display = if h264 { "?display=h264" } else { "" };
    let url = format!("{base_url}/{display}{sep}hbk={control_secret_copy}");

    tracing::debug!(target: "hellbox::open", "proxy serving loopback stream (entry token + cookie gated; JWE <redacted>)");

    // Prove the whole chain before handing the user a URL: proxy answering on
    // loopback, and each stream channel handshaking through it into the VM.
    println!("==> Verifying the stream end to end");
    let failures = verify_end_to_end(&base_url, &control_secret_copy).await;
    if failures.is_empty() {
        println!("verified: page ✓  video ✓  audio ✓  input ✓");
    } else {
        let what = failures.join(", ");
        if strict {
            anyhow::bail!(
                "end-to-end verification failed ({what}) — the capsule is up but those \
                 channels are not answering; try `hellbox suspend` + `hellbox resume`, \
                 or rebuild with `hellbox rm` + `hellbox deploy`"
            );
        }
        tracing::warn!(target: "hellbox::open", "verification failed for: {what} (continuing)");
        println!("warning: {what} not answering yet — the page may still recover once loaded");
    }

    if no_open {
        println!("==> proxy ready at {url}  (--no-open: browser not launched)");
    } else {
        browser::open(&url)?;
        println!("==> DOOM is open at {url}");
    }

    if idle_minutes > 0 {
        println!(
            "\nPlaying '{name}'. Ctrl-C to stop hellbox (auto-suspends after {idle_minutes} idle min)."
        );
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = idle_monitor(activity, idle_minutes) => {
                suspend_idle(aws, microvm_id, name).await?;
            }
        }
    } else {
        println!(
            "\nPlaying '{name}'. Ctrl-C to stop hellbox (the MicroVM auto-suspends after ~5 idle min and stops billing)."
        );
        tokio::signal::ctrl_c().await.ok();
    }
    println!("\nStopped. Run `hellbox` again anytime to jump back in.");
    Ok(())
}

#[cfg(feature = "proxy")]
async fn idle_monitor(activity: std::sync::Arc<crate::proxy::ProxyActivity>, idle_minutes: u64) {
    use std::time::Duration;
    let idle_secs = idle_minutes * 60;
    let tick = 15u64;
    let mut idle_for = 0u64;
    let mut ever_connected = false;
    loop {
        tokio::time::sleep(Duration::from_secs(tick)).await;
        if activity.active() > 0 {
            ever_connected = true;
            idle_for = 0;
        } else if ever_connected {
            idle_for += tick;
            if idle_for >= idle_secs {
                return;
            }
        }
    }
}

#[cfg(feature = "proxy")]
async fn suspend_idle(aws: &Aws, microvm_id: &str, name: &str) -> Result<()> {
    use anyhow::Context;
    tracing::info!(target: "hellbox::open", "idle timeout reached — auto-suspending {microvm_id}");
    aws.microvm
        .suspend_microvm()
        .microvm_identifier(microvm_id)
        .send()
        .await
        .context("auto-suspend (idle)")?;
    if let Ok(mut st) = State::load() {
        let _ = st.upsert(name, |c| c.state = Some("SUSPENDING".to_string()));
    }
    println!(
        "auto-suspended '{name}' after idle timeout — cost saved. `hellbox resume --name {name}` to thaw."
    );
    Ok(())
}

/// Prove the proxy answers and every stream channel handshakes through it
/// into the VM. Retries with capped exponential backoff while the capsule
/// settles after boot/resume. Returns the channels that never answered.
#[cfg(feature = "proxy")]
async fn verify_end_to_end(base_url: &str, control_secret: &str) -> Vec<String> {
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let mut failures = Vec::new();

    // The page check must carry the entry token, same as the browser's first
    // navigation, now that the data plane rejects tokenless/cookieless requests.
    let page_url = format!("{base_url}/?hbk={control_secret}");
    let mut page_ok = false;
    for attempt in 0..5u32 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(1u64 << attempt.min(3))).await;
        }
        if let Ok(resp) = reqwest::get(&page_url).await
            && resp.status().is_success()
        {
            page_ok = true;
            break;
        }
    }
    if !page_ok {
        failures.push("page".to_string());
    }

    let ws_base = base_url.replacen("http", "ws", 1);
    for channel in ["video", "audio", "input"] {
        let mut ok = false;
        for attempt in 0..6u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(1u64 << attempt.min(3))).await;
            }
            let url = format!("{ws_base}/hellbox/{channel}");
            let Ok(mut req) = url.into_client_request() else {
                break;
            };
            if let Ok(v) =
                hyper::header::HeaderValue::from_str(&format!("hellbox_control={control_secret}"))
            {
                req.headers_mut().insert(hyper::header::COOKIE, v);
            }
            match tokio_tungstenite::connect_async(req).await {
                Ok((mut ws, _)) => {
                    let _ = ws.close(None).await;
                    ok = true;
                    break;
                }
                Err(e) => {
                    tracing::debug!(target: "hellbox::open", "verify {channel} attempt {attempt}: {e}");
                }
            }
        }
        if !ok {
            failures.push(channel.to_string());
        }
    }
    failures
}

#[cfg(feature = "proxy")]
fn generate_control_secret() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).context("generating loopback control secret")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(not(feature = "proxy"))]
async fn open_fork_b(args: OpenForkBArgs<'_>) -> Result<()> {
    let _ = (
        args.endpoint,
        args.port,
        args.audio_port,
        args.video_port,
        args.input_port,
        args.h264,
        args.jwe,
        args.no_open,
        args.aws,
        args.microvm_id,
        args.name,
        args.idle_minutes,
        args.strict,
    );
    anyhow::bail!(
        "hellbox open requires the `proxy` feature (Fork B loopback proxy), which is \
         on by default. Rebuild without `--no-default-features`."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_of_normalizes_scheme_and_slash() {
        assert_eq!(
            host_of("https://abc.lambda-microvm.us-east-2.on.aws/"),
            "abc.lambda-microvm.us-east-2.on.aws"
        );
        assert_eq!(host_of("abc.example.com"), "abc.example.com");
        assert_eq!(host_of("wss://h/"), "h");
    }
}
