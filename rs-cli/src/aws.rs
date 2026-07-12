//! AWS client wiring.

use anyhow::Result;
use aws_config::BehaviorVersion;
use aws_sdk_lambdamicrovms::Client as MicrovmClient;

use crate::config::Config;

pub struct Aws {
    pub microvm: MicrovmClient,
    pub s3: aws_sdk_s3::Client,
    pub cloudformation: aws_sdk_cloudformation::Client,
}

/// SDK config for a region, independent of ~/.hellbox/config.toml — `hellbox
/// deploy` needs AWS clients before that file exists.
///
/// Adaptive retry = jittered exponential backoff PLUS a client-side rate
/// limiter that throttles this process when AWS signals throttling, so the
/// lifecycle polling stays a polite API citizen even in tight loops.
pub async fn sdk_config(region: &str) -> aws_config::SdkConfig {
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
        .load()
        .await
}

/// The caller's resolved AWS identity, from STS.
pub struct Identity {
    pub account: String,
    pub arn: String,
}

/// Check that credentials actually work before doing anything, and learn who
/// we are. Turns the SDK's raw error chain into something a person can act on.
/// The default provider chain covers env vars, ~/.aws profiles (AWS_PROFILE),
/// IAM Identity Center / SSO token caches, and credential_process helpers like
/// Granted, so whatever tool wrote the credentials, this reads them.
pub async fn preflight_identity(sdk: &aws_config::SdkConfig) -> Result<Identity> {
    let sts = aws_sdk_sts::Client::new(sdk);
    match sts.get_caller_identity().send().await {
        Ok(out) => Ok(Identity {
            account: out.account().unwrap_or_default().to_string(),
            arn: out.arn().unwrap_or_default().to_string(),
        }),
        Err(e) => {
            tracing::debug!(target: "hellbox::aws", "sts get-caller-identity failed: {e:?}");
            // A profile carrying a `login_session` key (written by Granted / Common Fate,
            // or a newer AWS CLI login) trips the SDK's ProfileFile provider: this build
            // has `credentials-login` off, so the provider fails to build on that key
            // rather than falling through to the profile's `credential_process`. Enabling
            // the feature doesn't help either — it then resolves through a native
            // login-session cache that Granted never populates. The AWS CLI tolerates the
            // key, so users hit a case where `aws` works but hellbox doesn't; the generic
            // "check your creds" text would send them in circles. Detect it and point at
            // the workarounds that actually resolve it.
            if format!("{e:?}").contains("credentials-login") {
                anyhow::bail!(
                    "AWS credentials could not be resolved from your profile.\n\
                     The profile carries a `login_session` key (Granted / Common Fate, or a \
                     newer AWS CLI login). The AWS CLI tolerates it, but hellbox's AWS SDK \
                     resolves it through a native login-session cache that Granted does not \
                     populate, so it fails here even though `aws sts get-caller-identity` \
                     works with the same profile.\n\
                     Two ways forward:\n  \
                     - Export env-var credentials (works now, any shell):\n      \
                       run `aws configure export-credentials --profile <name> --format env`, \
                       set those variables, and clear AWS_PROFILE before running hellbox.\n  \
                     - Or remove the `login_session` line from the profile in ~/.aws/config \
                       so hellbox uses its `credential_process` (e.g. `granted \
                       credential-process`), which this build resolves fine.\n\
                     Details: docs/cli.md (Troubleshooting)."
                );
            }
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
    }
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
    /// Build the AWS clients, first checking that credentials actually work so
    /// every command gets the friendly "no working AWS credentials" message
    /// instead of a raw SDK provider-chain dump. (`play`/`deploy`/`destroy`
    /// preflight separately because they also need the returned `Identity`.)
    pub async fn new(cfg: &Config) -> Result<Self> {
        let sdk = sdk_config(&cfg.region).await;
        preflight_identity(&sdk).await?;
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
