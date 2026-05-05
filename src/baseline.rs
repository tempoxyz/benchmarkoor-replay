use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    cli::{BaselinePrepareArgs, BaselinePromoteArgs, BaselineVerifyArgs, Cli},
    fixtures,
    replay::EngineClient,
    schelk,
    suite::Suite,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct BaselineMarker {
    pub suite_id: String,
    pub network: String,
    pub block: u64,
    pub fork: String,
    pub test_type: String,
    pub promoted_at: Option<String>,
    pub state: String,
}

pub async fn prepare(
    cache_dir: &Path,
    suite: &Suite,
    cli: &Cli,
    args: BaselinePrepareArgs,
) -> Result<()> {
    if !args.no_mount && !args.dry_run {
        schelk::run(&cli.schelk_bin, ["mount"])?;
    }
    if !args.skip_prerun {
        let index = fixtures::load_index(cache_dir, suite)?;
        let mut client =
            EngineClient::new(&cli.engine_url, cli.jwt_secret.as_deref(), args.dry_run)?;
        for step in &index.pre_run {
            client.replay_file(&step.abs_path).await?;
        }
    }
    if args.dry_run {
        println!("dry-run: skipped schelk mount and baseline marker write");
        return Ok(());
    }
    write_marker(cache_dir, suite, "prepared", None)?;
    Ok(())
}

pub async fn promote_prerun(
    cache_dir: &Path,
    suite: &Suite,
    cli: &Cli,
    args: BaselinePromoteArgs,
) -> Result<()> {
    let index = fixtures::load_index(cache_dir, suite)?;
    let mut client = EngineClient::new(&cli.engine_url, cli.jwt_secret.as_deref(), args.dry_run)?;
    for step in &index.pre_run {
        client.replay_file(&step.abs_path).await?;
    }

    if args.dry_run {
        println!("dry-run: skipped schelk promote and baseline marker write");
        return Ok(());
    }

    if args.kill {
        schelk::run(&cli.schelk_bin, ["promote", "--kill"])?;
    } else {
        schelk::run(&cli.schelk_bin, ["promote"])?;
    }
    write_marker(
        cache_dir,
        suite,
        "promoted",
        Some(chrono::Utc::now().to_rfc3339()),
    )?;
    Ok(())
}

pub async fn verify(
    cache_dir: &Path,
    suite: &Suite,
    cli: &Cli,
    args: BaselineVerifyArgs,
) -> Result<()> {
    let marker = read_marker(cache_dir, suite)?;
    if marker.suite_id != suite.id || marker.block != suite.block || marker.fork != suite.fork {
        anyhow::bail!("baseline marker does not match selected suite");
    }
    if marker.state != "promoted" {
        anyhow::bail!("baseline is not promoted: state={}", marker.state);
    }
    if let Some(datadir) = args.datadir {
        if !datadir.exists() {
            anyhow::bail!("datadir does not exist: {}", datadir.display());
        }
    }
    let output = Command::new(&cli.reth_bin)
        .arg("--version")
        .output()
        .with_context(|| format!("running {}", cli.reth_bin))?;
    if !output.status.success() {
        let text = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{} --version failed: {}", cli.reth_bin, text.trim());
    }
    println!("baseline verified for {}", suite.id);
    Ok(())
}

pub fn marker_path(cache_dir: &Path, suite: &Suite) -> std::path::PathBuf {
    cache_dir
        .join("baselines")
        .join(format!("{}.json", suite.slug()))
}

pub fn marker_status(cache_dir: &Path, suite: &Suite) -> String {
    match read_marker(cache_dir, suite) {
        Ok(marker) => format!("{} promoted_at={:?}", marker.state, marker.promoted_at),
        Err(_) => "missing".to_string(),
    }
}

fn write_marker(
    cache_dir: &Path,
    suite: &Suite,
    state: &str,
    promoted_at: Option<String>,
) -> Result<()> {
    let path = marker_path(cache_dir, suite);
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("baseline marker path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).context("creating baseline marker directory")?;
    let marker = BaselineMarker {
        suite_id: suite.id.clone(),
        network: suite.network.clone(),
        block: suite.block,
        fork: suite.fork.clone(),
        test_type: suite.test_type.clone(),
        promoted_at,
        state: state.to_string(),
    };
    fs::write(&path, serde_json::to_vec_pretty(&marker)?)
        .with_context(|| format!("writing baseline marker {}", path.display()))
}

fn read_marker(cache_dir: &Path, suite: &Suite) -> Result<BaselineMarker> {
    let path = marker_path(cache_dir, suite);
    let data =
        fs::read(&path).with_context(|| format!("reading baseline marker {}", path.display()))?;
    Ok(serde_json::from_slice(&data)?)
}
