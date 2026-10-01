//! Integration tests: exercise the crate through its public API only.
//!
//! # Why these live in `tests/` and not `src/`
//!
//! A unit test inside `src/domain/market.rs` can reach private items and
//! `crate::…` paths. That makes it easy to accidentally depend on something
//! internal, and it never proves the public surface is coherent.
//!
//! A test here can only `use diggers_rarity::…`. If it compiles, the public
//! API is genuinely usable from outside — which is exactly the property you
//! care about when the crate grows, or when you extract a module.
//!
//! # What to test here versus in `src/`
//!
//! - **Pure rules** (median, rarity, coherence) → unit tests in their module.
//!   They are cheap, precise, and have a clear home.
//! - **Wiring** (does the pipeline hold together, does the JSON contract hold)
//!   → here.
//!
//! Do not duplicate. A test that exists in both places will drift.

use diggers_rarity::{
    adapters::testing::{FakeCollectionSource, FakeSaleSource, FixedClock, RecordingSleeper},
    app::{self, Report, Request},
    domain::{
        collection::Page,
        identity::{FolderId, PageSize, ReleaseId, Username},
        market::{Currency, Money, Sale},
        release::{Artist, Format as MediaFormat, Label, Release},
    },
    ui::Format as OutputFormat,
};

/// A fixed instant, so every assertion about "recent" is deterministic.
const NOW: i64 = 1_700_000_000;

/// Build a release with only the fields a test cares about set.
fn release(id: u64, title: &str) -> Release {
    Release {
        id: ReleaseId::new(id).expect("test id must be valid"),
        title: title.to_owned(),
        year: Some(1959),
        artists: vec![Artist {
            id: 1,
            name: "Miles Davis".to_owned(),
        }],
        formats: vec![MediaFormat {
            name: "Vinyl".to_owned(),
            qty: "1".to_owned(),
            descriptions: vec!["First Pressing".to_owned()],
        }],
        labels: vec![Label {
            id: 1,
            name: "Columbia".to_owned(),
            catno: "CL 1355".to_owned(),
        }],
        thumb: String::new(),
        resource_url: String::new(),
        country: Some("US".to_owned()),
        master_id: None,
    }
}

/// Wrap releases into a coherent single-page source.
fn source_for(releases: Vec<Release>) -> FakeCollectionSource {
    let items = releases.len() as u64;
    FakeCollectionSource::with_pages(vec![Page {
        number: 1,
        total_pages: 1,
        total_items: items,
        releases,
    }])
}

fn request() -> Request {
    Request::new("arok", 0, 100, 365).expect("test request must be valid")
}

/// Run the pipeline with no sale data at all.
fn run_without_sales(releases: Vec<Release>) -> Report {
    app::run(
        request(),
        &source_for(releases),
        &FakeSaleSource::new(),
        &FixedClock(NOW),
        &RecordingSleeper::new(),
        std::time::Duration::ZERO,
    )
    .expect("run should succeed")
}

// ---------------------------------------------------------------------------
// Layer boundaries
// ---------------------------------------------------------------------------

/// The domain must be usable with no adapter in sight.
///
/// This test compiles only if `domain` depends on nothing outside itself and
/// the identity types. That is the property the whole layout exists to protect:
/// if someone adds `use crate::adapters::…` to the domain, this file stops
/// building.
#[test]
fn domain_stands_alone() {
    let id = ReleaseId::new(42).unwrap();
    assert_eq!(id.get(), 42);
    assert_eq!(FolderId::ALL.to_string(), "all");
    assert_eq!(PageSize::new(100).unwrap().get(), 100);
    assert_eq!(Username::new("arok").unwrap().as_str(), "arok");
}

/// Ports are object-safe, so the app can take any implementation.
///
/// `&dyn CollectionSource` in every `app::` signature means a fake and the real
/// client are interchangeable. This test proves the fakes satisfy the traits.
#[test]
fn fakes_satisfy_the_ports() {
    let collection: &dyn diggers_rarity::ports::CollectionSource =
        &FakeCollectionSource::with_pages(vec![]);
    let sales: &dyn diggers_rarity::ports::SaleSource = &FakeSaleSource::new();
    let sleeper: &dyn diggers_rarity::ports::Sleeper = &RecordingSleeper::new();
    let clock: &dyn diggers_rarity::ports::Clock = &FixedClock(NOW);

    // All four are usable through the trait objects the app actually takes.
    assert_eq!(clock.now_unix(), NOW);
    let _ = collection;
    let _ = sales;
    let _ = sleeper;
}

// ---------------------------------------------------------------------------
// Behaviour: the pipeline holds together
// ---------------------------------------------------------------------------

#[test]
fn a_fetch_of_duplicate_copies_reports_one_pressing_and_two_copies() {
    let report = run_without_sales(vec![
        release(101, "Kind of Blue"),
        release(101, "Kind of Blue"),
        release(102, "Bitches Brew"),
    ]);

    assert_eq!(report.collection.item_count(), 3, "three entries fetched");
    assert_eq!(report.valuations.len(), 2, "two distinct pressings");
    assert_eq!(
        report
            .valuation_for(ReleaseId::new(101).unwrap())
            .unwrap()
            .copies,
        2
    );
}

