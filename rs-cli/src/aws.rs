//! AWS client wiring.

use anyhow::Result;
use aws_config::BehaviorVersion;
use aws_sdk_lambdamicrovms::Client as MicrovmClient;

use crate::config::Config;

pub struct Aws {
    pub microvm: MicrovmClient,
    pub s3: aws_sdk_s3::Client,
    #[allow(dead_code)]
    pub region: String,
}

impl Aws {
    pub async fn new(cfg: &Config) -> Result<Self> {
        let region = aws_config::Region::new(cfg.region.clone());
        let sdk_config = aws_config::defaults(BehaviorVersion::latest())
            .region(region)
            .load()
            .await;

        let microvm = MicrovmClient::new(&sdk_config);
        let s3 = aws_sdk_s3::Client::new(&sdk_config);

        Ok(Self {
            microvm,
            s3,
            region: cfg.region.clone(),
        })
    }
}
