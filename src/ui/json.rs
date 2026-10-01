//! The machine-readable renderer.
//!
//! # Contract
//!
//! The JSON is a **normalized subset** of what this app knows, not a backup of
//! an API response. Two consequences worth stating:
//!
//! 1. Unknown fields do not appear. Adding a field upstream does not silently
//!    change your output.
//! 2. Unknown *values* appear as `null`, never as `0` or `""`. A consumer can
//!    tell "we have no price" from "the price is nothing".
//!
//! # `#[serde(skip_serializing_if)]`
//!
//! Optional fields are omitted when empty. It keeps the output small and makes
//! the common case readable, at the cost of consumers that must handle a
//! missing key. If you would rather every key always be present, drop the
//! attribute — a real decision, not a style rule.

use serde::Serialize;

use crate::{
    app::Report,
    domain::{
        market::{MarketSnapshot, Money, Sale},
        rarity::Rarity,
        valuation::Valuation as DomainValuation,
    },
    error::Result,
};

/// The top-level document.
#[derive(Debug, Serialize)]
pub struct Document<'a> {
    username: &'a str,
    folder_id: u32,
    /// Entries fetched, duplicate copies included.
    collection_items: u64,
    /// Pressings after deduplication.
    unique_pressings: usize,
    /// Total physical units owned.
    total_units: u32,
    /// Unix seconds the prices were considered valid for.
    priced_at: i64,
    /// Valuation of the whole collection. `None` when nothing could be priced.
    #[serde(skip_serializing_if = "Option::is_none")]
    total_value: Option<AmountView>,
    /// Fraction of pressings that received a value, `0.0..=1.0`.
    ///
    /// Always present, even when `total_value` is null. Reading a total without
    /// its coverage is how a partial estimate gets mistaken for a full one.
    coverage: f64,
    valuations: Vec<ValuationView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ValuationView {
    release_id: u64,
    copies: u32,
    condition: String,
    /// `null` when rarity could not be assessed.
    rarity: Option<RarityView>,
    /// `null` when no price could be estimated.
    value: Option<RangeView>,
    /// `0.0..=1.0`.
    confidence: f64,
    /// Raw sale evidence, so a consumer can re-derive the estimate.
    ///
    /// Serialized in its own `Vec<…>` view rather than by reference: the view
    /// is owned, so the borrow of the report ends at render time.
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    sales: Vec<SaleView>,
}

/// A single amount, in minor units plus a display string.
///
/// # Why the domain `Money` is not serialized directly
///
/// `Money` is a domain type and knows nothing about JSON. If it derived
/// `Serialize`, its wire shape would be frozen by whatever we happen to
/// serialize first. Projecting into a view keeps the wire format a decision of
/// this module.
#[derive(Debug, Serialize)]
struct AmountView {
    /// Minor units (cents). Integer, so consumers never inherit float money.
    minor: i64,
    /// ISO 4217 code.
    currency: &'static str,
    /// Human-readable, e.g. `€19.99`.
    display: String,
}

impl From<Money> for AmountView {
    fn from(money: Money) -> Self {
        Self {
            minor: money.minor_units,
            currency: currency_code(money.currency),
            display: money.to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
struct RarityView {
    /// `0.0` = common, `1.0` = as rare as we can measure.
    score: f64,
    /// Coarse band for filtering.
    band: &'static str,
    /// How many signals were actually measured.
    measured_signals: usize,
    /// Which signals, and how strongly.
    components: Vec<ComponentView>,
}

#[derive(Debug, Serialize)]
struct ComponentView {
    name: &'static str,
    magnitude: f64,
}

#[derive(Debug, Serialize)]
struct RangeView {
    /// Minor units (cents) of the low end.
    low_minor: i64,
    /// Minor units (cents) of the high end.
    high_minor: i64,
    currency: &'static str,
    /// The same range as a human string, e.g. `€35.00`.
    display: String,
}

#[derive(Debug, Serialize)]
struct SaleView {
    /// Minor units. Integer so consumers do not inherit float money.
    price_minor: i64,
    currency: &'static str,
    sold_at: i64,
    condition: u8,
    signed: bool,
    /// Whether this sale was excluded from the estimate.
    excluded: bool,
}

/// Render a report as pretty-printed JSON.
///
/// # TODO(you) — exercise 6
///
/// Make failure visible. `serde_json::to_string_pretty` on data you built
/// yourself basically cannot fail, but `unwrap()` here would panic on a user
/// with an empty terminal. Return `Result<String>` and let `main` decide what
/// to print. The doc comment on this function already claims a `Result`; make
/// the signature match.
pub fn render(report: &Report) -> String {
    serde_json::to_string_pretty(&build(report))
        .unwrap_or_else(|error| format!("{{\"error\":\"could not serialize report: {error}\"}}"))
}

/// Build the document from a report.
pub fn build(report: &Report) -> Document<'_> {
    Document {
        username: report.request.username.as_str(),
        folder_id: report.request.folder.get(),
        collection_items: report.collection.item_count(),
        unique_pressings: report.valuations.len(),
        total_units: report.collection.total_units(),
        priced_at: report.priced_at_unix,
        total_value: report.collection_total.total.map(AmountView::from),
        coverage: report.collection_total.coverage(),
        valuations: report
            .valuations
            .iter()
            .map(|valuation| view_for(valuation, snapshot_for(report, valuation)))
            .collect(),
    }
}

/// Project a domain valuation into the wire shape.
fn view_for(valuation: &DomainValuation, snapshot: Option<&MarketSnapshot>) -> ValuationView {
    ValuationView {
        release_id: valuation.release_id.get(),
        copies: valuation.copies,
        condition: valuation.condition.to_string(),
        rarity: valuation.rarity.as_ref().map(rarity_view),
        value: valuation.value.map(|range| RangeView {
            low_minor: range.low.minor_units,
            high_minor: range.high.minor_units,
            currency: currency_code(range.low.currency),
            display: range.midpoint().to_string(),
        }),
        confidence: valuation.confidence,
        sales: snapshot
            .map(|snapshot| snapshot.sales.iter().map(sale_view).collect())
            .unwrap_or_default(),
    }
}

/// Project one domain sale into the wire shape.
fn sale_view(sale: &Sale) -> SaleView {
    SaleView {
        price_minor: sale.price.minor_units,
        currency: currency_code(sale.price.currency),
        sold_at: sale.sold_at,
        condition: sale.condition,
        signed: sale.is_signed,
        excluded: sale.is_outlier(),
    }
}

fn rarity_view(rarity: &Rarity) -> RarityView {
    RarityView {
        score: rarity.score,
        band: rarity.band().as_str(),
        measured_signals: rarity.measured_count(),
        components: rarity
            .components
            .iter()
            .map(|component| ComponentView {
                name: component.name,
                magnitude: component.magnitude,
            })
            .collect(),
    }
}

/// Find the snapshot for a valuation, if one exists.
fn snapshot_for<'a>(report: &'a Report, valuation: &DomainValuation) -> Option<&'a MarketSnapshot> {
    report
        .snapshots
        .iter()
        .find(|snapshot| snapshot.release_id == valuation.release_id)
}

