pub mod baseline;
pub mod cli;
pub mod fixtures;
pub mod index;
pub mod jsonrpc;
pub mod replay;
pub mod schelk;
pub mod snapshot;
pub mod status;
pub mod suite;

use std::path::PathBuf;

pub fn default_cache_dir() -> PathBuf {
    let root = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("."));
    let current = root.join("benchmarkoor-replay");
    let legacy = root.join("benchreplay");
    if !current.exists() && legacy.exists() {
        legacy
    } else {
        current
    }
}
