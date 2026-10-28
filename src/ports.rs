//! Ports: the traits the application speaks.
//!
//! # Ports and adapters (hexagonal architecture)
//!
//! The core idea is that the interesting logic should not depend on the messy
//! outside world. So we describe the outside world as a set of small traits
//! written in *domain* vocabulary, and push all the actual machinery behind
//! them.
//!
//! ```text
//!        ┌─────────────────────────────────────────┐
//!        │              app (orchestration)        │
//!        │  fetch_all → domain::PageSet::assemble  │
//!        └───────────────┬─────────────────────────┘
//!                        │ depends only on traits
//!        ┌───────────────▼─────────────────────────┐
//!        │   ports::CollectionSource              │
//!        │   ports::SaleSource                    │
//!        │   ports::Sleeper                       │
//!        └───────────────▲─────────────────────────┘
//!                        │ implemented by
//!        ┌───────────────┴─────────────────────────┐
//!        │  adapters::discogs   adapters::system   │
//!        │  (reqwest)           (std::thread::sleep)│
//!        └─────────────────────────────────────────┘
//! ```
//!
//! # Why `&self` and not `&mut self`
//!
//! No port mutates anything. That is not style — it means the app can hold
//! several sources at once, share them across calls, and (in tests) hand in a
//! source you built in two lines. `&mut self` would only be needed if fetching
//! advanced internal state, which for HTTP does not.
//!
//! # Why `Sleeper` exists
//!
//! Rate-limit pacing is the clearest example of logic you do not want welded to
//! a mechanism. `thread::sleep` in the middle of business logic means any test
//! that hits the pacing path waits 60 real seconds. Behind a trait, the test
//! records the requested durations and returns instantly — and the *policy*
//! (how long to wait) stays in the app where you can read it.

use std::time::Duration;

use crate::domain::browse::{
    ArtistProfile, CatalogPage, EntityKind, LabelProfile, ReleaseDetail, SearchPage, WantEntry,
};
use crate::domain::collection::Page;
use crate::domain::identity::{FolderId, PageSize, ReleaseId, Username};
use crate::domain::market::Sale;
use crate::error::Result;

/// A source of collection pages.
///
/// # Contract
///
/// An implementation must return **every** page, in ascending page order, each
/// carrying the server's own pagination claims. It must *not* filter,
/// reorder, or repair anything — coherence checking belongs in
/// [`crate::domain::collection::PageSet::assemble`], where it can be tested
/// without a network.
///
/// Returning `Result` lets an adapter fail on transport or authorization
/// problems. Note what is *not* in the error type: no `Page`, no partial
/// data. Partial results are handled by refusing, not by recovering.
pub trait CollectionSource {
    /// Fetch pages `1..=total_pages` for `username` in `folder`.
    fn fetch_pages(
        &self,
        username: &Username,
        folder: FolderId,
        page_size: PageSize,
    ) -> Result<Vec<Page>>;
}

/// A source of sale history for a pressing.
///
/// Separate from [`CollectionSource`] on purpose: they are different endpoints,
/// they rate-limit differently, and sale history is *optional* — a missing
/// price source should degrade to "unknown value", not fail the whole run.
pub trait SaleSource {
    /// Recent sales for one pressing, most recent first. An empty vector means
    /// "no sales", which is different from an error.
    fn sales_for(&self, release_id: ReleaseId) -> Result<Vec<Sale>>;
}

/// The ability to wait.
///
/// Exists so pacing and retry policy are testable and so nothing in the app
/// sleeps a real thread during a test run.
pub trait Sleeper {
    /// Wait for `duration`.
    fn sleep(&self, duration: Duration);
}

/// Wall-clock time, injected rather than read from `SystemTime` directly.
///
/// # Why
///
/// Anything that reads the clock is a function of hidden state, and you cannot
/// test a pricing window without controlling time. Passing `now` as a
/// `i64` timestamp keeps the market logic a pure function of its inputs.
pub trait Clock {
    /// Unix seconds since the epoch.
    fn now_unix(&self) -> i64;
}

/// A source of Discogs database lookups for the digger: artists, labels,
/// release details, search.
///
/// # Contract
///
/// Each method maps one Discogs endpoint, one page at a time; the adapter
/// translates and paces but does not filter or interpret. Vinyl filtering
/// ([`crate::domain::browse::is_vinyl_format`]) happens in the adapter's row
/// mapping, since it is a *display* decision about the table the digger
/// asked for — see `docs/DISCOGS.md` § endpoints for the full list and the
/// quirks en route.
///
/// Separate from [`CollectionSource`] on purpose: different endpoints, and a
/// different app surface. The collection flow stays untouched.
pub trait CatalogSource {
    /// Who recorded this music.
    fn artist(&self, id: u64) -> Result<ArtistProfile>;
    /// The label, and who runs it.
    fn label(&self, id: u64) -> Result<LabelProfile>;
    /// Everything the release endpoint says, in USD where money appears.
    fn release_detail(&self, id: ReleaseId) -> Result<ReleaseDetail>;
    /// One page of the artist's releases, vinyl only, newest year first.
    fn artist_releases(&self, id: u64, page: u32) -> Result<CatalogPage>;
    /// One page of the label's releases, vinyl only.
    fn label_releases(&self, id: u64, page: u32) -> Result<CatalogPage>;
    /// Name search. Requires a token upstream; without one the adapter
    /// returns an error the UI can name.
    fn search(&self, query: &str, kind: Option<EntityKind>, page: u32) -> Result<SearchPage>;
    /// The release's sale rows — condition, price, seller — flat, as the
    /// release sheet prints them. Unreadable rows are dropped, not a failure:
    /// one bad datum must not blank a page of evidence.
    fn release_sales(&self, id: ReleaseId) -> Result<Vec<Sale>>;
}

