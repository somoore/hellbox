//! AWS client wiring.

use anyhow::Result;
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_credential_types::provider::SharedCredentialsProvider;
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

/// Same config, but with an explicit static credentials provider. An explicit
/// provider takes precedence over the default chain, so this bypasses the
/// profile-file provider entirely (the one that chokes on `login_session`).
async fn sdk_config_with_creds(region: &str, creds: Credentials) -> aws_config::SdkConfig {
    base_loader(region)
        .credentials_provider(SharedCredentialsProvider::new(creds))
        .load()
        .await
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
        if let Some(creds) = credential_process_credentials() {
            let sdk = sdk_config_with_creds(region, creds).await;
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

/// Locate the active profile's `credential_process` command and run it, parsing
/// the temporary credentials it prints. Returns None when there is no profile
/// credential_process to run or it doesn't produce usable credentials.
fn credential_process_credentials() -> Option<Credentials> {
    let profile = std::env::var("AWS_PROFILE")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .or_else(|| {
            std::env::var("AWS_DEFAULT_PROFILE")
                .ok()
                .filter(|p| !p.trim().is_empty())
        })
        .unwrap_or_else(|| "default".to_string());
    let cmd = credential_process_command(&profile)?;
    tracing::debug!(target: "hellbox::aws", "running credential_process for profile '{profile}'");
    let stdout = run_credential_process(&cmd)?;
    parse_process_credentials(&stdout)
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
    for line in text.lines() {
        let line = line.split(['#', ';']).next().unwrap_or("").trim();
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

/// Parse the AWS credential_process JSON contract into static credentials.
/// Expiry is intentionally dropped: these serve one hellbox run, well within the
/// process credentials' own lifetime, and a static provider can't refresh anyway.
fn parse_process_credentials(stdout: &str) -> Option<Credentials> {
    #[derive(serde::Deserialize)]
    struct ProcessCreds {
        #[serde(rename = "AccessKeyId")]
        access_key_id: String,
        #[serde(rename = "SecretAccessKey")]
        secret_access_key: String,
        #[serde(rename = "SessionToken")]
        session_token: Option<String>,
    }
    let pc: ProcessCreds = serde_json::from_str(stdout.trim()).ok()?;
    if pc.access_key_id.is_empty() || pc.secret_access_key.is_empty() {
        return None;
    }
    Some(Credentials::new(
        pc.access_key_id,
        pc.secret_access_key,
        pc.session_token,
        None,
        "hellbox-credential-process",
    ))
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
