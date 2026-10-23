//! Test doubles for the ports.
//!
//! # Why the crate ships fakes
//!
//! Because the alternative is not "write fewer tests", it is "write fewer
//! *fast, specific* tests". A hand-rolled HTTP mock makes assertions about
//! status codes; a fake [`CollectionSource`] lets you assert about *policy* —
//! "the app requests 100-item pages", "the app paces between requests" — which
//! is where orchestration bugs live.
//!
//! A note on integrity: these fakes are for *your* tests. If you use one in
//! `main.rs`, the code still compiles, which is exactly why fakes belong in a
//! clearly-named module with no `#[cfg(test)]` on the *content* — they are
//! legitimate library, just not used by the app.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::domain::browse::{
    ArtistProfile, CatalogPage, EntityKind, LabelProfile, ReleaseDetail, SearchPage, WantEntry,
};
use crate::domain::collection::Page;
use crate::domain::identity::{FolderId, PageSize, ReleaseId, Username};
use crate::domain::market::Sale;
use crate::error::{Error, Result};
use crate::ports::{CatalogSource, Clock, CollectionSource, SaleSource, Sleeper, WantlistStore};

/// A [`CollectionSource`] that replays canned pages and records what it was
/// asked for.
#[derive(Debug, Clone, Default)]
pub struct FakeCollectionSource {
    pages: Vec<Page>,
    fail_with: Option<String>,
    /// Every `(username, folder, page_size)` this source was asked for.
    ///
    /// Recorded so tests can assert on the *requests*, not just the result.
    requests: Arc<Mutex<Vec<(String, FolderId, PageSize)>>>,
}

impl FakeCollectionSource {
    /// Replay `pages` verbatim.
    pub fn with_pages(pages: Vec<Page>) -> Self {
        Self {
            pages,
            fail_with: None,
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Fail every call with `message`.
    pub fn failing(message: impl Into<String>) -> Self {
        Self {
            pages: Vec::new(),
            fail_with: Some(message.into()),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Every request this source received, in order.
    pub fn requests(&self) -> Vec<(String, FolderId, PageSize)> {
        self.requests
            .lock()
            .expect("recording mutex poisoned")
            .clone()
    }
}

impl CollectionSource for FakeCollectionSource {
    fn fetch_pages(
        &self,
        username: &Username,
        folder: FolderId,
        page_size: PageSize,
    ) -> Result<Vec<Page>> {
        self.requests
            .lock()
            .expect("recording mutex poisoned")
            .push((username.as_str().to_owned(), folder, page_size));
        match &self.fail_with {
            Some(message) => Err(Error::msg(message.clone())),
            None => Ok(self.pages.clone()),
        }
    }
}

/// A [`SaleSource`] backed by a map, with an optional per-ID failure.
#[derive(Debug, Clone, Default)]
pub struct FakeSaleSource {
    sales: HashMap<ReleaseId, Vec<Sale>>,
    fail_all: Option<String>,
}

impl FakeSaleSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(release_id: ReleaseId, sales: Vec<Sale>) -> Self {
        let mut map = HashMap::new();
        map.insert(release_id, sales);
        Self {
            sales: map,
            fail_all: None,
        }
    }

    pub fn failing(message: impl Into<String>) -> Self {
        Self {
            sales: HashMap::new(),
            fail_all: Some(message.into()),
        }
    }

    pub fn insert(&mut self, release_id: ReleaseId, sales: Vec<Sale>) {
        self.sales.insert(release_id, sales);
    }
}

impl SaleSource for FakeSaleSource {
    fn sales_for(&self, release_id: ReleaseId) -> Result<Vec<Sale>> {
        if let Some(message) = &self.fail_all {
            return Err(Error::msg(message.clone()));
        }
        Ok(self.sales.get(&release_id).cloned().unwrap_or_default())
    }
}

/// A [`Sleeper`] that records requested durations and returns instantly.
#[derive(Debug, Clone, Default)]
pub struct RecordingSleeper {
    slept: Arc<Mutex<Vec<Duration>>>,
}

impl RecordingSleeper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every duration this sleeper was asked to wait for.
    pub fn slept_for(&self) -> Vec<Duration> {
        self.slept.lock().expect("recording mutex poisoned").clone()
    }

    /// Total time this sleeper *would* have waited.
    pub fn total(&self) -> Duration {
        self.slept
            .lock()
            .expect("recording mutex poisoned")
            .iter()
            .sum()
    }
}

impl Sleeper for RecordingSleeper {
    fn sleep(&self, duration: Duration) {
        self.slept
            .lock()
            .expect("recording mutex poisoned")
            .push(duration);
    }
}

/// A [`Clock`] frozen at a fixed instant.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub i64);

impl Clock for FixedClock {
    fn now_unix(&self) -> i64 {
        self.0
    }
}

impl Default for FixedClock {
    fn default() -> Self {
        Self(1_700_000_000)
    }
}

// ---------------------------------------------------------------------------
// Digger test doubles: a catalog fake that records calls, and a wantlist
// that lives only in memory.
// ---------------------------------------------------------------------------

/// One catalog lookup, recorded so tests can assert about *policy* (what was
/// asked, in what order) rather than about HTTP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogCall {
    Artist(u64),
    Label(u64),
    Detail(ReleaseId),
    ArtistReleases(u64, u32),
    LabelReleases(u64, u32),
    Search(String),
}

/// A [`CatalogSource`] serving canned answers and recording every call.
///
/// # Deliberately no network
///
/// The digger's orchestration bugs are "requested page 2 twice", "re-enqueued
/// the same detail". This fake makes those observable without a listener.
#[derive(Debug, Default)]
pub struct FakeCatalogSource {
    recorded: std::sync::Mutex<Vec<CatalogCall>>,
    artists: HashMap<u64, ArtistProfile>,
    labels: HashMap<u64, LabelProfile>,
    details: HashMap<ReleaseId, ReleaseDetail>,
    artist_releases: HashMap<u64, CatalogPage>,
    label_releases: HashMap<u64, CatalogPage>,
    search: SearchPage,
    fault: Option<String>,
}

impl FakeCatalogSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every lookup fails with this message.
    pub fn failing(message: impl Into<String>) -> Self {
        let mut source = Self::new();
        source.fault = Some(message.into());
        source
    }

