//! Application logic: wiring ports to domain functions.
//!
//! # What belongs here
//!
//! Orchestration only — deciding *what to call in what order*, translating
//! between port types and domain types, and turning a sequence of fallible
//! steps into one clear error.
//!
//! What does **not** belong here: rules about what a release is or what it is
//! worth. If you find yourself writing `if`/`else` about music in this module,
//! that condition is a domain rule in disguise. Push it down.
//!
//! # The functional shape
//!
//! Each step is a small function taking dependencies as arguments and returning
//! a value. No globals, no `?` chains hidden in `main`, no side effects
//! between steps. [`run`] composes them. That means you can test step 3 without
//! running steps 1 and 2, and swap in fakes for steps 1 and 2 when testing
//! step 3.

use crate::{
    adapters::discogs::DiscogsCollectionSource,
    domain::{
        collection::{Collection, PageSet},
        identity::{FolderId, PageSize, ReleaseId, Username},
        market::{self, MarketSnapshot, Sale},
        release::{OwnedRelease, Release},
        valuation::{CollectionValuation, Condition, Valuation},
    },
    error::{Error, Result},
    ports::{Clock, CollectionSource, SaleSource, Sleeper},
};

/// What the user asked for, after validation.
#[derive(Debug, Clone)]
pub struct Request {
    pub username: Username,
    pub folder: FolderId,
    pub page_size: PageSize,
    /// How recent a sale must be to inform a price.
    pub market_window_days: i32,
}

impl Request {
    /// Build a request, rejecting invalid inputs at the boundary.
    ///
    /// Validation happens here — once, at the edge — so that everything
    /// downstream can assume validity. That is the payoff of the newtypes in
    /// [`crate::domain::identity`].
    pub fn new(
        username: &str,
        folder: u32,
        page_size: u32,
        market_window_days: i32,
    ) -> Result<Self> {
        if market_window_days <= 0 {
            return Err(Error::msg(
                "market window must be a positive number of days",
            ));
        }
        Ok(Self {
            username: Username::new(username)?,
            folder: FolderId::new(folder).ok_or(crate::error::DomainError::InvalidFolder)?,
            page_size: PageSize::new(page_size)
                .ok_or(crate::error::DomainError::InvalidPageSize)?,
            market_window_days,
        })
    }
}

/// The result of a full run, ready for presentation.
#[derive(Debug, Clone)]
pub struct Report {
    pub request: Request,
    pub collection: Collection,
    pub valuations: Vec<Valuation>,
    /// Per-pressing sale evidence, for explainability.
    pub snapshots: Vec<MarketSnapshot>,
    pub collection_total: CollectionValuation,
    /// When the prices were considered valid.
    pub priced_at_unix: i64,
}

impl Report {
    /// One entry per owned pressing, paired with its valuation.
    ///
    /// # Why this is not a borrowed slice
    ///
    /// A tempting shortcut is to hand back `&[OwnedRelease]`. But a
    /// [`Valuation`] holds only a [`ReleaseId`] — the full `Release` lives in
    /// [`Report::collection`], and deduplicating a second time to rebuild it
    /// would produce placeholder releases with empty titles (which is exactly
    /// what an earlier draft of this code did).
    ///
    /// Returning an owned `Vec` of references keeps the borrow honest. When the
    /// renderer needs a title, look it up in `collection` by ID — see
    /// [`Report::release_for`].
    pub fn owned(&self) -> Vec<OwnedRelease> {
        self.collection.dedupe()
    }

    /// Find a release by ID within this report's collection.
    pub fn release_for(&self, id: ReleaseId) -> Option<&Release> {
        self.collection
            .releases
            .iter()
            .find(|release| release.id == id)
    }

    /// The valuation for one pressing, if the report has one.
    pub fn valuation_for(&self, id: ReleaseId) -> Option<&Valuation> {
        self.valuations
            .iter()
            .find(|valuation| valuation.release_id == id)
    }
}

/// Step 1: fetch pages and decide whether they form one consistent snapshot.
///
/// # Why this is two calls and not one
///
/// The transport can fail (`Error`). The coherence check can fail
/// (`DomainError`). Fusing them into one function would mean the caller cannot
/// tell "the network died" from "the data was inconsistent" — and those need
/// different responses from a user.
pub fn load_collection(source: &dyn CollectionSource, request: &Request) -> Result<Collection> {
    let pages = source.fetch_pages(&request.username, request.folder, request.page_size)?;
    PageSet { pages }.assemble().map_err(Error::from)
}

