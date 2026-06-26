//! Capsule state stored in `~/.lambdadoom/state.json`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::lambdadoom_dir;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Capsule {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub image_arn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub image_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub microvm_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub state: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub capsules: BTreeMap<String, Capsule>,
}

impl State {
    pub fn path() -> Result<PathBuf> {
        Ok(lambdadoom_dir()?.join("state.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).context("serializing state")?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Capsule> {
        self.capsules.get(name)
    }

    pub fn require(&self, name: &str) -> Result<&Capsule> {
        self.get(name).with_context(|| {
            format!("no capsule named '{name}' in state — run `ldoom build --name {name}` first")
        })
    }

    pub fn upsert(&mut self, name: &str, f: impl FnOnce(&mut Capsule)) -> Result<()> {
        let entry = self.capsules.entry(name.to_string()).or_default();
        f(entry);
        self.save()
    }

    pub fn remove(&mut self, name: &str) -> Result<()> {
        self.capsules.remove(name);
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_merges_fields() {
        let mut s = State::default();
        s.capsules.entry("demo".into()).or_default().image_arn = Some("arn:image".into());
        {
            let c = s.capsules.entry("demo".into()).or_default();
            c.microvm_id = Some("mvm-1".into());
        }
        let c = s.get("demo").unwrap();
        assert_eq!(c.image_arn.as_deref(), Some("arn:image"));
        assert_eq!(c.microvm_id.as_deref(), Some("mvm-1"));
    }
}
