//! AWS client wiring.

use anyhow::Result;
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_credential_types::provider::{self, ProvideCredentials, error::CredentialsError, future};
use aws_sdk_lambdamicrovms::Client as MicrovmClient;

use crate::config::Config;

pub struct Aws {
    pub microvm: MicrovmClient,
    pub s3: aws_sdk_s3::Client,
    pub cloudformation: aws_sdk_cloudformation::Client,
}

/// Shared SDK config builder for a region, independent of ~/.hellbox/config.toml
/// (`hellbox deploy` needs AWS clients before that file exists).
///
/// Adaptive retry = jittered exponential backoff PLUS a client-side rate limiter
/// that throttles this process when AWS signals throttling, so the lifecycle
/// polling stays a polite API citizen even in tight loops.
fn base_loader(region: &str) -> aws_config::ConfigLoader {
    use std::time::Duration;
    aws_config::defaults(BehaviorVersion::latest())
        .region(aws_config::Region::new(region.to_string()))
        .retry_config(aws_config::retry::RetryConfig::adaptive().with_max_attempts(5))
        .timeout_config(
            aws_config::timeout::TimeoutConfig::builder()
                .operation_attempt_timeout(Duration::from_secs(30))
                .operation_timeout(Duration::from_secs(120))
                .build(),
        )
}

pub async fn sdk_config(region: &str) -> aws_config::SdkConfig {
    base_loader(region).load().await
}

/// The caller's resolved AWS identity, from STS.
pub struct Identity {
    pub account: String,
    pub arn: String,
}

/// Build an SdkConfig for `region` and confirm the credentials actually work,
/// returning the caller's identity. Every AWS command goes through here.
///
/// The rescue path: a profile carrying a `login_session` key (Granted / Common
/// Fate, and newer AWS CLI logins, write one) makes the SDK's ProfileFile
/// provider fail to build, because that key needs the non-default
/// `credentials-login` feature — and even with the feature it wants a native
/// login-session cache Granted never populates. The AWS CLI sidesteps all this
/// by using the profile's `credential_process`. So do we: on that specific
/// failure, run the profile's own `credential_process` (e.g. `granted
/// credential-process`) and inject the credentials it returns. This makes
/// `assume <profile>` + `hellbox` "just work" like `aws`, with no AWS-CLI
/// shell-out and no dependency on the login-session cache.
pub async fn resolve(region: &str) -> Result<(aws_config::SdkConfig, Identity)> {
    let sdk = sdk_config(region).await;
    let err = match identity_of(&sdk).await {
        Ok(id) => return Ok((sdk, id)),
        Err(e) => e,
    };

    // The only failure we can rescue is the login_session parse error; anything
    // else (expired session, no creds at all) falls through to the plain hint.
    if err.contains("credentials-login") {
        if let Some(command) = active_credential_process() {
            let sdk = base_loader(region)
                .credentials_provider(CredentialProcessProvider { command })
                .load()
                .await;
            if let Ok(id) = identity_of(&sdk).await {
                tracing::info!(
                    target: "hellbox::aws",
                    "resolved credentials via the profile's credential_process \
                     (its login_session key is one this SDK build can't parse directly)"
                );
                return Ok((sdk, id));
            }
            tracing::debug!(target: "hellbox::aws", "credential_process fallback did not yield working credentials");
        }
        anyhow::bail!(
            "AWS credentials could not be resolved from your profile.\n\
             The profile carries a `login_session` key (Granted / Common Fate, or a \
             newer AWS CLI login) that this build's AWS SDK can't parse, and its \
             `credential_process` didn't yield working credentials either.\n\
             Try one of:\n  \
             - refresh the session (`assume <profile>`, or `aws sso login`), then retry\n  \
             - export env-var credentials: `aws configure export-credentials --profile \
             <name> --format env`, set them, and clear AWS_PROFILE\n  \
             - remove the `login_session` line from the profile in ~/.aws/config"
        );
    }

    tracing::debug!(target: "hellbox::aws", "sts get-caller-identity failed: {err}");
    anyhow::bail!(
        "no working AWS credentials found.\n\
         hellbox reads credentials the same way the AWS CLI does: environment \
         variables, ~/.aws profiles (AWS_PROFILE), IAM Identity Center / SSO \
         logins, and credential_process tools like Granted.\n\
         Things to try:\n  \
         - `aws sts get-caller-identity` (does the AWS CLI work right now?)\n  \
         - `aws sso login` if your session expired\n  \
         - `assume <profile>` if you use Granted\n  \
         - `export AWS_PROFILE=<name>` if you have multiple profiles"
    );
}

