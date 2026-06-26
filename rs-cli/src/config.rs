//! User config loaded from `~/.hellbox/config.toml`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: i32 = 6901;
pub const DEFAULT_AUDIO_PORT: i32 = 6902;
pub const DEFAULT_VIDEO_PORT: i32 = 6903;
pub const DEFAULT_INPUT_PORT: i32 = 6904;
pub const DEFAULT_REGION: &str = "us-east-1";
const HELLBOX_HOME_ENV: &str = "HELLBOX_HOME";
const LEGACY_HOME_ENV: &str = "LAMBDADOOM_HOME";
const HELLBOX_DIR_NAME: &str = ".hellbox";
const LEGACY_DIR_NAME: &str = ".lambdadoom";

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
        Ok(hellbox_dir()?.join("config.toml"))
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

pub fn hellbox_dir() -> Result<PathBuf> {
    if let Some(dir) = env_path(HELLBOX_HOME_ENV) {
        return Ok(dir);
    }
    if let Some(dir) = env_path(LEGACY_HOME_ENV) {
        return Ok(dir);
    }
    let dirs =
        directories::BaseDirs::new().context("could not resolve home directory for ~/.hellbox")?;
    Ok(default_home_dir(dirs.home_dir()))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .filter(|dir| !dir.trim().is_empty())
        .map(PathBuf::from)
}

fn default_home_dir(home: &std::path::Path) -> PathBuf {
    let current = home.join(HELLBOX_DIR_NAME);
    let legacy = home.join(LEGACY_DIR_NAME);
    if !current.exists() && legacy.exists() {
        legacy
    } else {
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_home(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "hellbox-configtest-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn default_home_dir_uses_new_dir_for_fresh_installs() {
        let home = scratch_home("fresh");

        assert_eq!(default_home_dir(&home), home.join(HELLBOX_DIR_NAME));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn default_home_dir_reads_legacy_dir_for_upgrades() {
        let home = scratch_home("legacy");
        std::fs::create_dir(home.join(LEGACY_DIR_NAME)).unwrap();

        assert_eq!(default_home_dir(&home), home.join(LEGACY_DIR_NAME));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn default_home_dir_prefers_existing_new_dir() {
        let home = scratch_home("both");
        std::fs::create_dir(home.join(HELLBOX_DIR_NAME)).unwrap();
        std::fs::create_dir(home.join(LEGACY_DIR_NAME)).unwrap();

        assert_eq!(default_home_dir(&home), home.join(HELLBOX_DIR_NAME));
        let _ = std::fs::remove_dir_all(home);
    }
}
