use std::{
    fs,
    fs::File,
    io,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::{index::FixtureIndex, suite::Suite};

#[derive(Clone, Debug)]
pub enum FixtureSource {
    Url(String),
    Release(String),
}

impl FromStr for FixtureSource {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Ok(Self::Url(s.to_string()))
    }
}

pub struct DownloadedFixture {
    pub fixture_root: PathBuf,
    pub index_path: PathBuf,
    pub index: FixtureIndex,
}

pub async fn download_and_index(
    cache_dir: &Path,
    suite: &Suite,
    source: FixtureSource,
    force: bool,
) -> Result<DownloadedFixture> {
    let source_ref = match source {
        FixtureSource::Url(url) => url,
        FixtureSource::Release(release) => discover_release_asset(&release, suite).await?,
    };

    if source_ref.is_empty() {
        anyhow::bail!("no fixture URL is known for suite {}", suite.id);
    }

    let suite_dir = cache_dir.join("fixtures").join(suite.slug());
    let archive_dir = suite_dir.join("archive");
    let fixture_root = suite_dir.join("extracted");
    fs::create_dir_all(&suite_dir).context("creating fixture cache")?;

    let local = PathBuf::from(&source_ref);
    let root_to_index = if local.exists() && local.is_dir() {
        local
    } else {
        fs::create_dir_all(&archive_dir).context("creating fixture archive cache")?;
        let archive_path = if local.exists() {
            local
        } else {
            let archive_path = archive_dir.join(cache_filename(&source_ref));
            if force || !archive_path.exists() {
                download_to_file(&source_ref, &archive_path).await?;
            }
            archive_path
        };

        if force || !fixture_root.exists() || fixture_root.read_dir()?.next().is_none() {
            if fixture_root.exists() {
                fs::remove_dir_all(&fixture_root).context("clearing extracted fixture cache")?;
            }
            fs::create_dir_all(&fixture_root).context("creating extracted fixture cache")?;
            extract_archive(&archive_path, &fixture_root)?;
        }
        fixture_root
    };

    let index = FixtureIndex::build(&suite.id, &root_to_index)?;
    let index_path = suite_dir.join("index.json");
    fs::write(&index_path, serde_json::to_vec_pretty(&index)?).context("writing fixture index")?;

    Ok(DownloadedFixture {
        fixture_root: root_to_index,
        index_path,
        index,
    })
}

pub fn load_index(cache_dir: &Path, suite: &Suite) -> Result<FixtureIndex> {
    let path = cache_dir
        .join("fixtures")
        .join(suite.slug())
        .join("index.json");
    let data = fs::read(&path).with_context(|| {
        format!(
            "reading fixture index {}; run `benchmarkoor-replay fixtures download` first",
            path.display()
        )
    })?;
    serde_json::from_slice(&data)
        .with_context(|| format!("parsing fixture index {}", path.display()))
}

pub fn cache_state(cache_dir: &Path, suite: &Suite) -> String {
    let suite_dir = cache_dir.join("fixtures").join(suite.slug());
    let index = suite_dir.join("index.json");
    if index.exists() {
        match load_index(cache_dir, suite) {
            Ok(index) => format!(
                "ready: {} tests at {}",
                index.tests.len(),
                suite_dir.display()
            ),
            Err(err) => format!("invalid: {err:#}"),
        }
    } else if suite_dir.exists() {
        format!("partial: {}", suite_dir.display())
    } else {
        "missing".to_string()
    }
}

async fn download_to_file(url: &str, path: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent("benchmarkoor-replay/0.1")
        .build()
        .context("building HTTP client")?;
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("requesting fixture archive {url}"))?
        .error_for_status()
        .with_context(|| format!("downloading fixture archive {url}"))?;

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
            "moving downloaded fixture archive {} to {}",
            tmp.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn cache_filename(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    let hash = hex::encode(&hasher.finalize()[..8]);
    let basename = url
        .split('/')
        .next_back()
        .filter(|name| !name.is_empty())
        .unwrap_or("fixture-archive");
    format!("{hash}-{basename}")
}

