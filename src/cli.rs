use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "benchmarkoor-replay")]
#[command(about = "Replay benchmarkoor Engine API fixtures against bare Reth")]
pub struct Cli {
    /// Fixture/cache root. Defaults to benchmarkoor-replay cache, reusing an existing benchreplay cache.
    #[arg(long, env = "BENCHMARKOOR_REPLAY_CACHE")]
    pub cache_dir: Option<PathBuf>,

    /// Suite identity, for example perf-devnet-3/24358000 or jochemnet/24402727.
    #[arg(long, global = true, default_value = "perf-devnet-3/24358000")]
    pub suite: String,

    /// Benchmark context from benchmarkoor-tests.
    #[arg(long, global = true, default_value = "repricing")]
    pub context: String,

    /// Fork label from benchmarkoor-tests.
    #[arg(long, global = true, default_value = "amsterdam")]
    pub fork: String,

    /// Test type from benchmarkoor-tests.
    #[arg(long = "test-type", global = true, default_value = "stateful")]
    pub test_type: String,

    /// benchmarkoor-tests config root.
    #[arg(
        long,
        global = true,
        default_value = "/home/ubuntu/projects/benchmarkoor-tests/configs"
    )]
    pub metadata_root: PathBuf,

    /// Engine API endpoint.
    #[arg(
        long,
        global = true,
        env = "BENCHMARKOOR_REPLAY_ENGINE_URL",
        default_value = "http://127.0.0.1:8551"
    )]
    pub engine_url: String,

    /// Reth Engine API JWT secret file.
    #[arg(long, global = true, env = "BENCHMARKOOR_REPLAY_JWT_SECRET")]
    pub jwt_secret: Option<PathBuf>,

    /// schelk binary.
    #[arg(long, global = true, default_value = "schelk")]
    pub schelk_bin: String,

    /// reth binary.
    #[arg(long, global = true, default_value = "reth")]
    pub reth_bin: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand, Clone)]
pub enum Command {
    /// Report cache, suite, schelk, baseline, Reth, and hazard state.
    Status(StatusArgs),
    /// Download and inspect benchmarkoor fixture files.
    #[command(subcommand)]
    Fixtures(FixturesCommand),
    /// Download, extract, normalize, and optionally migrate a Reth snapshot.
    #[command(subcommand)]
    Snapshot(SnapshotCommand),
    /// Prepare, promote, and verify schelk-backed baselines.
    #[command(subcommand)]
    Baseline(BaselineCommand),
    /// Replay raw newline-delimited JSON-RPC files directly.
    Replay(ReplayArgs),
    /// Run one indexed test by filename.
    Run(RunArgs),
    /// Run many indexed tests and recover between tests.
    RunMany(RunManyArgs),
    /// Download a fixture URL, index it, then run one test.
    RunUrl(RunUrlArgs),
    /// Direct schelk helpers for mount/recover.
    #[command(subcommand)]
    Schelk(SchelkCommand),
}

#[derive(Debug, Args, Clone)]
pub struct StatusArgs {
    /// Expected Reth datadir, used for status hazard checks.
    #[arg(long)]
    pub datadir: Option<PathBuf>,
}

#[derive(Debug, Subcommand, Clone)]
pub enum FixturesCommand {
    /// Download and extract the selected fixture archive.
    Download(FixtureDownloadArgs),
    /// Search indexed tests.
    ListTests(ListTestsArgs),
    /// Show one indexed test as JSON.
    ShowTest(ShowTestArgs),
}

#[derive(Debug, Args, Clone)]
pub struct FixtureDownloadArgs {
    /// Direct fixture archive URL, local archive, or local fixture tree.
    #[arg(long, conflicts_with = "release")]
    pub url: Option<crate::fixtures::FixtureSource>,

    /// GitHub release URL. benchmarkoor-replay selects the best matching gas-benchmarks asset.
    #[arg(long)]
    pub release: Option<String>,

    /// Re-download and re-extract even when a cache exists.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args, Clone)]
pub struct QueryArgs {
    /// Exact filename or indexed test name.
    #[arg(long)]
    pub exact: Option<String>,

    /// Substring to match against indexed names and relative paths.
    #[arg(long)]
    pub contains: Option<String>,

