//! The `digger` binary: env in, ports wired, terminal owned, exit code out.
//!
//! # How short this file should be
//!
//! The same contract as the collection CLI's `main.rs` (see its doc): parse
//! less, wire, hand to the runtime. If this grows past ~60 lines, a decision
//! wandered out of its module.
//!
//! # The exit-code contract
//!
//! | Code | Meaning |
//! |------|---------|
//! | 0 | the user quit the digger |
//! | 1 | failure, message on stderr |
//!
//! # Environment
//!
//! - `DISCOGS_TOKEN` — optional; without it id-browsing works anonymously
//!   (slower) and search refuses with a message naming this variable.
//!   It may also sit in a gitignored `.env` next to the manifest (loaded
//!   first, real environment wins).
//! - `XDG_DATA_HOME` (else `$HOME/.local/share`) — where the wantlist file
//!   lives: `<data>/diggers-rarity/wantlist.json`. On Windows, `%APPDATA%`
//!   plays the same role.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use diggers_rarity::{
    adapters::{DiscogsCatalogSource, JsonWantlist},
    tui::runtime,
};

fn main() -> ExitCode {
    match dig() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn dig() -> Result<(), String> {
    // An empty-but-set token is not "anonymous", it is a typo; refuse it at
    // the doorway the same way the collection CLI does.
    //
    // `.env` is read first so the token can live in one gitignored file
    // (`DISCOGS_TOKEN=...`). A missing file is ordinary — an *unreadable*
    // one is rare, and the TUI has no stderr to explain before the master
    // takes over, so either way we continue and the token simply stays empty.
    let _ = dotenvy::dotenv();

    let token = std::env::var("DISCOGS_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty());

    // The ports are shared: enrichment fetch threads use the same adapters.
    let api = Arc::new(DiscogsCatalogSource::new(token.as_deref()).map_err(|e| e.to_string())?);
    let store = Arc::new(JsonWantlist::at(wantlist_file()?));
    runtime::run(api, store, Arc::new(diggers_rarity::adapters::SystemClock))
}

/// `<data>/diggers-rarity/wantlist.json`, created on first write.
fn wantlist_file() -> Result<PathBuf, String> {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(fallback_home)
        .ok_or_else(|| "cannot find a data directory: set HOME or XDG_DATA_HOME".to_string())?;
    Ok(base.join("diggers-rarity").join("wantlist.json"))
}

fn fallback_home() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(|home| PathBuf::from(home).join("diggers-rarity-data"))
    } else {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
    }
}
