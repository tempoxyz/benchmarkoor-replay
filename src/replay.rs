use std::{
    fs,
    path::Path,
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::Serialize;
use serde_json::Value;

use crate::{
    cli::{Cli, ReplayArgs, ReplayMode, RunArgs, RunManyArgs, RunManyMode},
    index::{FixtureIndex, StepFile, TestEntry, TestQuery},
    jsonrpc::{parse_request_line, request_metrics, validate_engine_response, JsonRpcResponse},
    schelk,
    suite::Suite,
};

const DRY_RUN_PRINT_LIMIT: usize = 50;

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct ReplayStats {
    pub request_count: u64,
    pub payload_count: u64,
    pub gas_used: u64,
}

impl ReplayStats {
    fn add(&mut self, other: ReplayStats) {
        self.request_count += other.request_count;
        self.payload_count += other.payload_count;
        self.gas_used += other.gas_used;
    }
}

#[derive(Clone, Debug, Serialize)]
struct RunManyResult {
    kind: &'static str,
    suite: String,
    test: String,
    repetition: usize,
    mode: RunManyMode,
    started_at: String,
    schelk_recovered: bool,
    cache_drop: schelk::DropCachesReport,
    node_restart: NodeRestartReport,
    #[serde(rename = "setup_elapsed")]
    setup_elapsed_secs: Option<f64>,
    #[serde(rename = "testing_elapsed")]
    testing_elapsed_secs: Option<f64>,
    #[serde(rename = "total_elapsed")]
    total_elapsed_secs: f64,
    setup_request_count: u64,
    setup_payload_count: u64,
    setup_gas_used: u64,
    testing_request_count: u64,
    testing_payload_count: u64,
    testing_gas_used: u64,
    request_count: u64,
    payload_count: u64,
    gas_used: u64,
    #[serde(rename = "gas_per_sec")]
    testing_gas_per_sec: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
struct NodeRestartReport {
    requested: bool,
    command: Option<String>,
    succeeded: bool,
    elapsed_secs: Option<f64>,
    error: Option<String>,
}

impl NodeRestartReport {
    fn not_requested() -> Self {
        Self {
            requested: false,
            command: None,
            succeeded: false,
            elapsed_secs: None,
            error: None,
        }
    }
}

pub async fn replay_files(cli: &Cli, args: ReplayArgs) -> Result<()> {
    let mut client = EngineClient::new(&cli.engine_url, cli.jwt_secret.as_deref(), args.dry_run)?;
    for file in args.files {
        client.replay_file(&file).await?;
    }
    Ok(())
}

pub async fn run_one(cli: &Cli, _suite: &Suite, index: &FixtureIndex, args: RunArgs) -> Result<()> {
    let test = index
        .find_one(&args.test)?
        .ok_or_else(|| anyhow::anyhow!("test not found: {}", args.test))?;
    if args.recover_before && !args.no_schelk {
        schelk::recover(&cli.schelk_bin, false)?;
    }
    let mut client = EngineClient::new(&cli.engine_url, cli.jwt_secret.as_deref(), args.dry_run)?;
    replay_test(index, test, args.mode, &mut client).await
}

pub async fn run_many(
    cli: &Cli,
    suite: &Suite,
    index: &FixtureIndex,
    args: RunManyArgs,
) -> Result<()> {
    if args.limit == 0 {
        anyhow::bail!("--limit must be greater than zero");
    }
    if args.repetitions == 0 {
        anyhow::bail!("--repetitions must be greater than zero");
    }
    if args.restart_node_command.is_some() && args.mode != RunManyMode::SetupThenTesting {
        anyhow::bail!("--restart-node-command requires --mode setup-then-testing");
    }
    let query = TestQuery::from(args.query.clone());
    let matches = index.search(&query)?;
    if matches.is_empty() {
        anyhow::bail!("no tests matched query");
    }

    let tests = matches.into_iter().take(args.limit).collect::<Vec<_>>();
    let mut run_index = 0usize;
    for test in tests {
        for repetition in 1..=args.repetitions {
            let result = if args.mode == RunManyMode::SetupThenTesting {
                run_setup_then_testing(cli, suite, test, repetition, &args).await
            } else {
                run_simple_many(cli, suite, index, test, repetition, &args, run_index > 0).await
            }
            .with_context(|| format!("running {} repetition {}", test.name, repetition))?;
            emit_run_many_result(&result, args.json)?;
            run_index += 1;
        }
    }
    Ok(())
}

async fn run_simple_many(
    cli: &Cli,
    suite: &Suite,
    index: &FixtureIndex,
    test: &TestEntry,
    repetition: usize,
    args: &RunManyArgs,
    recover_before: bool,
) -> Result<RunManyResult> {
    let started_at = chrono::Utc::now().to_rfc3339();
    let total_start = Instant::now();
    let mut cache_drop = schelk::DropCachesReport::not_requested();
    let mut schelk_recovered = false;

    if recover_before && !args.no_schelk {
        schelk::run(&cli.schelk_bin, ["recover"])?;
        schelk_recovered = true;
        if args.drop_caches {
            cache_drop = schelk::drop_caches()?;
        }
    }

    let mode = args
        .mode
        .as_replay_mode()
        .ok_or_else(|| anyhow::anyhow!("setup-then-testing requires measured orchestration"))?;
    let mut client = run_many_client(cli, args)?;
    let (stats, elapsed) = timed_replay_test(index, test, mode, &mut client).await?;
    let (setup_stats, testing_stats, testing_elapsed_secs) = match mode {
        ReplayMode::Setup => (stats, ReplayStats::default(), None),
        ReplayMode::Testing => (ReplayStats::default(), stats, Some(seconds(elapsed))),
        _ => (ReplayStats::default(), ReplayStats::default(), None),
    };

    Ok(build_run_many_result(RunManyResultParts {
        suite,
        test,
        repetition,
        mode: args.mode,
        started_at,
        schelk_recovered,
        cache_drop,
        node_restart: NodeRestartReport::not_requested(),
        setup_elapsed_secs: None,
        testing_elapsed_secs,
        total_elapsed: total_start.elapsed(),
        setup_stats,
        testing_stats,
        total_stats: stats,
    }))
}

async fn run_setup_then_testing(
    cli: &Cli,
    suite: &Suite,
    test: &TestEntry,
    repetition: usize,
    args: &RunManyArgs,
) -> Result<RunManyResult> {
    let started_at = chrono::Utc::now().to_rfc3339();
    let total_start = Instant::now();
    let mut schelk_recovered = false;

    if !args.no_schelk {
        schelk::run(&cli.schelk_bin, ["recover"])?;
        schelk_recovered = true;
    }

    let mut setup_client = run_many_client(cli, args)?;
    let (setup_stats, setup_elapsed) =
        timed_replay_optional_step(&mut setup_client, &test.setup).await?;
    drop(setup_client);

    let node_restart = restart_node(args.restart_node_command.as_deref())?;
    let cache_drop = if args.drop_caches {
        schelk::drop_caches()?
    } else {
        schelk::DropCachesReport::not_requested()
    };

    let mut testing_client = run_many_client(cli, args)?;
    let (testing_stats, testing_elapsed) =
        timed_replay_optional_step(&mut testing_client, &test.testing).await?;
    let mut total_stats = setup_stats;
    total_stats.add(testing_stats);

    Ok(build_run_many_result(RunManyResultParts {
        suite,
        test,
        repetition,
        mode: args.mode,
        started_at,
        schelk_recovered,
        cache_drop,
        node_restart,
        setup_elapsed_secs: Some(seconds(setup_elapsed)),
        testing_elapsed_secs: Some(seconds(testing_elapsed)),
        total_elapsed: total_start.elapsed(),
        setup_stats,
        testing_stats,
        total_stats,
    }))
}

struct RunManyResultParts<'a> {
    suite: &'a Suite,
    test: &'a TestEntry,
    repetition: usize,
    mode: RunManyMode,
    started_at: String,
    schelk_recovered: bool,
    cache_drop: schelk::DropCachesReport,
    node_restart: NodeRestartReport,
    setup_elapsed_secs: Option<f64>,
    testing_elapsed_secs: Option<f64>,
    total_elapsed: Duration,
    setup_stats: ReplayStats,
    testing_stats: ReplayStats,
    total_stats: ReplayStats,
}

fn build_run_many_result(parts: RunManyResultParts<'_>) -> RunManyResult {
    RunManyResult {
        kind: "run_many_result",
        suite: parts.suite.id.clone(),
        test: parts.test.name.clone(),
        repetition: parts.repetition,
        mode: parts.mode,
        started_at: parts.started_at,
        schelk_recovered: parts.schelk_recovered,
        cache_drop: parts.cache_drop,
        node_restart: parts.node_restart,
        setup_elapsed_secs: parts.setup_elapsed_secs,
        testing_elapsed_secs: parts.testing_elapsed_secs,
        total_elapsed_secs: seconds(parts.total_elapsed),
        setup_request_count: parts.setup_stats.request_count,
        setup_payload_count: parts.setup_stats.payload_count,
        setup_gas_used: parts.setup_stats.gas_used,
        testing_request_count: parts.testing_stats.request_count,
        testing_payload_count: parts.testing_stats.payload_count,
        testing_gas_used: parts.testing_stats.gas_used,
        request_count: parts.total_stats.request_count,
        payload_count: parts.total_stats.payload_count,
        gas_used: parts.total_stats.gas_used,
        testing_gas_per_sec: parts.testing_elapsed_secs.and_then(|elapsed| {
            (elapsed > 0.0 && parts.testing_stats.gas_used > 0)
                .then(|| parts.testing_stats.gas_used as f64 / elapsed)
        }),
    }
}

async fn timed_replay_test(
    index: &FixtureIndex,
    test: &TestEntry,
    mode: ReplayMode,
    client: &mut EngineClient,
) -> Result<(ReplayStats, Duration)> {
    let start = Instant::now();
    let stats = replay_test_stats(index, test, mode, client).await?;
    Ok((stats, start.elapsed()))
}

async fn timed_replay_optional_step(
    client: &mut EngineClient,
    step: &Option<StepFile>,
) -> Result<(ReplayStats, Duration)> {
    let start = Instant::now();
    let stats = replay_optional_step(client, step).await?;
    Ok((stats, start.elapsed()))
}

fn restart_node(command: Option<&str>) -> Result<NodeRestartReport> {
    let Some(command) = command else {
        return Ok(NodeRestartReport::not_requested());
    };

    let start = Instant::now();
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .with_context(|| format!("running restart node command: {command}"))?;
    let elapsed_secs = Some(seconds(start.elapsed()));
    if output.status.success() {
        return Ok(NodeRestartReport {
            requested: true,
            command: Some(command.to_string()),
            succeeded: true,
            elapsed_secs,
            error: None,
        });
    }

    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let error = format!("command exited with {}: {}", output.status, text.trim());
    Err(anyhow::anyhow!(error)).with_context(|| format!("restart node command failed: {command}"))
}

fn emit_run_many_result(result: &RunManyResult, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(result)?);
        return Ok(());
    }

    if result.mode == RunManyMode::SetupThenTesting {
        println!(
            "test={} repetition={} testing_elapsed_secs={:.6} testing_requests={} testing_payloads={} testing_gas={} testing_gas_per_sec={:.3} drop_caches={} node_restart={}",
            result.test,
            result.repetition,
            result.testing_elapsed_secs.unwrap_or_default(),
            result.testing_request_count,
            result.testing_payload_count,
            result.testing_gas_used,
            result.testing_gas_per_sec.unwrap_or_default(),
            report_state(result.cache_drop.requested, result.cache_drop.succeeded),
            report_state(result.node_restart.requested, result.node_restart.succeeded),
        );
    }
    Ok(())
}

