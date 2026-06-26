//! Browser opener.

use anyhow::{Context, Result};

pub fn open(url: &str) -> Result<()> {
    tracing::info!(target: "hellbox::browser", "opening {url}");
    webbrowser::open(url).with_context(|| format!("failed to open browser at {url}"))?;
    Ok(())
}