    /// Regex to match against indexed names and relative paths.
    #[arg(long)]
    pub pattern: Option<String>,

    /// Opcode filter parsed from fixture filename metadata.
    #[arg(long)]
    pub opcode: Option<String>,

    /// Gas bucket filter, for example 210M.
    #[arg(long = "gas-bucket")]
    pub gas_bucket: Option<String>,

    /// Cache strategy filter, for example NO_CACHE or CacheStrategy.NO_CACHE.
    #[arg(long = "cache-strategy")]
    pub cache_strategy: Option<String>,

    /// Account mode filter, for example EXISTING_EOA.
    #[arg(long = "account-mode")]
    pub account_mode: Option<String>,

    /// Fork filter, for example Amsterdam.
    #[arg(long)]
    pub fork: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub struct ListTestsArgs {
    #[command(flatten)]
    pub query: QueryArgs,

    /// Print copy-pasteable benchmarkoor-replay run commands instead of summaries.
    #[arg(long)]
    pub command: bool,

    /// Mode to include when --command is used.
    #[arg(long, value_enum, default_value_t = ReplayMode::Full)]
    pub mode: ReplayMode,

    /// Maximum rows to print.
    #[arg(long, default_value_t = 100, value_parser = parse_nonzero_limit)]
    pub limit: usize,
}

#[derive(Debug, Args, Clone)]
pub struct ShowTestArgs {
    pub name: String,
}

#[derive(Debug, Subcommand, Clone)]
pub enum SnapshotCommand {
    /// Import the selected suite snapshot.
    Import(SnapshotImportArgs),
}

#[derive(Debug, Args, Clone)]
pub struct SnapshotImportArgs {
    /// Output datadir where snapshot contents should land.
    #[arg(long)]
    pub datadir: PathBuf,

    /// Override the snapshot URL from suite metadata.
    #[arg(long)]
    pub url: Option<String>,

    /// Path to write/read the genesis file.
    #[arg(long)]
    pub genesis: Option<PathBuf>,

    /// Run reth db migrate-v2 after extraction.
    #[arg(long = "migrate-v2")]
    pub migrate_v2: bool,

    /// Skip downloading when archive already exists in cache.
    #[arg(long)]
    pub offline: bool,

    /// Remove an existing non-empty datadir before extracting.
    #[arg(long)]
    pub force: bool,

    /// Expected head block number. Defaults to suite block.
    #[arg(long = "expected-head")]
    pub expected_head: Option<u64>,
}

#[derive(Debug, Subcommand, Clone)]
pub enum BaselineCommand {
    /// Mount schelk and optionally run prerun steps without promoting.
    Prepare(BaselinePrepareArgs),
    /// Run prerun steps and explicitly promote scratch to the baseline.
    PromotePrerun(BaselinePromoteArgs),
    /// Verify baseline marker and selected suite metadata.
    Verify(BaselineVerifyArgs),
}

#[derive(Debug, Args, Clone)]
pub struct BaselinePrepareArgs {
    /// Skip gas-bump/funding.
    #[arg(long)]
    pub skip_prerun: bool,

    /// Do not call schelk mount.
    #[arg(long)]
    pub no_mount: bool,

    /// Print replay requests without mounting schelk or writing a marker.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args, Clone)]
pub struct BaselinePromoteArgs {
    /// Kill processes blocking schelk promote.
    #[arg(long)]
    pub kill: bool,

    /// Print replay requests without promoting schelk or writing a marker.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args, Clone)]
pub struct BaselineVerifyArgs {
    /// Expected datadir.
    #[arg(long)]
    pub datadir: Option<PathBuf>,
}

#[derive(Debug, Args, Clone)]
pub struct ReplayArgs {
    /// Files to replay in order.
    #[arg(required = true)]
    pub files: Vec<PathBuf>,

    /// Replay without sending requests.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args, Clone)]
pub struct RunArgs {
    /// Exact test filename or indexed test name.
    #[arg(long)]
    pub test: String,

    /// Which files to replay.
    #[arg(long, value_enum, default_value_t = ReplayMode::Full)]
    pub mode: ReplayMode,

    /// Replay without sending requests.
    #[arg(long)]
    pub dry_run: bool,

