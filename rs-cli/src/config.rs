//! User config loaded from `~/.lambdadoom/config.toml`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: i32 = 6901;
pub const DEFAULT_AUDIO_PORT: i32 = 6902;
pub const DEFAULT_VIDEO_PORT: i32 = 6903;
pub const DEFAULT_INPUT_PORT: i32 = 6904;
pub const DEFAULT_REGION: &str = "us-east-1";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_region")]
    pub region: String,
    pub artifact_bucket: String,
    pub build_role_arn: String,
    #[serde(default)]
    pub execution_role_arn: Option<String>,
    #[serde(default)]
    pub ingress_connector_arn: String,
    #[serde(default)]
    pub egress_connector_arn: String,
    pub base_image_arn: String,
    #[serde(default = "default_port")]
    pub port: i32,
    #[serde(default = "default_audio_port")]
    pub audio_port: i32,
    #[serde(default = "default_video_port")]
    pub video_port: i32,
    #[serde(default = "default_input_port")]
    pub input_port: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_suspend_minutes: Option<u64>,
}

fn default_region() -> String {
    DEFAULT_REGION.to_string()
}
fn default_port() -> i32 {
    DEFAULT_PORT
}
fn default_audio_port() -> i32 {
    DEFAULT_AUDIO_PORT
}
fn default_video_port() -> i32 {
    DEFAULT_VIDEO_PORT
}
fn default_input_port() -> i32 {
    DEFAULT_INPUT_PORT
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        Ok(lambdadoom_dir()?.join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "no config at {} — deploy `deploy/doom.yaml` (the Launch Stack button \
                 or `aws cloudformation deploy`) and copy the stack Outputs there \
                 (region, artifact_bucket, build_role_arn, execution_role_arn). See README.",
                path.display()
            )
        })?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

pub fn lambdadoom_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("LAMBDADOOM_HOME")
        && !dir.trim().is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    let dirs = directories::BaseDirs::new()
        .context("could not resolve home directory for ~/.lambdadoom")?;
    Ok(dirs.home_dir().join(".lambdadoom"))
}
