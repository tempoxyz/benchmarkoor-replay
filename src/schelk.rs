use std::{ffi::OsStr, fs, process::Command};

use anyhow::{Context, Result};
use serde::Serialize;

const DROP_CACHES_PATH: &str = "/proc/sys/vm/drop_caches";

#[derive(Clone, Debug, Serialize)]
pub struct DropCachesReport {
    pub requested: bool,
    pub path: String,
    pub succeeded: bool,
    pub error: Option<String>,
}

impl DropCachesReport {
    pub fn not_requested() -> Self {
        Self {
            requested: false,
            path: DROP_CACHES_PATH.to_string(),
            succeeded: false,
            error: None,
        }
    }
}

pub fn run<I, S>(bin: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let status = Command::new(bin)
        .args(args)
        .status()
        .with_context(|| format!("running {bin}"))?;
    if !status.success() {
        anyhow::bail!("{bin} exited with {status}");
    }
    Ok(())
}

pub fn output<I, S>(bin: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(bin)
        .args(args)
        .output()
        .with_context(|| format!("running {bin}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        anyhow::bail!("{bin} exited with {}: {}", output.status, text.trim());
    }
    Ok(text)
}

pub fn status(bin: &str) -> String {
    match output(bin, ["status"]) {
        Ok(out) => out.trim().to_string(),
        Err(err) => format!("unavailable: {err:#}"),
    }
}

pub fn recover(bin: &str, drop_caches_after: bool) -> Result<()> {
    run(bin, ["recover"])?;
    if drop_caches_after {
        drop_caches()?;
    }
    Ok(())
}

pub fn drop_caches() -> Result<DropCachesReport> {
    let report = try_drop_caches();
    if let Some(error) = &report.error {
        anyhow::bail!("dropping Linux page cache failed: {error}");
    }
    Ok(report)
}

pub fn try_drop_caches() -> DropCachesReport {
    match fs::write(DROP_CACHES_PATH, b"3\n").context("dropping Linux page cache") {
        Ok(()) => DropCachesReport {
            requested: true,
            path: DROP_CACHES_PATH.to_string(),
            succeeded: true,
            error: None,
        },
        Err(err) => DropCachesReport {
            requested: true,
            path: DROP_CACHES_PATH.to_string(),
            succeeded: false,
            error: Some(format!("{err:#}")),
        },
    }
}
