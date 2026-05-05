use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::cli::QueryArgs;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FixtureIndex {
    pub suite_id: String,
    pub root: PathBuf,
    pub generated_at: String,
    pub pre_run: Vec<StepFile>,
    pub tests: Vec<TestEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepFile {
    pub name: String,
    pub rel_path: String,
    pub abs_path: PathBuf,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TestEntry {
    pub name: String,
    pub setup: Option<StepFile>,
    pub testing: Option<StepFile>,
    pub cleanup: Option<StepFile>,
    pub metadata: FilenameMetadata,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FilenameMetadata {
    pub opcode: Option<String>,
    pub gas_bucket: Option<String>,
    pub cache_strategy: Option<String>,
    pub account_mode: Option<String>,
    pub fork: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct TestQuery {
    pub exact: Option<String>,
    pub contains: Option<String>,
    pub pattern: Option<String>,
    pub opcode: Option<String>,
    pub gas_bucket: Option<String>,
    pub cache_strategy: Option<String>,
    pub account_mode: Option<String>,
    pub fork: Option<String>,
}

impl From<QueryArgs> for TestQuery {
    fn from(value: QueryArgs) -> Self {
        Self {
            exact: value.exact,
            contains: value.contains,
            pattern: value.pattern,
            opcode: value.opcode,
            gas_bucket: value.gas_bucket,
            cache_strategy: value.cache_strategy,
            account_mode: value.account_mode,
            fork: value.fork,
        }
    }
}

impl FixtureIndex {
    pub fn build(suite_id: &str, root: &Path) -> Result<Self> {
        let mut pre_run = Vec::new();
        let mut tests: BTreeMap<String, TestEntry> = BTreeMap::new();

        for entry in WalkDir::new(root).follow_links(false) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.into_path();
            if path.extension().and_then(|e| e.to_str()) != Some("txt") {
                continue;
            }
            let step = StepFile::from_path(root, &path)?;
            let rel = step.rel_path.replace('\\', "/");
            let basename = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            let rel_lower = rel.to_ascii_lowercase();

            if basename == "gas-bump.txt" || basename == "funding.txt" {
                pre_run.push(step);
                continue;
            }

            let name = basename;
            let test = tests.entry(name.clone()).or_insert_with(|| TestEntry {
                name: name.clone(),
                metadata: FilenameMetadata::parse(&name),
                ..Default::default()
            });

            if rel_lower.contains("/setup/") || rel_lower.starts_with("setup/") {
                test.setup = Some(step);
            } else if rel_lower.contains("/testing/")
                || rel_lower.starts_with("testing/")
                || rel_lower.contains("/test/")
                || rel_lower.starts_with("test/")
            {
                test.testing = Some(step);
            } else if rel_lower.contains("/cleanup/") || rel_lower.starts_with("cleanup/") {
                test.cleanup = Some(step);
            }
        }

        pre_run.sort_by(|a, b| {
            pre_run_sort_key(&a.name)
                .cmp(&pre_run_sort_key(&b.name))
                .then(a.rel_path.cmp(&b.rel_path))
        });

        Ok(Self {
            suite_id: suite_id.to_string(),
            root: root.to_path_buf(),
            generated_at: chrono::Utc::now().to_rfc3339(),
            pre_run,
            tests: tests.into_values().collect(),
        })
    }

    pub fn search(&self, query: &TestQuery) -> Result<Vec<&TestEntry>> {
        let pattern = match &query.pattern {
            Some(pattern) => Some(Regex::new(pattern).context("compiling --pattern")?),
            None => None,
        };
        Ok(self
            .tests
            .iter()
            .filter(|test| query.matches(test, pattern.as_ref()))
            .collect())
    }

    pub fn find_one(&self, name: &str) -> Result<Option<&TestEntry>> {
        let query = TestQuery {
            exact: Some(name.to_string()),
            ..Default::default()
        };
        Ok(self.search(&query)?.into_iter().next())
    }
}

impl StepFile {
    fn from_path(root: &Path, path: &Path) -> Result<Self> {
        let metadata = path
            .metadata()
            .with_context(|| format!("stat {}", path.display()))?;
        let rel_path = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        Ok(Self {
            name,
            rel_path,
            abs_path: path.to_path_buf(),
            bytes: metadata.len(),
        })
    }
}

impl TestEntry {
    pub fn summary_line(&self) -> String {
        let mut parts = vec![self.name.clone()];
        if let Some(opcode) = &self.metadata.opcode {
            parts.push(format!("opcode={opcode}"));
        }
        if let Some(gas) = &self.metadata.gas_bucket {
            parts.push(format!("gas={gas}"));
        }
        if let Some(cache) = &self.metadata.cache_strategy {
            parts.push(format!("cache={cache}"));
        }
        if let Some(account) = &self.metadata.account_mode {
            parts.push(format!("account={account}"));
        }
        parts.join(" ")
    }

    pub fn searchable_text(&self) -> String {
        let mut text = self.name.clone();
        for step in [&self.setup, &self.testing, &self.cleanup]
            .into_iter()
            .flatten()
        {
            text.push(' ');
            text.push_str(&step.rel_path);
        }
        text
    }
}

impl FilenameMetadata {
    pub fn parse(name: &str) -> Self {
        Self {
            opcode: extract_token(name, "opcode_").map(|value| strip_enum_prefix(&value)),
            gas_bucket: extract_gas_bucket(name),
            cache_strategy: extract_token(name, "cache_strategy_")
                .map(|value| strip_enum_prefix(&value)),
            account_mode: extract_token(name, "account_mode_")
                .map(|value| strip_enum_prefix(&value)),
            fork: extract_token(name, "fork_"),
        }
    }
}

impl TestQuery {
    fn matches(&self, test: &TestEntry, pattern: Option<&Regex>) -> bool {
        if let Some(exact) = &self.exact {
            let exact = exact.as_str();
            if test.name != exact
                && ![&test.setup, &test.testing, &test.cleanup]
                    .into_iter()
                    .flatten()
                    .any(|step| step.name == exact || step.rel_path == exact)
            {
                return false;
            }
        }
        let text = test.searchable_text();
        if let Some(contains) = &self.contains {
            if !text.contains(contains) {
                return false;
            }
        }
        if let Some(pattern) = pattern {
            if !pattern.is_match(&text) {
                return false;
            }
        }
        field_matches(&test.metadata.opcode, &self.opcode)
            && field_matches(&test.metadata.gas_bucket, &self.gas_bucket)
            && field_matches(&test.metadata.cache_strategy, &self.cache_strategy)
            && field_matches(&test.metadata.account_mode, &self.account_mode)
            && field_matches(&test.metadata.fork, &self.fork)
    }
}

fn field_matches(actual: &Option<String>, expected: &Option<String>) -> bool {
    match expected {
        None => true,
        Some(expected) => actual
            .as_ref()
            .map(|actual| normalize(actual) == normalize(expected))
            .unwrap_or(false),
    }
}

fn normalize(value: &str) -> String {
    strip_enum_prefix(value).to_ascii_lowercase()
}

fn strip_enum_prefix(value: &str) -> String {
    value.rsplit('.').next().unwrap_or(value).to_string()
}

fn extract_token(name: &str, key: &str) -> Option<String> {
    let start = name.find(key)? + key.len();
    let rest = &name[start..];
    let end = rest.find(['-', ']', '[', ',', ')']).unwrap_or(rest.len());
    let token = rest[..end].trim_end_matches(".txt");
    (!token.is_empty()).then(|| token.to_string())
}

fn extract_gas_bucket(name: &str) -> Option<String> {
    let mut rest = name;
    while let Some(idx) = rest.find("benchmark_") {
        let candidate = &rest[idx + "benchmark_".len()..];
        if candidate
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
        {
            let end = candidate
                .find(['-', ']', '[', ',', ')', '.'])
                .unwrap_or(candidate.len());
            return Some(candidate[..end].to_string());
        }
        rest = &candidate[1.min(candidate.len())..];
    }
    None
}

fn pre_run_sort_key(name: &str) -> usize {
    match name {
        "gas-bump.txt" => 0,
        "funding.txt" => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn parses_benchmarkoor_filename_metadata() {
        let meta = FilenameMetadata::parse("test_single_opcode.py__test_account_access[fork_Amsterdam-benchmark_test-opcode_CALL-value_sent_0-account_mode_AccountMode.NON_EXISTING_ACCOUNT-cache_strategy_CacheStrategy.NO_CACHE-benchmark_30M].txt");
        assert_eq!(meta.fork.as_deref(), Some("Amsterdam"));
        assert_eq!(meta.opcode.as_deref(), Some("CALL"));
        assert_eq!(meta.account_mode.as_deref(), Some("NON_EXISTING_ACCOUNT"));
        assert_eq!(meta.cache_strategy.as_deref(), Some("NO_CACHE"));
        assert_eq!(meta.gas_bucket.as_deref(), Some("30M"));
    }

    #[test]
    fn indexes_and_searches_fixture_layout() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path().join("gas-bump.txt"));
        write(tmp.path().join("funding.txt"));
        let name = "test_single_opcode.py__test_account_access[fork_Amsterdam-benchmark_test-opcode_CALL-account_mode_AccountMode.EXISTING_EOA-cache_strategy_CacheStrategy.NO_CACHE-benchmark_210M].txt";
        write(tmp.path().join("setup").join(name));
        write(tmp.path().join("testing").join(name));

        let index = FixtureIndex::build("suite", tmp.path()).unwrap();
        assert_eq!(index.pre_run.len(), 2);
        assert_eq!(index.tests.len(), 1);
        let query = TestQuery {
            opcode: Some("CALL".to_string()),
            gas_bucket: Some("210M".to_string()),
            cache_strategy: Some("NO_CACHE".to_string()),
            account_mode: Some("EXISTING_EOA".to_string()),
            fork: Some("Amsterdam".to_string()),
            ..Default::default()
        };
        assert_eq!(index.search(&query).unwrap().len(), 1);
    }

    fn write(path: PathBuf) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "{}\n").unwrap();
    }
}