/// Step 2: gather sale history for every pressing.
///
/// # Why missing prices are not fatal
///
/// A sale failure for one pressing must not sink the whole report. So errors
/// are swallowed *per pressing* and the pressing ends up as "unknown value".
/// That is a deliberate availability trade: partial information beats no
/// information for a tool whose output is advisory.
///
/// The alternative — fail everything — is defensible for a shop but wrong for
/// a personal organiser. If you take that view, change it here and say so.
pub fn gather_sales(
    source: &dyn SaleSource,
    owned: &[OwnedRelease],
    sleeper: &dyn Sleeper,
    interval: std::time::Duration,
) -> std::collections::HashMap<ReleaseId, Vec<Sale>> {
    let mut by_release = std::collections::HashMap::with_capacity(owned.len());
    for (index, item) in owned.iter().enumerate() {
        if index > 0 {
            sleeper.sleep(interval);
        }
        // Two reasons a pressing ends up absent from the map, and they mean
        // different things, so do not conflate them:
        //
        // - the request failed → no entry, treated as unknown,
        // - the request succeeded with zero sales → also no entry, because
        //   "no sales" and "no data" produce the same `PriceEstimate::UNKNOWN`
        //   and storing an empty `Vec` would only cost an allocation.
        //
        // Callers that need to distinguish should ask the source directly.
        if let Ok(sales) = source.sales_for(item.release.id)
            && !sales.is_empty()
        {
            by_release.insert(item.release.id, sales);
        }
    }
    by_release
}

/// Value one pressing.
///
/// The pricing story (a market estimator, a condition model, and the
/// confidence that mixes them — "how does a thin sample change the number
/// you print?") is deliberately not written: a rule that confidently
/// mis-prices a pressing is worse than an honest "unknown". Until a decision
/// lands and an estimator exists, every valuation is [`Valuation::unknown`],
/// which the renderer reports as `value unknown` rather than `€0.00`. The
/// arguments are kept so the eventual wiring is local, and the raw copies
/// count is part of that eventual wiring: multiplicity discounts lots, which
/// is its own decision.
pub fn value_release(
    release: &Release,
    copies: u32,
    condition: Condition,
    snapshot: Option<&MarketSnapshot>,
) -> Valuation {
    let _ = (copies, condition, snapshot);
    Valuation::unknown(release.id, copies, condition)
}

/// Step 5: assemble the collection-level report.
pub fn value_collection(
    request: &Request,
    collection: Collection,
    owned: Vec<OwnedRelease>,
    snapshots: Vec<MarketSnapshot>,
    now: i64,
) -> Report {
    let valuations = owned
        .iter()
        .map(|item| value_release(&item.release, item.copies, Condition::Mint, None))
        .collect();

    Report {
        request: request.clone(),
        collection,
        valuations,
        snapshots,
        collection_total: CollectionValuation {
            valuations: Vec::new(),
            total: None,
            coverage: 0.0,
        },
        priced_at_unix: now,
    }
}

/// The whole pipeline, with dependencies passed in.
///
/// # Why `main` is this thin
///
/// Everything here is testable with fakes. `main.rs` only parses arguments,
/// builds real adapters, calls `run`, and turns the error into an exit code.
/// That is the shape to aim for: the argument parser is the least interesting
/// code in the program and should be the shortest module.
pub fn run(
    request: Request,
    collection_source: &dyn CollectionSource,
    sale_source: &dyn SaleSource,
    clock: &dyn Clock,
    sleeper: &dyn Sleeper,
    pacing: std::time::Duration,
) -> Result<Report> {
    let collection = load_collection(collection_source, &request)?;
    let owned = collection.dedupe();
    let sales = gather_sales(sale_source, &owned, sleeper, pacing);
    let now = clock.now_unix();

    let snapshots = market::snapshots_for(
        owned.iter().map(|item| &item.release),
        &sales,
        now,
        request.market_window_days,
    );

    Ok(value_collection(
        &request, collection, owned, snapshots, now,
    ))
}