fn report_state(requested: bool, succeeded: bool) -> &'static str {
    match (requested, succeeded) {
        (false, _) => "not-requested",
        (true, true) => "succeeded",
        (true, false) => "failed",
    }
}

fn seconds(duration: Duration) -> f64 {
    duration.as_secs_f64()
}

fn run_many_client(cli: &Cli, args: &RunManyArgs) -> Result<EngineClient> {
    EngineClient::new_with_dry_run_print(
        &cli.engine_url,
        cli.jwt_secret.as_deref(),
        args.dry_run,
        !args.json,
    )
}

pub fn run_command(
    cli: &Cli,
    suite: &Suite,
    test_name: &str,
    mode: ReplayMode,
    cache_dir: &Path,
) -> String {
    let mut cmd = vec![
        "benchmarkoor-replay".to_string(),
        "--suite".to_string(),
        shell_words::quote(&suite.id).into_owned(),
        "--context".to_string(),
        shell_words::quote(&suite.context).into_owned(),
        "--fork".to_string(),
        shell_words::quote(&suite.fork).into_owned(),
        "--test-type".to_string(),
        shell_words::quote(&suite.test_type).into_owned(),
        "--metadata-root".to_string(),
        shell_words::quote(&cli.metadata_root.display().to_string()).into_owned(),
        "--cache-dir".to_string(),
        shell_words::quote(&cache_dir.display().to_string()).into_owned(),
        "--engine-url".to_string(),
        shell_words::quote(&cli.engine_url).into_owned(),
        "--schelk-bin".to_string(),
        shell_words::quote(&cli.schelk_bin).into_owned(),
        "--reth-bin".to_string(),
        shell_words::quote(&cli.reth_bin).into_owned(),
    ];
    if let Some(jwt_secret) = &cli.jwt_secret {
        cmd.push("--jwt-secret".to_string());
        cmd.push(shell_words::quote(&jwt_secret.display().to_string()).into_owned());
    }
    cmd.extend([
        "run".to_string(),
        "--test".to_string(),
        shell_words::quote(test_name).into_owned(),
        "--mode".to_string(),
        format!("{mode:?}").to_ascii_lowercase(),
    ]);
    cmd.join(" ")
}