    pub fn with_artist(mut self, id: u64, profile: ArtistProfile) -> Self {
        self.artists.insert(id, profile);
        self
    }

    pub fn with_label(mut self, id: u64, profile: LabelProfile) -> Self {
        self.labels.insert(id, profile);
        self
    }

    pub fn with_detail(mut self, id: ReleaseId, detail: ReleaseDetail) -> Self {
        self.details.insert(id, detail);
        self
    }

    /// One canned page per listing id (the fake answers every page with it).
    pub fn with_artist_page(mut self, id: u64, page: CatalogPage) -> Self {
        self.artist_releases.insert(id, page);
        self
    }

    pub fn with_label_page(mut self, id: u64, page: CatalogPage) -> Self {
        self.label_releases.insert(id, page);
        self
    }

    pub fn with_search(mut self, page: SearchPage) -> Self {
        self.search = page;
        self
    }

    /// The calls so far, most recent last.
    pub fn requests(&self) -> Vec<CatalogCall> {
        self.recorded
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }
}

impl CatalogSource for FakeCatalogSource {
    fn artist(&self, id: u64) -> Result<ArtistProfile> {
        self.recorded
            .lock()
            .map(|mut c| c.push(CatalogCall::Artist(id)))
            .ok();
        if let Some(fault) = &self.fault {
            return Err(Error::msg(fault.clone()));
        }
        self.artists
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::msg(format!("fake catalog: no artist {id} canned")))
    }

    fn label(&self, id: u64) -> Result<LabelProfile> {
        self.recorded
            .lock()
            .map(|mut c| c.push(CatalogCall::Label(id)))
            .ok();
        if let Some(fault) = &self.fault {
            return Err(Error::msg(fault.clone()));
        }
        self.labels
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::msg(format!("fake catalog: no label {id} canned")))
    }

    fn release_detail(&self, id: ReleaseId) -> Result<ReleaseDetail> {
        self.recorded
            .lock()
            .map(|mut c| c.push(CatalogCall::Detail(id)))
            .ok();
        if let Some(fault) = &self.fault {
            return Err(Error::msg(fault.clone()));
        }
        self.details
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::msg(format!("fake catalog: no detail for release {id}")))
    }

    fn artist_releases(&self, id: u64, page: u32) -> Result<CatalogPage> {
        self.recorded
            .lock()
            .map(|mut c| c.push(CatalogCall::ArtistReleases(id, page)))
            .ok();
        if let Some(fault) = &self.fault {
            return Err(Error::msg(fault.clone()));
        }
        self.artist_releases
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::msg(format!("fake catalog: no artist releases for {id}")))
    }

    fn label_releases(&self, id: u64, page: u32) -> Result<CatalogPage> {
        self.recorded
            .lock()
            .map(|mut c| c.push(CatalogCall::LabelReleases(id, page)))
            .ok();
        if let Some(fault) = &self.fault {
            return Err(Error::msg(fault.clone()));
        }
        self.label_releases
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::msg(format!("fake catalog: no label releases for {id}")))
    }

    fn search(&self, query: &str, _kind: Option<EntityKind>, _page: u32) -> Result<SearchPage> {
        self.recorded
            .lock()
            .map(|mut c| c.push(CatalogCall::Search(query.to_string())))
            .ok();
        if let Some(fault) = &self.fault {
            return Err(Error::msg(fault.clone()));
        }
        Ok(self.search.clone())
    }
}

