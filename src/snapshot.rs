use std::{fs, fs::File, path::Path, process::Command};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

use crate::{
    cli::{GenesisDownloadArgs, SnapshotImportArgs},
    suite::Suite,
};

pub async fn import_snapshot(
    suite: &Suite,
    reth_bin: &str,
    args: SnapshotImportArgs,
) -> Result<()> {
    let url = args
        .url
        .clone()
        .unwrap_or_else(|| suite.snapshot_url.clone());
    if url.is_empty() {
        anyhow::bail!(
            "no snapshot URL is known for suite {}; pass --url",
            suite.id
        );
    }

    let archive = snapshot_archive_path(&args.datadir, suite, &url);
    if let Some(parent) = archive.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating snapshot archive directory {}", parent.display()))?;
    }
    if !archive.exists() && args.offline {
        anyhow::bail!(
            "offline requested but snapshot archive does not exist: {}",
            archive.display()
        );
    }
    if !archive.exists() {
        download_to_file(&url, &archive).await?;
    }

    prepare_datadir(&args.datadir, args.force)?;
    extract_snapshot(&archive, &args.datadir)?;
    normalize_datadir(&args.datadir)?;

    let genesis_path = resolve_genesis_path(&args.datadir, args.genesis);
    ensure_genesis(suite, &genesis_path, false).await?;

    if args.migrate_v2 {
        run_reth(
            reth_bin,
            db_args(&genesis_path, &args.datadir, ["migrate-v2"]),
        )?;
    }

    verify_snapshot(
        reth_bin,
        &genesis_path,
        &args.datadir,
        args.expected_head.unwrap_or(suite.block),
    )?;
    println!("snapshot imported datadir={}", args.datadir.display());
    Ok(())
}

pub async fn download_genesis(suite: &Suite, args: GenesisDownloadArgs) -> Result<()> {
    let genesis_path = resolve_genesis_path(&args.datadir, args.genesis);
    match ensure_genesis(suite, &genesis_path, args.force).await? {
        GenesisStatus::Existing => println!("genesis exists path={}", genesis_path.display()),
        GenesisStatus::Downloaded => println!("genesis downloaded path={}", genesis_path.display()),
    }
    Ok(())
}

fn resolve_genesis_path(datadir: &Path, genesis: Option<std::path::PathBuf>) -> std::path::PathBuf {
    genesis.unwrap_or_else(|| datadir.join("genesis.json"))
}

#[derive(Debug)]
enum GenesisStatus {
    Existing,
    Downloaded,
}

async fn ensure_genesis(suite: &Suite, genesis_path: &Path, force: bool) -> Result<GenesisStatus> {
    if genesis_path.exists() && !force {
        return Ok(GenesisStatus::Existing);
    }
    if suite.genesis_url.is_empty() {
        anyhow::bail!(
            "genesis file missing and suite has no genesis URL: {}",
            genesis_path.display()
        );
    }
    if let Some(parent) = genesis_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating genesis directory {}", parent.display()))?;
    }
    download_to_file(&suite.genesis_url, genesis_path).await?;
    Ok(GenesisStatus::Downloaded)
}

fn prepare_datadir(datadir: &Path, force: bool) -> Result<()> {
    if datadir.exists() && !datadir.is_dir() {
        anyhow::bail!(
            "snapshot datadir path is not a directory: {}",
            datadir.display()
        );
    }
    if datadir.exists() && !is_empty_dir(datadir)? {
        if !force {
            anyhow::bail!(
                "snapshot datadir is not empty: {}; pass --force to remove it before extraction",
                datadir.display()
            );
        }
        ensure_safe_to_clear(datadir)?;
        fs::remove_dir_all(datadir)
            .with_context(|| format!("removing existing datadir {}", datadir.display()))?;
    }
    fs::create_dir_all(datadir).with_context(|| format!("creating datadir {}", datadir.display()))
}