pub async fn replay_test(
    index: &FixtureIndex,
    test: &TestEntry,
    mode: ReplayMode,
    client: &mut EngineClient,
) -> Result<()> {
    replay_test_stats(index, test, mode, client).await?;
    Ok(())
}

async fn replay_test_stats(
    index: &FixtureIndex,
    test: &TestEntry,
    mode: ReplayMode,
    client: &mut EngineClient,
) -> Result<ReplayStats> {
    let mut stats = ReplayStats::default();
    match mode {
        ReplayMode::Prerun => {
            for step in &index.pre_run {
                stats.add(client.replay_file(&step.abs_path).await?);
            }
        }
        ReplayMode::Funding => {
            for step in index
                .pre_run
                .iter()
                .filter(|step| step.name == "funding.txt")
            {
                stats.add(client.replay_file(&step.abs_path).await?);
            }
        }
        ReplayMode::Setup => stats.add(replay_optional_step(client, &test.setup).await?),
        ReplayMode::Testing => stats.add(replay_optional_step(client, &test.testing).await?),
        ReplayMode::Full => {
            stats.add(replay_optional_step(client, &test.setup).await?);
            stats.add(replay_optional_step(client, &test.testing).await?);
        }
    }
    Ok(stats)
}

async fn replay_optional_step(
    client: &mut EngineClient,
    step: &Option<StepFile>,
) -> Result<ReplayStats> {
    let step = step
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("selected test does not have this step"))?;
    client.replay_file(&step.abs_path).await
}