/// One STS get-caller-identity round-trip. On error, returns the SDK error's
/// full debug string (which carries the `credentials-login` marker `resolve`
/// keys off) rather than a typed error, so callers can just string-match it.
async fn identity_of(sdk: &aws_config::SdkConfig) -> std::result::Result<Identity, String> {
    match aws_sdk_sts::Client::new(sdk)
        .get_caller_identity()
        .send()
        .await
    {
        Ok(out) => Ok(Identity {
            account: out.account().unwrap_or_default().to_string(),
            arn: out.arn().unwrap_or_default().to_string(),
        }),
        Err(e) => Err(format!("{e:?}")),
    }
}

/// A refreshable credentials provider backed by a profile's `credential_process`
/// command. Refreshable — not a one-shot static credential — so the SDK re-runs
/// the command when the temporary credentials expire, matching the AWS CLI/SDK
/// contract. That matters here: the proxy keeps minting auth tokens (and driving
/// suspend/resume) with this client for the life of a `hellbox open` session, so
/// static creds would fail those calls the moment the process credentials lapse.
#[derive(Debug)]
struct CredentialProcessProvider {
    command: String,
}

impl CredentialProcessProvider {
    async fn load(&self) -> provider::Result {
        let command = self.command.clone();
        // run_credential_process spawns a subprocess (blocking); keep it off the
        // async worker so a slow helper can't stall the runtime.
        let stdout = tokio::task::spawn_blocking(move || run_credential_process(&command))
            .await
            .map_err(CredentialsError::provider_error)?
            .ok_or_else(|| {
                CredentialsError::provider_error("credential_process returned no credentials")
            })?;
        parse_process_credentials(&stdout).ok_or_else(|| {
            CredentialsError::provider_error(
                "credential_process output was not valid credentials JSON",
            )
        })
    }
}

impl ProvideCredentials for CredentialProcessProvider {
    fn provide_credentials<'a>(&'a self) -> future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        future::ProvideCredentials::new(self.load())
    }
}

/// The active profile's `credential_process` command, if any. Uses the same
/// profile the SDK selects — `AWS_PROFILE`, else `default` — so we run the exact
/// profile whose parse just failed. (We deliberately do NOT consult
/// `AWS_DEFAULT_PROFILE`: that's an AWS CLI compatibility knob the SDK ignores
/// for profile selection, and honoring it could inject a different account's
/// credentials than the SDK would ever have used.)
fn active_credential_process() -> Option<String> {
    let profile = std::env::var("AWS_PROFILE")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| "default".to_string());
    let cmd = credential_process_command(&profile)?;
    tracing::debug!(target: "hellbox::aws", "using credential_process for profile '{profile}'");
    Some(cmd)
}

/// The AWS config file: `AWS_CONFIG_FILE` if set, else `~/.aws/config`.
fn aws_config_file() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("AWS_CONFIG_FILE")
        && !p.trim().is_empty()
    {
        return std::path::PathBuf::from(p);
    }
    directories::BaseDirs::new()
        .map(|d| d.home_dir().join(".aws").join("config"))
        .unwrap_or_else(|| std::path::PathBuf::from(".aws/config"))
}