/// An in-memory [`WantlistStore`], for update-loop tests that must not touch
/// a filesystem.
#[derive(Debug, Default)]
pub struct MemoryWantlist {
    entries: std::sync::Mutex<Vec<WantEntry>>,
}

impl MemoryWantlist {
    pub fn new() -> Self {
        Self::default()
    }
}

impl WantlistStore for MemoryWantlist {
    fn add(&self, entry: &WantEntry) -> Result<bool> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| Error::msg("wantlist poisoned"))?;
        if entries
            .iter()
            .any(|want| want.kind == entry.kind && want.id == entry.id)
        {
            return Ok(false);
        }
        entries.push(entry.clone());
        entries.sort_by(|a, b| b.added_at.cmp(&a.added_at).then(a.name.cmp(&b.name)));
        Ok(true)
    }

    fn remove(&self, kind: EntityKind, id: u64) -> Result<bool> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| Error::msg("wantlist poisoned"))?;
        let before = entries.len();
        entries.retain(|want| !(want.kind == kind && want.id == id));
        Ok(entries.len() != before)
    }

    fn list(&self) -> Result<Vec<WantEntry>> {
        Ok(self
            .entries
            .lock()
            .map_err(|_| Error::msg("wantlist poisoned"))?
            .clone())
    }

    fn contains(&self, kind: EntityKind, id: u64) -> Result<bool> {
        Ok(self
            .list()?
            .iter()
            .any(|want| want.kind == kind && want.id == id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_sleeper_never_actually_waits() {
        // If this test is slow, the fake is wrong somewhere.
        let sleeper = RecordingSleeper::new();
        sleeper.sleep(Duration::from_secs(600));
        sleeper.sleep(Duration::from_secs(30));
        assert_eq!(sleeper.total(), Duration::from_secs(630));
    }

    #[test]
    fn fake_collection_source_records_its_requests() {
        let source = FakeCollectionSource::with_pages(vec![]);
        let username = Username::new("arok").unwrap();
        let _ = source.fetch_pages(&username, FolderId::ALL, PageSize::default());
        let requests = source.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "arok");
    }

    #[test]
    fn fake_sale_source_distinguishes_empty_from_error() {
        let empty = FakeSaleSource::new();
        assert!(
            empty
                .sales_for(ReleaseId::new(1).unwrap())
                .unwrap()
                .is_empty()
        );

        let broken = FakeSaleSource::failing("rate limited");
        assert!(broken.sales_for(ReleaseId::new(1).unwrap()).is_err());
    }

    #[test]
    fn fixed_clock_does_not_move() {
        let clock = FixedClock(123);
        assert_eq!(clock.now_unix(), 123);
        assert_eq!(clock.now_unix(), 123);
    }
}