pub struct EngineClient {
    endpoint: String,
    http: reqwest::Client,
    jwt_secret: Option<Vec<u8>>,
    dry_run: bool,
    dry_run_print: bool,
}

impl EngineClient {
    pub fn new(endpoint: &str, jwt_secret: Option<&Path>, dry_run: bool) -> Result<Self> {
        Self::new_with_dry_run_print(endpoint, jwt_secret, dry_run, true)
    }

    fn new_with_dry_run_print(
        endpoint: &str,
        jwt_secret: Option<&Path>,
        dry_run: bool,
        dry_run_print: bool,
    ) -> Result<Self> {
        let jwt_secret = jwt_secret.map(read_jwt_secret).transpose()?;
        Ok(Self {
            endpoint: endpoint.to_string(),
            http: reqwest::Client::new(),
            jwt_secret,
            dry_run,
            dry_run_print,
        })
    }

    pub async fn replay_file(&mut self, path: &Path) -> Result<ReplayStats> {
        let data =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut stats = ReplayStats::default();
        let mut dry_run_printed = 0usize;
        let mut dry_run_omitted = 0usize;
        for (line_no, line) in data.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let request = parse_request_line(line)
                .with_context(|| format!("{}:{}", path.display(), line_no + 1))?;
            stats.request_count += 1;
            let metrics = request_metrics(&request).with_context(|| {
                format!("extracting metrics at {}:{}", path.display(), line_no + 1)
            })?;
            stats.payload_count += metrics.payload_count;
            stats.gas_used += metrics.gas_used;
            if self.dry_run {
                if self.dry_run_print && dry_run_printed < DRY_RUN_PRINT_LIMIT {
                    println!(
                        "dry-run {}:{} {}",
                        path.display(),
                        line_no + 1,
                        request.method
                    );
                    dry_run_printed += 1;
                } else if self.dry_run_print {
                    dry_run_omitted += 1;
                }
                continue;
            }
            let request_value = serde_json::to_value(&request)?;
            let request_object = request_value
                .as_object()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("serialized JSON-RPC request was not an object"))?;
            let response = self
                .send(Value::Object(request_object))
                .await
                .with_context(|| {
                    format!(
                        "sending {} from {}:{}",
                        request.method,
                        path.display(),
                        line_no + 1
                    )
                })?;
            validate_engine_response(&request.method, &response).with_context(|| {
                format!(
                    "validating {} response at {}:{}",
                    request.method,
                    path.display(),
                    line_no + 1
                )
            })?;
        }
        if self.dry_run && self.dry_run_print && dry_run_omitted > 0 {
            println!(
                "dry-run {}: omitted {} additional requests after first {}",
                path.display(),
                dry_run_omitted,
                DRY_RUN_PRINT_LIMIT
            );
        }
        Ok(stats)
    }

    async fn send(&self, request: Value) -> Result<JsonRpcResponse> {
        let mut builder = self.http.post(&self.endpoint).json(&request);
        if let Some(secret) = &self.jwt_secret {
            let mut headers = HeaderMap::new();
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", jwt(secret)?))?,
            );
            builder = builder.headers(headers);
        }
        let response = builder
            .send()
            .await?
            .error_for_status()?
            .json::<JsonRpcResponse>()
            .await
            .context("parsing JSON-RPC response")?;
        Ok(response)
    }
}