/// Read `credential_process` for `profile` from the AWS config file. Sections
/// are `[profile NAME]` there (except `[default]`), matching the AWS CLI.
fn credential_process_command(profile: &str) -> Option<String> {
    let text = std::fs::read_to_string(aws_config_file()).ok()?;
    let target = if profile == "default" {
        "default".to_string()
    } else {
        format!("profile {profile}")
    };
    let mut in_target = false;
    for raw in text.lines() {
        let line = raw.trim();
        // AWS config comments are whole-line only (a leading `#` or `;`). A `#`
        // or `;` inside a value is literal — the CLI keeps it — so we must not
        // split it off, or a credential_process command containing one gets
        // truncated and silently fails.
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_target = name.trim() == target;
        } else if in_target
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "credential_process"
        {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Run a credential_process command line through the platform shell (how the
/// profile author expects it to run) and return its stdout.
fn run_credential_process(cmd: &str) -> Option<String> {
    let output = if cfg!(windows) {
        std::process::Command::new("cmd")
            .arg("/C")
            .arg(cmd)
            .output()
    } else {
        std::process::Command::new("sh").arg("-c").arg(cmd).output()
    }
    .ok()?;
    if !output.status.success() {
        tracing::debug!(
            target: "hellbox::aws",
            "credential_process failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse the AWS credential_process JSON contract into credentials, keeping the
/// `Expiration` so the SDK refreshes (re-runs the process) before they lapse.
fn parse_process_credentials(stdout: &str) -> Option<Credentials> {
    #[derive(serde::Deserialize)]
    struct ProcessCreds {
        #[serde(rename = "AccessKeyId")]
        access_key_id: String,
        #[serde(rename = "SecretAccessKey")]
        secret_access_key: String,
        #[serde(rename = "SessionToken")]
        session_token: Option<String>,
        #[serde(rename = "Expiration")]
        expiration: Option<String>,
    }
    let pc: ProcessCreds = serde_json::from_str(stdout.trim()).ok()?;
    if pc.access_key_id.is_empty() || pc.secret_access_key.is_empty() {
        return None;
    }
    let expires_after = pc.expiration.as_deref().and_then(parse_rfc3339);
    Some(Credentials::new(
        pc.access_key_id,
        pc.secret_access_key,
        pc.session_token,
        expires_after,
        "hellbox-credential-process",
    ))
}

/// Parse an RFC3339 timestamp (the credential_process `Expiration` format) into
/// a `SystemTime`. On any parse failure the field is treated as absent.
fn parse_rfc3339(s: &str) -> Option<std::time::SystemTime> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(std::time::SystemTime::from)
}

/// Guard against acting on the wrong AWS account: config.toml remembers which
/// account `hellbox deploy` wrote it for.
pub fn require_same_account(cfg: &Config, identity: &Identity) -> Result<()> {
    if let Some(deployed) = cfg.aws_account_id.as_deref()
        && !deployed.is_empty()
        && deployed != identity.account
    {
        anyhow::bail!(
            "account mismatch: this Hellbox was deployed to account {deployed}{profile}, \
             but your current credentials are for account {now} ({arn}).\n\
             Switch back to the profile you deployed with, or run `hellbox deploy` to set \
             up in this account too.",
            profile = cfg
                .aws_profile
                .as_deref()
                .map(|p| format!(" (profile '{p}')"))
                .unwrap_or_default(),
            now = identity.account,
            arn = identity.arn,
        );
    }
    Ok(())
}

impl Aws {
    /// Build the AWS clients, first checking that credentials actually work (and
    /// applying the login_session/credential_process rescue in `resolve`) so
    /// every command gets a friendly message instead of a raw SDK provider dump.
    /// (`play`/`deploy`/`destroy` call `resolve` directly because they also need
    /// the returned `Identity`.)
    pub async fn new(cfg: &Config) -> Result<Self> {
        let (sdk, _identity) = resolve(&cfg.region).await?;
        Ok(Self::from_sdk_config(&sdk))
    }

    pub fn from_sdk_config(sdk_config: &aws_config::SdkConfig) -> Self {
        Self {
            microvm: MicrovmClient::new(sdk_config),
            s3: aws_sdk_s3::Client::new(sdk_config),
            cloudformation: aws_sdk_cloudformation::Client::new(sdk_config),
        }
    }
}