fn ensure_safe_to_clear(path: &Path) -> Result<()> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("canonicalizing {}", path.display()))?;
    if canonical.parent().is_none() || canonical.parent() == Some(Path::new("/")) {
        anyhow::bail!("refusing to remove top-level path {}", canonical.display());
    }
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        if home.exists() && canonical == home.canonicalize()? {
            anyhow::bail!("refusing to remove home directory {}", canonical.display());
        }
    }
    let cwd = std::env::current_dir().context("reading current directory")?;
    if canonical == cwd.canonicalize()? {
        anyhow::bail!(
            "refusing to remove current directory {}",
            canonical.display()
        );
    }
    Ok(())
}

fn is_empty_dir(path: &Path) -> Result<bool> {
    Ok(fs::read_dir(path)
        .with_context(|| format!("reading directory {}", path.display()))?
        .next()
        .is_none())
}

fn snapshot_filename(url: &str) -> String {
    url.split('/')
        .next_back()
        .filter(|s| !s.is_empty())
        .unwrap_or("snapshot.tar.zst")
        .to_string()
}

fn snapshot_archive_path(datadir: &Path, suite: &Suite, url: &str) -> std::path::PathBuf {
    datadir
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{}-{}", suite.slug(), snapshot_filename(url)))
}

async fn download_to_file(url: &str, path: &Path) -> Result<()> {
    let response = reqwest::Client::builder()
        .user_agent("benchmarkoor-replay/0.1")
        .build()?
        .get(url)
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?
        .error_for_status()
        .with_context(|| format!("downloading {url}"))?;
    let tmp = path.with_extension("download");
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .with_context(|| format!("creating {}", tmp.display()))?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk.context("reading download chunk")?)
            .await?;
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&tmp, path).await.with_context(|| {
        format!(
            "moving downloaded file {} to {}",
            tmp.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn extract_snapshot(archive: &Path, datadir: &Path) -> Result<()> {
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.ends_with(".tar.zst") {
        let file = File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
        let decoder = zstd::stream::read::Decoder::new(file)?;
        let mut tar = tar::Archive::new(decoder);
        tar.unpack(datadir).with_context(|| {
            format!("extracting {} to {}", archive.display(), datadir.display())
        })?;
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let file = File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut tar = tar::Archive::new(decoder);
        tar.unpack(datadir).with_context(|| {
            format!("extracting {} to {}", archive.display(), datadir.display())
        })?;
    } else {
        anyhow::bail!("unsupported snapshot archive format: {}", archive.display());
    }
    Ok(())
}

fn normalize_datadir(datadir: &Path) -> Result<()> {
    if looks_like_reth_datadir(datadir) {
        return Ok(());
    }

    let entries = fs::read_dir(datadir)?.collect::<std::io::Result<Vec<_>>>()?;
    if entries.len() == 1 && entries[0].file_type()?.is_dir() {
        let nested = entries[0].path();
        if looks_like_reth_datadir(&nested) {
            for child in fs::read_dir(&nested)? {
                let child = child?;
                fs::rename(child.path(), datadir.join(child.file_name()))?;
            }
            fs::remove_dir_all(nested)?;
        }
    }
    Ok(())
}

fn looks_like_reth_datadir(path: &Path) -> bool {
    path.join("db").exists()
        || path.join("mdbx.dat").exists()
        || path.join("static_files").exists()
        || path.join("reth.toml").exists()
}

fn verify_snapshot(
    reth_bin: &str,
    genesis: &Path,
    datadir: &Path,
    expected_head: u64,
) -> Result<()> {
    if !genesis.exists() {
        anyhow::bail!("genesis file missing after import: {}", genesis.display());
    }
    if !datadir.exists() {
        anyhow::bail!("datadir missing after import: {}", datadir.display());
    }
    if !looks_like_reth_datadir(datadir) {
        anyhow::bail!(
            "extracted snapshot does not look like a Reth datadir: {}",
            datadir.display()
        );
    }
    let genesis_json: serde_json::Value = serde_json::from_slice(
        &fs::read(genesis).with_context(|| format!("reading genesis {}", genesis.display()))?,
    )
    .with_context(|| format!("parsing genesis {}", genesis.display()))?;
    let chain_id = genesis_json
        .pointer("/config/chainId")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| anyhow::anyhow!("genesis missing config.chainId: {}", genesis.display()))?;

    run_reth_capture(reth_bin, db_args(genesis, datadir, ["stats"]))?;

    let finish = run_reth_capture(
        reth_bin,
        db_args(
            genesis,
            datadir,
            ["stage-checkpoints", "get", "--stage", "finish"],
        ),
    )?;
    let finish_head = parse_stage_checkpoint_block(&finish).ok_or_else(|| {
        anyhow::anyhow!(
            "could not parse Finish stage checkpoint from: {}",
            finish.trim()
        )
    })?;
    if finish_head != expected_head {
        anyhow::bail!(
            "snapshot head mismatch: Finish checkpoint is {finish_head}, expected {expected_head}"
        );
    }

    verify_header(reth_bin, genesis, datadir, 0)?;
    verify_header(reth_bin, genesis, datadir, expected_head)?;

    println!("snapshot verified chain_id={chain_id} head={expected_head}");
    Ok(())
}

fn db_args<'a, I>(genesis: &'a Path, datadir: &'a Path, tail: I) -> Vec<std::ffi::OsString>
where
    I: IntoIterator,
    I::Item: AsRef<std::ffi::OsStr>,
{
    let mut args = vec![
        "db".into(),
        "--chain".into(),
        genesis.as_os_str().to_owned(),
        "--datadir".into(),
        datadir.as_os_str().to_owned(),
    ];
    args.extend(tail.into_iter().map(|arg| arg.as_ref().to_owned()));
    args
}

fn verify_header(reth_bin: &str, genesis: &Path, datadir: &Path, block: u64) -> Result<()> {
    let static_output = run_reth_capture(
        reth_bin,
        db_args(
            genesis,
            datadir,
            ["get", "static-file", "headers", &block.to_string()],
        ),
    );
    if static_output
        .as_deref()
        .map(has_header_content)
        .unwrap_or(false)
    {
        return Ok(());
    }

    let mdbx_output = run_reth_capture(
        reth_bin,
        db_args(
            genesis,
            datadir,
            ["get", "mdbx", "Headers", &block.to_string()],
        ),
    );
    if mdbx_output
        .as_deref()
        .map(has_header_content)
        .unwrap_or(false)
    {
        return Ok(());
    }

    anyhow::bail!(
        "header for block {block} was not found in static files or MDBX; static={}; mdbx={}",
        command_result_summary(static_output),
        command_result_summary(mdbx_output)
    );
}

fn has_header_content(output: &str) -> bool {
    !output.contains("No content for the given table key")
        && (output.contains("BlockHash") || output.contains("parent_hash"))
}

fn command_result_summary(result: Result<String>) -> String {
    match result {
        Ok(output) => output.lines().next().unwrap_or("").trim().to_string(),
        Err(err) => err.to_string(),
    }
}

fn parse_stage_checkpoint_block(output: &str) -> Option<u64> {
    let (_, after) = output.split_once("block_number:")?;
    let number = after
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();
    number.parse().ok()
}

fn run_reth_capture<I, S>(reth_bin: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new(reth_bin)
        .args(args)
        .output()
        .with_context(|| format!("running {reth_bin}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        anyhow::bail!("{reth_bin} exited with {}: {}", output.status, text.trim());
    }
    Ok(text)
}

fn run_reth<I, S>(reth_bin: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let status = Command::new(reth_bin)
        .args(args)
        .status()
        .with_context(|| format!("running {reth_bin}"))?;
    if !status.success() {
        anyhow::bail!("{reth_bin} exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_snapshot_filename() {
        assert_eq!(
            snapshot_filename(
                "https://snapshots.ethpandaops.io/jochemnet/reth/24402727/snapshot.tar.zst"
            ),
            "snapshot.tar.zst"
        );
    }

    #[test]
    fn namespaces_snapshot_archive_by_suite() {
        let suite = Suite::resolve(
            "jochemnet/24402727",
            "repricing",
            "amsterdam",
            "stateful",
            Path::new("/missing"),
        )
        .unwrap();
        let path = snapshot_archive_path(
            Path::new("/tmp/reth"),
            &suite,
            "https://snapshots.ethpandaops.io/jochemnet/reth/24402727/snapshot.tar.zst",
        );
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("jochemnet-24402727-repricing-amsterdam-stateful-snapshot.tar.zst")
        );
    }

    #[test]
    fn defaults_genesis_path_to_datadir() {
        assert_eq!(
            resolve_genesis_path(Path::new("/tmp/reth"), None),
            Path::new("/tmp/reth/genesis.json")
        );
    }

    #[test]
    fn allows_explicit_genesis_path() {
        assert_eq!(
            resolve_genesis_path(
                Path::new("/tmp/reth"),
                Some(std::path::PathBuf::from("/tmp/custom-genesis.json")),
            ),
            Path::new("/tmp/custom-genesis.json")
        );
    }

    #[tokio::test]
    async fn existing_genesis_does_not_need_suite_url() {
        let tmp = tempfile::tempdir().unwrap();
        let genesis = tmp.path().join("genesis.json");
        fs::write(&genesis, b"{}").unwrap();
        let suite = Suite::resolve(
            "custom/1",
            "repricing",
            "amsterdam",
            "stateful",
            Path::new("/missing"),
        )
        .unwrap();
        ensure_genesis(&suite, &genesis, false).await.unwrap();
    }

    #[tokio::test]
    async fn missing_genesis_requires_suite_url() {
        let tmp = tempfile::tempdir().unwrap();
        let genesis = tmp.path().join("genesis.json");
        let suite = Suite::resolve(
            "custom/1",
            "repricing",
            "amsterdam",
            "stateful",
            Path::new("/missing"),
        )
        .unwrap();
        let err = ensure_genesis(&suite, &genesis, false).await.unwrap_err();
        assert!(err.to_string().contains("suite has no genesis URL"));
    }

    #[tokio::test]
    async fn relative_genesis_path_can_error_on_missing_url() {
        let suite = Suite::resolve(
            "custom/1",
            "repricing",
            "amsterdam",
            "stateful",
            Path::new("/missing"),
        )
        .unwrap();
        let err = ensure_genesis(&suite, Path::new("genesis.json"), false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("suite has no genesis URL"));
    }

    #[test]
    fn normalizes_single_nested_datadir() {
        let tmp = tempfile::tempdir().unwrap();
        let datadir = tmp.path().join("out");
        let nested = datadir.join("snapshot");
        fs::create_dir_all(nested.join("db")).unwrap();
        normalize_datadir(&datadir).unwrap();
        assert!(datadir.join("db").exists());
        assert!(!datadir.join("snapshot").exists());
    }

    #[test]
    fn refuses_non_empty_datadir_without_force() {
        let tmp = tempfile::tempdir().unwrap();
        let datadir = tmp.path().join("reth");
        fs::create_dir_all(&datadir).unwrap();
        fs::write(datadir.join("leftover"), b"data").unwrap();
        let err = prepare_datadir(&datadir, false).unwrap_err();
        assert!(err.to_string().contains("pass --force"));
        assert!(datadir.join("leftover").exists());
    }

    #[test]
    fn force_clears_non_empty_datadir() {
        let tmp = tempfile::tempdir().unwrap();
        let datadir = tmp.path().join("reth");
        fs::create_dir_all(&datadir).unwrap();
        fs::write(datadir.join("leftover"), b"data").unwrap();
        prepare_datadir(&datadir, true).unwrap();
        assert!(datadir.exists());
        assert!(is_empty_dir(&datadir).unwrap());
    }

    #[test]
    fn refuses_to_clear_top_level_path() {
        let err = ensure_safe_to_clear(Path::new("/")).unwrap_err();
        assert!(err.to_string().contains("top-level path"));
    }

    #[test]
    fn parses_finish_checkpoint_block() {
        let output =
            "Finish: Some(StageCheckpoint { block_number: 24402727, stage_checkpoint: None })";
        assert_eq!(parse_stage_checkpoint_block(output), Some(24_402_727));
    }

    #[test]
    fn detects_header_command_content() {
        assert!(has_header_content(
            "Header\n{\"parent_hash\":\"0x00\"}\n\nBlockHash\n\"0x01\""
        ));
        assert!(!has_header_content("No content for the given table key."));
    }
}
