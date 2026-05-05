use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Suite {
    pub id: String,
    pub network: String,
    pub block: u64,
    pub context: String,
    pub fork: String,
    pub test_type: String,
    pub release_url: String,
    pub fixture_url: String,
    pub genesis_url: String,
    pub snapshot_url: String,
    pub source: SuiteSource,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuiteSource {
    pub labels: serde_json::Map<String, Value>,
    pub pre_run_steps: Vec<String>,
    pub setup_globs: Vec<String>,
    pub testing_globs: Vec<String>,
    pub opcode_source: Option<String>,
}

impl Suite {
    pub fn resolve(
        id: &str,
        context: &str,
        fork: &str,
        test_type: &str,
        metadata_root: &Path,
    ) -> Result<Self> {
        let mut suite = known_suite(id, context, fork, test_type)?;
        if let Some(source) =
            read_benchmarkoor_source(metadata_root, &suite, context, fork, test_type)?
        {
            suite.source = source;
        }
        Ok(suite)
    }

    pub fn slug(&self) -> String {
        sanitize_slug(&format!(
            "{}-{}-{}-{}-{}",
            self.network, self.block, self.context, self.fork, self.test_type
        ))
    }
}

pub fn sanitize_slug(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn known_suite(id: &str, context: &str, fork: &str, test_type: &str) -> Result<Suite> {
    let release_url =
        "https://github.com/NethermindEth/gas-benchmarks/releases/tag/amsterdam-repricings-v4.1.0"
            .to_string();

    match id {
        "perf-devnet-3/24358000" => Ok(Suite {
            id: id.to_string(),
            network: "perf-devnet-3".to_string(),
            block: 24_358_000,
            context: context.to_string(),
            fork: fork.to_string(),
            test_type: test_type.to_string(),
            release_url,
            fixture_url: "https://github.com/NethermindEth/gas-benchmarks/releases/download/amsterdam-repricings-v4.1.0/generated-tests-stateful-perf-devnet-3.tar.gz".to_string(),
            genesis_url: "https://gist.githubusercontent.com/skylenet/83b19b06b91fb44e0131d275a1aa0495/raw/4fb5ed66370bc5dc7c6745a775d4972dd6c1a098/genesis-perf-devnet-3-24358000-amsterdam-genesis.json".to_string(),
            snapshot_url: "https://snapshots.ethpandaops.io/perf-devnet-3/reth/24358000/snapshot.tar.zst".to_string(),
            source: default_source("perf-devnet-3", context, fork, test_type),
        }),
        "jochemnet/24402727" => Ok(Suite {
            id: id.to_string(),
            network: "jochemnet".to_string(),
            block: 24_402_727,
            context: context.to_string(),
            fork: fork.to_string(),
            test_type: test_type.to_string(),
            release_url,
            fixture_url: "https://github.com/NethermindEth/gas-benchmarks/releases/download/amsterdam-repricings-v4.1.0/generated-tests-stateful-jochemnet.tar.gz".to_string(),
            genesis_url: "https://gist.githubusercontent.com/skylenet/8226878ba786efcf10b5fd74be97fe41/raw/ab888a0f90dcf214bd5d3c36b3dfa1827335ecf9/genesis-jochemnet-24402727-amsterdam-genesis.json".to_string(),
            snapshot_url: "https://snapshots.ethpandaops.io/jochemnet/reth/24402727/snapshot.tar.zst".to_string(),
            source: default_source("jochemnet", context, fork, test_type),
        }),
        other => {
            let (network, block) = other
                .split_once('/')
                .ok_or_else(|| anyhow::anyhow!("suite must be network/block, got {other}"))?;
            Ok(Suite {
                id: other.to_string(),
                network: network.to_string(),
                block: block.parse().context("parsing suite block")?,
                context: context.to_string(),
                fork: fork.to_string(),
                test_type: test_type.to_string(),
                release_url,
                fixture_url: String::new(),
                genesis_url: String::new(),
                snapshot_url: String::new(),
                source: default_source(network, context, fork, test_type),
            })
        }
    }
}

fn default_source(network: &str, context: &str, fork: &str, test_type: &str) -> SuiteSource {
    let root = match context {
        "bal" => "eest_bal".to_string(),
        _ if test_type == "compute" => format!("repricings_compute/{network}"),
        _ => format!("repricings_stateful/{network}"),
    };

    let mut labels = serde_json::Map::new();
    labels.insert("chain".to_string(), Value::String(network.to_string()));
    labels.insert("context".to_string(), Value::String(context.to_string()));
    labels.insert("fork".to_string(), Value::String(fork.to_string()));
    labels.insert(
        "test-type".to_string(),
        Value::String(test_type.to_string()),
    );

    SuiteSource {
        labels,
        pre_run_steps: vec![
            format!("{root}/gas-bump.txt"),
            format!("{root}/funding.txt"),
        ],
        setup_globs: vec![format!("{root}/setup/*.txt")],
        testing_globs: vec![format!("{root}/testing/*.txt")],
        opcode_source: None,
    }
}

fn read_benchmarkoor_source(
    metadata_root: &Path,
    suite: &Suite,
    context: &str,
    fork: &str,
    test_type: &str,
) -> Result<Option<SuiteSource>> {
    let path = metadata_root
        .join("contexts")
        .join(context)
        .join(&suite.network)
        .join(suite.block.to_string())
        .join(fork)
        .join(format!("test-source.{test_type}.yaml"));

    if !path.exists() {
        return Ok(None);
    }

    let data = fs::read_to_string(&path)
        .with_context(|| format!("reading benchmarkoor source config {}", path.display()))?;
    let yaml: serde_yaml::Value = serde_yaml::from_str(&data)
        .with_context(|| format!("parsing benchmarkoor source config {}", path.display()))?;
    Ok(Some(source_from_yaml(&yaml)?))
}

fn source_from_yaml(yaml: &serde_yaml::Value) -> Result<SuiteSource> {
    let labels = get_path(
        yaml,
        &["runner", "benchmark", "tests", "metadata", "labels"],
    )
    .and_then(|v| serde_json::to_value(v).ok())
    .and_then(|v| v.as_object().cloned())
    .unwrap_or_default();

    let archive = get_path(yaml, &["runner", "benchmark", "tests", "source", "archive"]);
    let pre_run_steps = archive
        .and_then(|v| get_key(v, "pre_run_steps"))
        .map(string_array)
        .unwrap_or_default();
    let setup_globs = archive
        .and_then(|v| get_path(v, &["steps", "setup"]))
        .map(string_array)
        .unwrap_or_default();
    let testing_globs = archive
        .and_then(|v| get_path(v, &["steps", "test"]))
        .map(string_array)
        .unwrap_or_default();
    let opcode_source = get_path(
        yaml,
        &["runner", "benchmark", "tests", "opcode_source", "file"],
    )
    .and_then(|v| v.as_str())
    .map(ToOwned::to_owned);

    Ok(SuiteSource {
        labels,
        pre_run_steps,
        setup_globs,
        testing_globs,
        opcode_source,
    })
}

fn get_path<'a>(value: &'a serde_yaml::Value, path: &[&str]) -> Option<&'a serde_yaml::Value> {
    let mut current = value;
    for key in path {
        current = get_key(current, key)?;
    }
    Some(current)
}

fn get_key<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a serde_yaml::Value> {
    value
        .as_mapping()?
        .get(serde_yaml::Value::String(key.to_string()))
}

fn string_array(value: &serde_yaml::Value) -> Vec<String> {
    value
        .as_sequence()
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_first_class_suites() {
        let perf = known_suite(
            "perf-devnet-3/24358000",
            "repricing",
            "amsterdam",
            "stateful",
        )
        .unwrap();
        assert_eq!(perf.network, "perf-devnet-3");
        assert_eq!(perf.block, 24_358_000);
        assert!(perf.snapshot_url.contains("perf-devnet-3/reth/24358000"));

        let jochem =
            known_suite("jochemnet/24402727", "repricing", "amsterdam", "stateful").unwrap();
        assert_eq!(jochem.network, "jochemnet");
        assert!(jochem
            .fixture_url
            .ends_with("generated-tests-stateful-jochemnet.tar.gz"));
    }

    #[test]
    fn slug_is_path_safe() {
        assert_eq!(
            sanitize_slug("perf-devnet-3/24358000/amsterdam"),
            "perf-devnet-3-24358000-amsterdam"
        );
    }
}
