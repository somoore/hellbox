//! Browser opener.

use anyhow::{Context, Result};

pub fn open(url: &str) -> Result<()> {
    // Never log the URL: it carries the single-use entry token in its query.
    // A scrubbed origin is enough for a breadcrumb.
    let origin = url.split_once("/?").map(|(o, _)| o).unwrap_or(url);
    tracing::info!(target: "hellbox::browser", "opening browser at {origin}");
    webbrowser::open(url).with_context(|| format!("failed to open browser at {origin}"))?;
    Ok(())
}