/// The ISO code for a currency.
const fn currency_code(currency: crate::domain::market::Currency) -> &'static str {
    use crate::domain::market::Currency as C;
    match currency {
        C::Usd => "USD",
        C::Eur => "EUR",
        C::Gbp => "GBP",
        C::Jpy => "JPY",
    }
}

/// Serialize a report straight to a writer, for streaming large output.
pub fn write_to(report: &Report, writer: &mut impl std::io::Write) -> Result<()> {
    serde_json::to_writer_pretty(writer, &build(report))
        .map_err(|error| crate::error::Error::msg(format!("could not write JSON: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        adapters::testing::{FakeCollectionSource, FakeSaleSource, FixedClock, RecordingSleeper},
        app::{Request, run},
        domain::{
            collection::Page,
            identity::ReleaseId,
            release::{Artist, Format, Label, Release},
        },
    };

    fn release(id: u64) -> Release {
        Release {
            id: ReleaseId::new(id).unwrap(),
            title: "Kind of Blue".into(),
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

    fn report_for(releases: Vec<Release>) -> Report {
        let items = releases.len() as u64;
        let source = FakeCollectionSource::with_pages(vec![Page {
            number: 1,
            total_pages: 1,
            total_items: items,
            releases,
        }]);
        run(
            Request::new("arok", 0, 100, 365).unwrap(),
            &source,
            &FakeSaleSource::new(),
            &FixedClock(1_700_000_000),
            &RecordingSleeper::new(),
            std::time::Duration::ZERO,
        )
        .unwrap()
    }

    #[test]
    fn output_is_valid_json() {
        let json = render(&report_for(vec![release(101)]));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["username"], "arok");
        assert_eq!(parsed["collection_items"], 1);
    }

    #[test]
    fn unknown_values_serialize_as_null_not_zero() {
        let parsed: serde_json::Value =
            serde_json::from_str(&render(&report_for(vec![release(101)]))).unwrap();
        assert_eq!(parsed["valuations"][0]["value"], serde_json::Value::Null);
        assert_eq!(parsed["valuations"][0]["rarity"], serde_json::Value::Null);
        assert!(parsed.get("total_value").is_none(), "omitted, not zero");
    }

    #[test]
    fn coverage_is_always_present() {
        let parsed: serde_json::Value = serde_json::from_str(&render(&report_for(vec![]))).unwrap();
        assert!(
            parsed.get("coverage").is_some(),
            "a total is meaningless without its coverage"
        );
        assert_eq!(parsed["coverage"], 0.0);
    }

    #[test]
    fn duplicate_copies_collapse_into_one_valuation() {
        let parsed: serde_json::Value = serde_json::from_str(&render(&report_for(vec![
            release(101),
            release(101),
            release(102),
        ])))
        .unwrap();
        assert_eq!(parsed["collection_items"], 3);
        assert_eq!(parsed["unique_pressings"], 2);
        assert_eq!(parsed["valuations"][0]["copies"], 2);
    }

    #[test]
    fn money_is_serialized_in_minor_units() {
        // The wire carries integers plus a display string; never a float.
        let money = Money {
            minor_units: 1999,
            currency: crate::domain::market::Currency::Eur,
        };
        let view = AmountView::from(money);
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("\"minor\":1999"));
        assert!(json.contains("\"currency\":\"EUR\""));
        assert!(json.contains("\"display\":\"€19.99\""));
        assert!(!json.contains("19.99,"), "minor units must stay an integer");
    }

    #[test]
    fn write_to_matches_render() {
        let report = report_for(vec![release(101)]);
        let mut buffer = Vec::new();
        write_to(&report, &mut buffer).unwrap();
        let from_writer = String::from_utf8(buffer).unwrap();
        // Both go through `build`, so they must agree structurally.
        let a: serde_json::Value = serde_json::from_str(&render(&report)).unwrap();
        let b: serde_json::Value = serde_json::from_str(&from_writer).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn currency_codes_are_iso_4217() {
        use crate::domain::market::Currency as C;
        assert_eq!(currency_code(C::Usd), "USD");
        assert_eq!(currency_code(C::Eur), "EUR");
        assert_eq!(currency_code(C::Gbp), "GBP");
        assert_eq!(currency_code(C::Jpy), "JPY");
    }
}