    /// Do not call schelk recover before the run.
    #[arg(long)]
    pub no_schelk: bool,

    /// Recover before this run.
    #[arg(long)]
    pub recover_before: bool,
}

#[derive(Debug, Args, Clone)]
pub struct RunManyArgs {
    #[command(flatten)]
    pub query: QueryArgs,

    /// Which files to replay for each test.
    #[arg(long, value_enum, default_value_t = RunManyMode::Full)]
    pub mode: RunManyMode,

    /// Maximum tests to run.
    #[arg(long, default_value_t = 10, value_parser = parse_nonzero_limit)]
    pub limit: usize,

    /// Number of repetitions to run for each selected test.
    #[arg(long, default_value_t = 1, value_parser = parse_nonzero_limit)]
    pub repetitions: usize,

    /// Replay without sending requests.
    #[arg(long)]
    pub dry_run: bool,

    /// Do not call schelk recover between tests, or before each measured repetition.
    #[arg(long)]
    pub no_schelk: bool,

    /// Drop Linux page cache after each schelk recover, or after setup in measured mode.
    #[arg(long)]
    pub drop_caches: bool,

    /// Print one structured JSON result per test repetition.
    #[arg(long)]
    pub json: bool,

    /// Shell command that restarts the node after setup and before measured testing.
    #[arg(long, env = "BENCHMARKOOR_REPLAY_RESTART_NODE_COMMAND")]
    pub restart_node_command: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub struct RunUrlArgs {
    /// Fixture archive URL.
    pub url: String,

    /// Test filename to run after indexing.
    #[arg(long)]
    pub test: String,

    /// Which files to replay.
    #[arg(long, value_enum, default_value_t = ReplayMode::Full)]
    pub mode: ReplayMode,

    /// Re-download and re-extract even when cache exists.
    #[arg(long)]
    pub force: bool,

    /// Replay without sending requests.
    #[arg(long)]
    pub dry_run: bool,

    /// Do not call schelk recover before the run.
    #[arg(long)]
    pub no_schelk: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReplayMode {
    /// Gas bump plus funding pre-run files.
    Prerun,
    /// Funding pre-run files only.
    Funding,
    /// Setup file only.
    Setup,
    /// Testing file only.
    Testing,
    /// Setup then testing, without recovery between them.
    Full,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunManyMode {
    /// Gas bump plus funding pre-run files.
    Prerun,
    /// Funding pre-run files only.
    Funding,
    /// Setup file only.
    Setup,
    /// Testing file only.
    Testing,
    /// Setup then testing, without recovery between them.
    Full,
    /// Recover, run setup unmeasured, optionally restart/drop caches, then measure testing.
    SetupThenTesting,
}

impl RunManyMode {
    pub fn as_replay_mode(self) -> Option<ReplayMode> {
        match self {
            Self::Prerun => Some(ReplayMode::Prerun),
            Self::Funding => Some(ReplayMode::Funding),
            Self::Setup => Some(ReplayMode::Setup),
            Self::Testing => Some(ReplayMode::Testing),
            Self::Full => Some(ReplayMode::Full),
            Self::SetupThenTesting => None,
        }
    }
}

#[derive(Debug, Subcommand, Clone)]
pub enum SchelkCommand {
    Mount,
    Recover(SchelkRecoverArgs),
    /// Run schelk full-recover after an explicit acknowledgement.
    FullRecover(SchelkFullRecoverArgs),
}

#[derive(Debug, Args, Clone)]
pub struct SchelkRecoverArgs {
    #[arg(long)]
    pub kill: bool,

    #[arg(long)]
    pub drop_caches: bool,
}

#[derive(Debug, Args, Clone)]
pub struct SchelkFullRecoverArgs {
    /// Required acknowledgement because full-recover overwrites scratch from virgin.
    #[arg(long)]
    pub yes: bool,
}

fn parse_nonzero_limit(value: &str) -> Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|err| format!("invalid limit: {err}"))?;
    if limit == 0 {
        return Err("--limit must be greater than zero".to_string());
    }
    Ok(limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_limit() {
        assert_eq!(parse_nonzero_limit("5"), Ok(5));
        assert!(parse_nonzero_limit("0")
            .unwrap_err()
            .contains("greater than zero"));
    }
}
