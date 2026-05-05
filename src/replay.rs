use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::Serialize;
use serde_json::Value;

use crate::{
    cli::{Cli, ReplayArgs, ReplayMode, RunArgs, RunManyArgs},
    index::{FixtureIndex, StepFile, TestEntry, TestQuery},
    jsonrpc::{parse_request_line, validate_engine_response, JsonRpcResponse},
    schelk,
    suite::Suite,
};

const DRY_RUN_PRINT_LIMIT: usize = 50;

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
    _suite: &Suite,
    index: &FixtureIndex,
    args: RunManyArgs,
) -> Result<()> {
    if args.limit == 0 {
        anyhow::bail!("--limit must be greater than zero");
    }
    let query = TestQuery::from(args.query.clone());
    let matches = index.search(&query)?;
    if matches.is_empty() {
        anyhow::bail!("no tests matched query");
    }
    let mut client = EngineClient::new(&cli.engine_url, cli.jwt_secret.as_deref(), args.dry_run)?;
    for (idx, test) in matches.into_iter().take(args.limit).enumerate() {
        if idx > 0 && !args.no_schelk {
            schelk::recover(&cli.schelk_bin, args.drop_caches)?;
        }
        replay_test(index, test, args.mode, &mut client)
            .await
            .with_context(|| format!("running {}", test.name))?;
    }
    Ok(())
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
    match mode {
        ReplayMode::Prerun => {
            for step in &index.pre_run {
                client.replay_file(&step.abs_path).await?;
            }
        }
        ReplayMode::Funding => {
            for step in index
                .pre_run
                .iter()
                .filter(|step| step.name == "funding.txt")
            {
                client.replay_file(&step.abs_path).await?;
            }
        }
        ReplayMode::Setup => replay_optional_step(client, &test.setup).await?,
        ReplayMode::Testing => replay_optional_step(client, &test.testing).await?,
        ReplayMode::Full => {
            replay_optional_step(client, &test.setup).await?;
            replay_optional_step(client, &test.testing).await?;
        }
    }
    Ok(())
}

async fn replay_optional_step(client: &mut EngineClient, step: &Option<StepFile>) -> Result<()> {
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
}

impl EngineClient {
    pub fn new(endpoint: &str, jwt_secret: Option<&Path>, dry_run: bool) -> Result<Self> {
        let jwt_secret = jwt_secret.map(read_jwt_secret).transpose()?;
        Ok(Self {
            endpoint: endpoint.to_string(),
            http: reqwest::Client::new(),
            jwt_secret,
            dry_run,
        })
    }

    pub async fn replay_file(&mut self, path: &Path) -> Result<()> {
        let data =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut dry_run_printed = 0usize;
        let mut dry_run_omitted = 0usize;
        for (line_no, line) in data.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let request = parse_request_line(line)
                .with_context(|| format!("{}:{}", path.display(), line_no + 1))?;
            if self.dry_run {
                if dry_run_printed < DRY_RUN_PRINT_LIMIT {
                    println!(
                        "dry-run {}:{} {}",
                        path.display(),
                        line_no + 1,
                        request.method
                    );
                    dry_run_printed += 1;
                } else {
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
        if self.dry_run && dry_run_omitted > 0 {
            println!(
                "dry-run {}: omitted {} additional requests after first {}",
                path.display(),
                dry_run_omitted,
                DRY_RUN_PRINT_LIMIT
            );
        }
        Ok(())
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
}
