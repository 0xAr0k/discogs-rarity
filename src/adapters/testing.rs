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

use crate::domain::collection::Page;
use crate::domain::identity::{FolderId, PageSize, ReleaseId, Username};
use crate::domain::market::Sale;
use crate::error::{Error, Result};
use crate::ports::{Clock, CollectionSource, SaleSource, Sleeper};

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
