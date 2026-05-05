use std::{
    path::Path,
    process::{Command, Stdio},
};

use anyhow::Result;

use crate::{baseline, fixtures, schelk, suite::Suite};

pub fn print_status(
    cache_dir: &Path,
    suite: &Suite,
    schelk_bin: &str,
    datadir: Option<&Path>,
) -> Result<()> {
    println!("suite={}", suite.id);
    println!(
        "suite_identity=network:{} block:{} context:{} fork:{} test_type:{}",
        suite.network, suite.block, suite.context, suite.fork, suite.test_type
    );
    println!("fixture_cache={}", fixtures::cache_state(cache_dir, suite));
    let schelk_status = schelk::status(schelk_bin);
    println!("schelk={}", summarize_schelk_status(&schelk_status));
    println!(
        "baseline_marker={}",
        baseline::marker_status(cache_dir, suite)
    );
    println!("reth={}", reth_status());
    println!("resource_profile={}", resource_profile());
    for hazard in hazards(suite, datadir, &schelk_status) {
        println!("hazard={hazard}");
    }
    Ok(())
}

fn reth_status() -> String {
    match Command::new("pgrep").arg("-af").arg("reth").output() {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            one_line(text.trim())
        }
        _ => "not-running".to_string(),
    }
}

fn resource_profile() -> String {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let drop_caches = Path::new("/proc/sys/vm/drop_caches").exists();
    format!("cpus={cpus} drop_caches={drop_caches}")
}

fn hazards(suite: &Suite, datadir: Option<&Path>, schelk_status: &str) -> Vec<String> {
    let mut hazards = Vec::new();
    if suite.fixture_url.is_empty() {
        hazards.push(
            "selected suite has no known fixture URL; pass fixtures download --url".to_string(),
        );
    }
    if suite.genesis_url.is_empty() {
        hazards.push(
            "selected suite has no known genesis URL; snapshot import needs --genesis".to_string(),
        );
    }
    if let Some(datadir) = datadir {
        if !datadir.exists() {
            hazards.push(format!("datadir missing: {}", datadir.display()));
        }
    }
    if Command::new("sh")
        .arg("-c")
        .arg("test -w /proc/sys/vm/drop_caches")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| !status.success())
        .unwrap_or(true)
    {
        hazards.push("drop_caches is not writable by current user".to_string());
    }
    if schelk_status.contains("inconsistent") || schelk_status.contains("MISSING") {
        hazards.push("schelk status reports inconsistent or missing device state".to_string());
    }
    hazards
}

fn one_line(text: &str) -> String {
    text.lines().next().unwrap_or(text).trim().to_string()
}

fn summarize_schelk_status(text: &str) -> String {
    if text.starts_with("unavailable:") {
        return one_line(text);
    }

    let parts = text
        .lines()
        .map(str::trim)
        .filter(|line| {
            line.starts_with("State file:")
                || line.starts_with("Mount point:")
                || line.starts_with("Mounted:")
                || line.starts_with("dm-era device:")
                || line.starts_with("Current era:")
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        one_line(text)
    } else {
        parts.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_schelk_runtime_state() {
        let status = "\
schelk status
=============

State file: /var/lib/schelk/state.json

Configuration:
  Mount point:    /schelk

Runtime status:
  Mounted: yes (state)
  Mounted: yes (actual)
  dm-era device: MISSING (state inconsistent, possible crash)
  Current era: 1
";
        let summary = summarize_schelk_status(status);
        assert!(summary.contains("Mounted: yes (actual)"));
        assert!(summary.contains("dm-era device: MISSING"));
        assert!(!summary.starts_with("schelk status"));
    }
}