fn read_jwt_secret(path: &Path) -> Result<Vec<u8>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading JWT secret {}", path.display()))?;
    let trimmed = raw.trim().strip_prefix("0x").unwrap_or(raw.trim());
    hex::decode(trimmed).with_context(|| format!("decoding JWT secret {}", path.display()))
}

#[derive(Serialize)]
struct Claims {
    iat: usize,
}

fn jwt(secret: &[u8]) -> Result<String> {
    let iat = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as usize;
    let header = Header::new(Algorithm::HS256);
    Ok(jsonwebtoken::encode(
        &header,
        &Claims { iat },
        &EncodingKey::from_secret(secret),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_copy_pasteable_command() {
        use crate::cli::{Command, StatusArgs};

        let suite = Suite::resolve(
            "perf-devnet-3/24358000",
            "repricing",
            "amsterdam",
            "stateful",
            Path::new("/tmp/metadata"),
        )
        .unwrap();
        let cli = Cli {
            cache_dir: None,
            suite: suite.id.clone(),
            context: suite.context.clone(),
            fork: suite.fork.clone(),
            test_type: suite.test_type.clone(),
            metadata_root: Path::new("/tmp/metadata").to_path_buf(),
            engine_url: "http://127.0.0.1:8551".to_string(),
            jwt_secret: Some(Path::new("/tmp/jwt hex").to_path_buf()),
            schelk_bin: "schelk".to_string(),
            reth_bin: "reth".to_string(),
            command: Command::Status(StatusArgs { datadir: None }),
        };
        let cmd = run_command(
            &cli,
            &suite,
            "test file.txt",
            ReplayMode::Full,
            Path::new("/tmp/cache"),
        );
        assert!(cmd.contains("--suite perf-devnet-3/24358000"));
        assert!(cmd.starts_with("benchmarkoor-replay "));
        assert!(cmd.contains("--context repricing"));
        assert!(cmd.contains("--metadata-root /tmp/metadata"));
        assert!(cmd.contains("--jwt-secret '/tmp/jwt hex'"));
        assert!(cmd.contains("--test 'test file.txt'"));
        assert!(cmd.contains("--mode full"));
    }

    #[tokio::test]
    async fn replay_file_collects_dry_run_stats() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            tmp.path(),
            concat!(
                r#"{"jsonrpc":"2.0","id":1,"method":"engine_newPayloadV3","params":[{"gasUsed":"0x64"}]}"#,
                "\n",
                r#"{"jsonrpc":"2.0","id":2,"method":"engine_forkchoiceUpdatedV3","params":[]}"#,
                "\n"
            ),
        )
        .unwrap();

        let mut client = EngineClient::new("http://127.0.0.1:8551", None, true).unwrap();
        let stats = client.replay_file(tmp.path()).await.unwrap();
        assert_eq!(stats.request_count, 2);
        assert_eq!(stats.payload_count, 1);
        assert_eq!(stats.gas_used, 100);
    }

    #[test]
    fn run_many_result_json_uses_measured_field_names() {
        let suite = Suite::resolve(
            "perf-devnet-3/24358000",
            "repricing",
            "amsterdam",
            "stateful",
            Path::new("/tmp/metadata"),
        )
        .unwrap();
        let test = TestEntry {
            name: "example.txt".to_string(),
            ..Default::default()
        };
        let result = build_run_many_result(RunManyResultParts {
            suite: &suite,
            test: &test,
            repetition: 1,
            mode: RunManyMode::SetupThenTesting,
            started_at: "2026-05-12T00:00:00Z".to_string(),
            schelk_recovered: true,
            cache_drop: schelk::DropCachesReport::not_requested(),
            node_restart: NodeRestartReport::not_requested(),
            setup_elapsed_secs: Some(1.0),
            testing_elapsed_secs: Some(2.0),
            total_elapsed: Duration::from_secs(3),
            setup_stats: ReplayStats::default(),
            testing_stats: ReplayStats {
                request_count: 2,
                payload_count: 1,
                gas_used: 100,
            },
            total_stats: ReplayStats {
                request_count: 2,
                payload_count: 1,
                gas_used: 100,
            },
        });
        let json = serde_json::to_value(result).unwrap();
        assert_eq!(json["testing_elapsed"], 2.0);
        assert_eq!(json["gas_per_sec"], 50.0);
        assert!(json.get("testing_elapsed_secs").is_none());
        assert!(json.get("testing_gas_per_sec").is_none());
    }
}