fn extract_archive(archive: &Path, dest: &Path) -> Result<()> {
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") || is_gzip(archive)? {
        let file = File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut tar = tar::Archive::new(decoder);
        tar.unpack(dest)
            .with_context(|| format!("extracting {} to {}", archive.display(), dest.display()))?;
    } else if name.ends_with(".tar.zst") || name.ends_with(".tzst") {
        let file = File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
        let decoder = zstd::stream::read::Decoder::new(file).context("creating zstd decoder")?;
        let mut tar = tar::Archive::new(decoder);
        tar.unpack(dest)
            .with_context(|| format!("extracting {} to {}", archive.display(), dest.display()))?;
    } else if name.ends_with(".zip") || is_zip(archive)? {
        let file = File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
        let mut zip = zip::ZipArchive::new(file).context("opening zip archive")?;
        zip.extract(dest)
            .with_context(|| format!("extracting zip to {}", dest.display()))?;
    } else {
        anyhow::bail!("unsupported fixture archive format: {}", archive.display());
    }
    Ok(())
}

fn is_gzip(path: &Path) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 2];
    let n = io::Read::read(&mut file, &mut magic)?;
    Ok(n == 2 && magic == [0x1f, 0x8b])
}

fn is_zip(path: &Path) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 4];
    let n = io::Read::read(&mut file, &mut magic)?;
    Ok(n == 4 && magic == [0x50, 0x4b, 0x03, 0x04])
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

async fn discover_release_asset(release_url: &str, suite: &Suite) -> Result<String> {
    let (owner, repo, tag) = parse_github_release_url(release_url)?;
    let api = format!("https://api.github.com/repos/{owner}/{repo}/releases/tags/{tag}");
    let release: GitHubRelease = reqwest::Client::builder()
        .user_agent("benchmarkoor-replay/0.1")
        .build()?
        .get(api)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("parsing GitHub release response")?;

    select_release_asset(release.assets, suite).ok_or_else(|| {
        anyhow::anyhow!(
            "no release asset matched suite {} in {}",
            suite.id,
            release_url
        )
    })
}

fn select_release_asset(assets: Vec<GitHubAsset>, suite: &Suite) -> Option<String> {
    let network = suite.network.to_ascii_lowercase();
    let test_type = suite.test_type.to_ascii_lowercase();
    assets
        .into_iter()
        .filter(|asset| asset.name.ends_with(".tar.gz") || asset.name.ends_with(".tgz"))
        .find(|asset| {
            let name = asset.name.to_ascii_lowercase();
            name.contains(&network) && name.contains(&test_type)
        })
        .map(|asset| asset.browser_download_url)
}

fn parse_github_release_url(release_url: &str) -> Result<(String, String, String)> {
    let parsed = url::Url::parse(release_url).context("parsing release URL")?;
    let mut segments = parsed
        .path_segments()
        .ok_or_else(|| anyhow::anyhow!("release URL has no path segments"))?;
    let owner = segments
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing owner in release URL"))?;
    let repo = segments
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing repo in release URL"))?;
    let releases = segments.next();
    let tag_keyword = segments.next();
    let tag = segments.next();
    if releases != Some("releases") || tag_keyword != Some("tag") {
        anyhow::bail!("expected GitHub release tag URL, got {release_url}");
    }
    let tag = tag.ok_or_else(|| anyhow::anyhow!("missing release tag in {release_url}"))?;
    Ok((owner.to_string(), repo.to_string(), tag.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn cache_filename_keeps_extension_hint() {
        let name = cache_filename("https://example.com/generated-tests-stateful-jochemnet.tar.gz");
        assert!(name.ends_with("generated-tests-stateful-jochemnet.tar.gz"));
    }

    #[test]
    fn parses_github_release_url() {
        let (owner, repo, tag) = parse_github_release_url(
            "https://github.com/NethermindEth/gas-benchmarks/releases/tag/amsterdam-repricings-v4.1.0",
        )
        .unwrap();
        assert_eq!(owner, "NethermindEth");
        assert_eq!(repo, "gas-benchmarks");
        assert_eq!(tag, "amsterdam-repricings-v4.1.0");
    }

    #[test]
    fn selects_suite_release_asset() {
        let suite = Suite::resolve(
            "jochemnet/24402727",
            "repricing",
            "amsterdam",
            "stateful",
            Path::new("/missing"),
        )
        .unwrap();
        let selected = select_release_asset(
            vec![
                GitHubAsset {
                    name: "generated-tests-stateful-perf-devnet-3.tar.gz".to_string(),
                    browser_download_url: "https://example.com/perf.tar.gz".to_string(),
                },
                GitHubAsset {
                    name: "generated-tests-stateful-jochemnet.tar.gz".to_string(),
                    browser_download_url: "https://example.com/jochemnet.tar.gz".to_string(),
                },
            ],
            &suite,
        );
        assert_eq!(
            selected.as_deref(),
            Some("https://example.com/jochemnet.tar.gz")
        );
    }
}
