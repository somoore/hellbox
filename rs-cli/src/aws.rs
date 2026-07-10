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

impl Aws {
    pub async fn new(cfg: &Config) -> Result<Self> {
        Ok(Self::from_sdk_config(&sdk_config(&cfg.region).await))
    }

    pub fn from_sdk_config(sdk_config: &aws_config::SdkConfig) -> Self {
        Self {
            microvm: MicrovmClient::new(sdk_config),
            s3: aws_sdk_s3::Client::new(sdk_config),
            cloudformation: aws_sdk_cloudformation::Client::new(sdk_config),
        }
    }
}