#[test]
fn an_incoherent_fetch_fails_rather_than_returning_partial_data() {
    // The source claims three pages but delivers one.
    let source = FakeCollectionSource::with_pages(vec![Page {
        number: 1,
        total_pages: 3,
        total_items: 300,
        releases: vec![release(101, "Only page we got")],
    }]);

    let error = app::run(
        request(),
        &source,
        &FakeSaleSource::new(),
        &FixedClock(NOW),
        &RecordingSleeper::new(),
        std::time::Duration::ZERO,
    )
    .expect_err("a truncated fetch must not be reported as a collection");

    assert!(
        error.domain.is_some(),
        "the failure should carry a domain cause, not just a sentence"
    );
}

#[test]
fn an_empty_collection_is_a_valid_answer_not_an_error() {
    let report = run_without_sales(vec![]);
    assert_eq!(report.collection.item_count(), 0);
    assert!(report.valuations.is_empty());
}

#[test]
fn a_price_failure_degrades_to_unknown_instead_of_failing_the_run() {
    // The whole point of separating CollectionSource from SaleSource.
    let report = app::run(
        request(),
        &source_for(vec![release(101, "Kind of Blue")]),
        &FakeSaleSource::failing("rate limited"),
        &FixedClock(NOW),
        &RecordingSleeper::new(),
        std::time::Duration::ZERO,
    )
    .expect("a price outage must not sink a collection fetch");

    assert_eq!(report.valuations.len(), 1);
    assert!(
        report.valuations[0].value.is_none(),
        "an unreachable price source means unknown, not worthless"
    );
}

#[test]
fn sale_lookups_are_paced_between_requests_but_not_before_the_first() {
    let mut sales = FakeSaleSource::new();
    for id in [101, 102, 103] {
        sales.insert(
            ReleaseId::new(id).unwrap(),
            vec![Sale {
                price: Money {
                    minor_units: 1000,
                    currency: Currency::Eur,
                },
                sold_at: NOW - 86_400,
                condition: 2,
                is_signed: false,
                is_media_only: false,
            }],
        );
    }

    // Calls `gather_sales` directly rather than the whole pipeline, so this
    // policy assertion keeps passing while exercise 2d is still a `todo!`.
    let owned = app::load_collection(
        &source_for(vec![
            release(101, "A"),
            release(102, "B"),
            release(103, "C"),
        ]),
        &request(),
    )
    .unwrap()
    .dedupe();

    let sleeper = RecordingSleeper::new();
    let collected = app::gather_sales(&sales, &owned, &sleeper, std::time::Duration::from_secs(2));

    assert_eq!(collected.len(), 3);
    assert_eq!(
        sleeper.total(),
        std::time::Duration::from_secs(4),
        "three pressings means two gaps between requests, never a leading sleep"
    );
}

// ---------------------------------------------------------------------------
// The output contract
// ---------------------------------------------------------------------------

#[test]
fn json_output_is_parseable_and_never_reports_unknown_as_zero() {
    let report = run_without_sales(vec![release(101, "Kind of Blue")]);
    let parsed: serde_json::Value =
        serde_json::from_str(&OutputFormat::Json.render(&report)).expect("valid JSON");

    assert_eq!(parsed["username"], "arok");
    assert_eq!(parsed["unique_pressings"], 1);
    assert_eq!(parsed["valuations"][0]["value"], serde_json::Value::Null);
    assert!(
        parsed.get("coverage").is_some(),
        "coverage must accompany any total"
    );
}

#[test]
fn text_output_never_prints_a_zero_for_an_unknown_value() {
    let report = run_without_sales(vec![release(101, "Kind of Blue")]);
    let text = OutputFormat::Text.render(&report);

    assert!(text.contains("unknown"));
    assert!(
        !text.contains("€0"),
        "a zero reads as worthless, which is a different claim than unknown"
    );
}

#[test]
fn a_report_can_look_up_its_own_releases() {
    let report = run_without_sales(vec![release(101, "Kind of Blue")]);
    let found = report.release_for(ReleaseId::new(101).unwrap()).unwrap();
    assert_eq!(found.title, "Kind of Blue");
    assert!(report.release_for(ReleaseId::new(999).unwrap()).is_none());
}

// ---------------------------------------------------------------------------
// The trait surface stays honest
// ---------------------------------------------------------------------------

/// A third-party port implementation, written the way a user of this crate
/// would write one.
///
/// This is the strongest architectural test in the crate: it proves a new data
/// source can be added without touching `domain`, `app`, or `ui`. If it fails
/// to compile, a trait has picked up a private type in its signature.
#[test]
fn an_outside_source_can_be_added_without_touching_the_core() {
    struct CannedSource {
        pages: Vec<Page>,
    }

    impl diggers_rarity::ports::CollectionSource for CannedSource {
        fn fetch_pages(
            &self,
            _username: &Username,
            _folder: FolderId,
            _page_size: PageSize,
        ) -> diggers_rarity::error::Result<Vec<Page>> {
            Ok(self.pages.clone())
        }
    }

    let source = CannedSource {
        pages: vec![Page {
            number: 1,
            total_pages: 1,
            total_items: 1,
            releases: vec![release(101, "From outside")],
        }],
    };

    let report = app::run(
        request(),
        &source,
        &FakeSaleSource::new(),
        &FixedClock(NOW),
        &RecordingSleeper::new(),
        std::time::Duration::ZERO,
    )
    .unwrap();

    assert_eq!(report.collection.item_count(), 1);
}