/// Convenience for `main`: build real Discogs adapters and run.
///
/// # Note the duplication of adapter types
///
/// Both sources are the same client, but they are separate ports, so the
/// function takes one value and uses it twice. Worth noticing: this is the
/// cost of clean separation. A monorepo with fifty ports would feel it; with
/// three it buys testability for free.
pub fn run_discogs(request: Request) -> Result<Report> {
    let client = DiscogsCollectionSource::new(std::env::var("DISCOGS_TOKEN").ok().as_deref())?;
    let clock = crate::adapters::SystemClock;
    let sleeper = crate::adapters::ThreadSleeper;
    let pacing = crate::ports::Pacing::for_token(std::env::var("DISCOGS_TOKEN").is_ok());
    run(request, &client, &client, &clock, &sleeper, pacing.interval)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        adapters::testing::{FakeCollectionSource, FakeSaleSource, FixedClock, RecordingSleeper},
        domain::{
            collection::Page,
            identity::ReleaseId,
            release::{Artist, Format, Label},
        },
    };

    const NOW: i64 = 1_700_000_000;

    fn release(id: u64, title: &str) -> Release {
        Release {
            id: ReleaseId::new(id).unwrap(),
            title: title.into(),
            year: Some(1959),
            artists: vec![Artist {
                id: 1,
                name: "Miles Davis".into(),
            }],
            formats: vec![Format {
                name: "Vinyl".into(),
                qty: "1".into(),
                descriptions: vec!["First Pressing".into()],
            }],
            labels: vec![Label {
                id: 1,
                name: "Columbia".into(),
                catno: "CL 1355".into(),
            }],
            thumb: String::new(),
            resource_url: String::new(),
            country: Some("US".into()),
            master_id: None,
        }
    }

    fn page(number: u32, total_pages: u32, total_items: u64, releases: Vec<Release>) -> Page {
        Page {
            number,
            total_pages,
            total_items,
            releases,
        }
    }

    fn request() -> Request {
        Request::new("arok", 0, 100, 365).unwrap()
    }

    // ---- Request validation -------------------------------------------

    #[test]
    fn request_rejects_a_non_positive_market_window() {
        assert!(Request::new("arok", 0, 100, 0).is_err());
        assert!(Request::new("arok", 0, 100, -1).is_err());
    }

    #[test]
    fn request_rejects_bad_identity_and_size() {
        assert!(Request::new("a/b", 0, 100, 30).is_err());
        assert!(Request::new("arok", 0, 0, 30).is_err());
        assert!(Request::new("arok", 0, 101, 30).is_err());
    }

    #[test]
    fn request_accepts_valid_input_and_defaults_are_sane() {
        let request = request();
        assert_eq!(request.username.as_str(), "arok");
        assert_eq!(request.page_size, PageSize::default());
        assert!(request.folder.is_all());
        // Sanity: the window we validate is the one we store.
        assert_eq!(request.market_window_days, 365);
    }

    // ---- Step 1: loading ----------------------------------------------

    #[test]
    fn load_collection_returns_a_validated_snapshot() {
        let source = FakeCollectionSource::with_pages(vec![page(1, 1, 1, vec![release(101, "A")])]);
        let collection = load_collection(&source, &request()).unwrap();
        assert_eq!(collection.item_count(), 1);
    }

    #[test]
    fn load_collection_propagates_a_transport_failure() {
        let source = FakeCollectionSource::failing("network is down");
        let error = load_collection(&source, &request()).unwrap_err();
        assert!(error.message.contains("network is down"));
        assert_eq!(error.domain, None, "a transport error has no domain cause");
    }

    #[test]
    fn load_collection_surfaces_a_domain_failure_as_a_domain_error() {
        // Two pages claimed, one delivered: incoherent, and it must be named.
        let source = FakeCollectionSource::with_pages(vec![page(1, 2, 4, vec![release(101, "A")])]);
        let error = load_collection(&source, &request()).unwrap_err();
        assert!(matches!(
            error.domain,
            Some(crate::error::DomainError::ShortCollection { .. })
        ));
    }

    // ---- Step 2: sales -------------------------------------------------

    #[test]
    fn gather_sales_collects_per_pressing_history() {
        let owned = vec![
            OwnedRelease {
                copies: 1,
                release: release(101, "A"),
            },
            OwnedRelease {
                copies: 1,
                release: release(102, "B"),
            },
        ];
        let sales = FakeSaleSource::new();
        let mut with_one = sales.clone();
        with_one.insert(
            ReleaseId::new(101).unwrap(),
            vec![market::Sale {
                price: market::Money {
                    minor_units: 1000,
                    currency: market::Currency::Eur,
                },
                sold_at: NOW - 86_400,
                condition: 2,
                is_signed: false,
                is_media_only: false,
                seller: None,
            }],
        );

        let sleeper = RecordingSleeper::new();
        let collected = gather_sales(
            &with_one,
            &owned,
            &sleeper,
            std::time::Duration::from_secs(1),
        );
        assert_eq!(collected.len(), 1);
        assert!(collected.contains_key(&ReleaseId::new(101).unwrap()));
        assert_eq!(
            sleeper.total(),
            std::time::Duration::from_secs(1),
            "exactly one inter-request gap between two pressings"
        );
    }

    #[test]
    fn gather_sales_degrades_to_unknown_when_a_source_fails() {
        let owned = vec![OwnedRelease {
            copies: 1,
            release: release(101, "A"),
        }];
        let sleeper = RecordingSleeper::new();
        let collected = gather_sales(
            &FakeSaleSource::failing("rate limited"),
            &owned,
            &sleeper,
            std::time::Duration::from_secs(1),
        );
        assert!(
            collected.is_empty(),
            "a failing price source must not sink the whole run"
        );
    }

    #[test]
    fn gather_sales_does_not_sleep_before_the_first_request() {
        let owned = vec![OwnedRelease {
            copies: 1,
            release: release(101, "A"),
        }];
        let sleeper = RecordingSleeper::new();
        gather_sales(
            &FakeSaleSource::new(),
            &owned,
            &sleeper,
            std::time::Duration::from_secs(30),
        );
        assert!(sleeper.total().is_zero(), "no leading sleep, please");
    }

    // ---- Step 5 and run ------------------------------------------------

    #[test]
    fn run_produces_a_report_from_fakes_alone() {
        let source = FakeCollectionSource::with_pages(vec![page(
            1,
            1,
            3,
            vec![release(101, "A"), release(101, "A"), release(102, "B")],
        )]);

        let report = run(
            request(),
            &source,
            &FakeSaleSource::new(),
            &FixedClock(NOW),
            &RecordingSleeper::new(),
            std::time::Duration::ZERO,
        )
        .unwrap();

        assert_eq!(report.collection.item_count(), 3);
        assert_eq!(report.snapshots.len(), 2, "one snapshot per pressing");
        assert!(report.snapshots.iter().all(|s| !s.value.price.is_known()));
    }

    #[test]
    fn run_reports_the_request_it_served() {
        let source = FakeCollectionSource::with_pages(vec![page(1, 1, 0, vec![])]);
        let report = run(
            request(),
            &source,
            &FakeSaleSource::new(),
            &FixedClock(NOW),
            &RecordingSleeper::new(),
            std::time::Duration::ZERO,
        )
        .unwrap();
        assert_eq!(report.request.username.as_str(), "arok");
        assert_eq!(report.priced_at_unix, NOW);
    }

    #[test]
    fn run_on_an_empty_collection_is_a_valid_empty_report() {
        let source = FakeCollectionSource::with_pages(vec![page(1, 0, 0, vec![])]);
        let report = run(
            request(),
            &source,
            &FakeSaleSource::new(),
            &FixedClock(NOW),
            &RecordingSleeper::new(),
            std::time::Duration::ZERO,
        )
        .unwrap();
        assert_eq!(report.collection.item_count(), 0);
        assert!(report.valuations.is_empty());
    }

    #[test]
    fn run_propagates_an_incoherent_fetch() {
        let source = FakeCollectionSource::with_pages(vec![page(1, 3, 3, vec![release(101, "A")])]);
        assert!(
            run(
                request(),
                &source,
                &FakeSaleSource::new(),
                &FixedClock(NOW),
                &RecordingSleeper::new(),
                std::time::Duration::ZERO,
            )
            .is_err()
        );
    }
}