/// The digger's local wantlist: the only truth about what to look for.
///
/// # Why this is a *file-backed* port and not memory-only
///
/// Entries survive restarts by definition — a wantlist that vanishes when the
/// terminal closes is a note that was never written. `&self` throughout:
/// the store mutates only its own file, never anything the caller sees.
pub trait WantlistStore {
    /// Adds an entry. `Ok(true)` when it was new; `Ok(false)` when it already
    /// existed (the stored name is kept — first answer wins).
    fn add(&self, entry: &WantEntry) -> Result<bool>;
    /// Removes an entry, `Ok(true)` when something was removed.
    fn remove(&self, kind: EntityKind, id: u64) -> Result<bool>;
    /// Every entry, newest first.
    fn list(&self) -> Result<Vec<WantEntry>>;
    /// Whether `(kind, id)` is already wanted.
    fn contains(&self, kind: EntityKind, id: u64) -> Result<bool>;
}

/// Convenience: rate-limit pacing policy, stated in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pacing {
    /// Floor applied to a server-provided `Retry-After`: never wait less
    /// than this, so a one-second header cannot cause a hot loop. (The
    /// request budget itself is a token bucket — see `cap` — not a gap.)
    pub interval: Duration,
    /// Longest we are willing to wait on a rate-limit response.
    pub max_backoff: Duration,
    /// The wire's documented request budget per minute: 60 with a token,
    /// 25 without. The adapter's pacing gate spends this as tokens —
    /// bursts up to its capacity, then refills at cap/minute.
    pub cap: u32,
}

impl Pacing {
    /// Pacing for an authenticated client: 60 requests/min is the documented
    /// limit, so stay under it.
    pub const AUTHENTICATED: Self = Self {
        interval: Duration::from_millis(1100),
        max_backoff: Duration::from_secs(300),
        cap: 60,
    };

    /// Pacing for an anonymous client: 25 requests/min.
    pub const ANONYMOUS: Self = Self {
        interval: Duration::from_millis(2500),
        max_backoff: Duration::from_secs(300),
        cap: 25,
    };

    /// Pick pacing based on whether a token is present.
    pub const fn for_token(has_token: bool) -> Self {
        if has_token {
            Self::AUTHENTICATED
        } else {
            Self::ANONYMOUS
        }
    }

    /// How long to wait after a `Retry-After: N` response.
    ///
    /// # The rule
    ///
    /// Honour the server's number, but never wait less than `interval` (it
    /// would just be rate-limited again), and never wait more than
    /// `max_backoff` — a user who gets told to wait 40 minutes has been told
    /// no. Refusing is better than freezing.
    pub fn backoff_for(self, retry_after: Option<Duration>) -> Duration {
        retry_after.unwrap_or(self.interval).max(self.interval)
    }

    /// Whether `delay` is acceptable at all.
    pub fn accepts(self, delay: Duration) -> bool {
        delay <= self.max_backoff
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacing_differs_by_authentication() {
        assert!(Pacing::ANONYMOUS.interval > Pacing::AUTHENTICATED.interval);
        assert_eq!(Pacing::for_token(true), Pacing::AUTHENTICATED);
        assert_eq!(Pacing::for_token(false), Pacing::ANONYMOUS);
    }

    #[test]
    fn backoff_honours_the_server_but_respects_the_minimum_interval() {
        let pacing = Pacing::AUTHENTICATED;
        assert_eq!(
            pacing.backoff_for(Some(Duration::from_secs(30))),
            Duration::from_secs(30)
        );
        assert_eq!(
            pacing.backoff_for(Some(Duration::from_millis(10))),
            pacing.interval,
            "a too-short Retry-After must be raised to the interval"
        );
        assert_eq!(pacing.backoff_for(None), pacing.interval);
    }

    #[test]
    fn absurd_backoffs_are_refused_rather_than_awaited() {
        let pacing = Pacing::AUTHENTICATED;
        assert!(!pacing.accepts(Duration::from_secs(600)));
        assert!(pacing.accepts(Duration::from_secs(300)));
    }
}
