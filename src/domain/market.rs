//! The market domain: what is this pressing worth right now?
//!
//! # "Right now" is the hard part
//!
//! A valuation with no date is a lie. This module keeps [`Sale`] as a dated
//! fact and derives prices *from* those facts, so every number that reaches
//! the user can be traced to real transactions with real dates.
//!
//! # Price is a distribution, not a number
//!
//! Discogs sale history is full of things that are not the market price for a
//! good copy of your pressing: signed copies, promos, mistyped listings, 10
//! copies of the same thing bought by one collector flipping it, and the
//! occasional typo that reports a LP as worth four figures. Any honest
//! estimator therefore needs:
//!
//! - a **filter** for what counts as a comparable sale ([`Sale::is_outlier`]),
//! - a **statistic** that resists a few wild values (a median, trimmed mean,
//!   percentiles),
//! - and an honest **confidence** figure when the sample is thin.
//!
//! # The failure mode to avoid
//!
//! "No sales data → worth €0.00". That reads as "worthless", and a user could
//! act on it. It must read as "no data". See
//! [`Valuation::confidence`] and the `Option` in [`PriceEstimate`].

use super::identity::ReleaseId;
use super::release::Release;

/// A currency amount in the minor unit (cents), plus its ISO code.
///
/// # Why integer cents
///
/// `f64` money accumulates error: `0.1 + 0.2 != 0.3`. Store minor units as
/// `i64` and formatting is the only place a decimal point appears. Conversion
/// happens once, at the edge, with a stated exchange rate — never implicitly.
///
/// # Why not `rust_decimal` or `bigdecimal`
///
/// Both are reasonable choices. This is a CLI over a JSON API that reports
/// floats anyway, so `i64` minor units is the honest lower-complexity choice.
/// Revisit when you need multi-currency totals: summing across currencies
/// requires a rate and a rounding policy, and that policy should be yours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Money {
    /// Amount in minor units. EUR and USD both use 2 digits; JPY uses 0.
    pub minor_units: i64,
    pub currency: Currency,
}

/// Currencies Discogs reports, with their minor-unit exponents.
///
/// Exponent lives with the currency so [`Money`] can round-trip a value
/// without a lookup at the format site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Currency {
    Usd,
    Eur,
    Gbp,
    Jpy,
}

impl Currency {
    /// Digits after the decimal point.
    pub const fn exponent(self) -> u32 {
        match self {
            Self::Jpy => 0,
            Self::Usd | Self::Eur | Self::Gbp => 2,
        }
    }

    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Usd => "$",
            Self::Eur => "€",
            Self::Gbp => "£",
            Self::Jpy => "¥",
        }
    }
}

impl std::fmt::Display for Money {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let scale = 10f64.powi(self.currency.exponent() as i32);
        // `i64` money can exceed `f64`'s exact-integer range; dividing by the
        // scale first keeps the printed value meaningful rather than `inf`.
        let major = self.minor_units as f64 / scale;
        write!(f, "{}{major:.2}", self.currency.symbol())
    }
}

/// One observed transaction, as reported by a marketplace.
///
/// This is *evidence*, not truth. [`Sale::is_outlier`] exists to separate the
/// two.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sale {
    /// Amount paid, in minor units of `currency`.
    pub price: Money,
    /// Unix timestamp of the sale.
    pub sold_at: i64,
    /// Condition grade as a number: 1 = mint, 10 = poor, per Discogs' scale.
    ///
    /// Conditions are *not* comparable across formats or eras. A VG+ 1970s
    /// pressing and a VG+ 2020s repress are different goods. This is why the
    /// valuation step needs a condition adjustment rather than a raw median.
    pub condition: u8,
    /// Whether the seller described it as signed, limited, or promo.
    ///
    /// Signed copies often sell for multiples of the plain version. Including
    /// them in an unsigned valuation is the single most common way these tools
    /// inflate prices.
    pub is_signed: bool,
    /// Whether the listing was for a media copy or an apparent "look at
    /// photo only" sale, which is unreliable data.
    pub is_media_only: bool,
}

impl Sale {
    /// Sales to exclude from a valuation of the *plain* pressing.
    ///
    /// The rule is deliberately minimal: media-only sales only, because
    /// "look at photos only" listings frequently describe an item the seller
    /// has never handled. Signed/promo sales, IQR-style price outliers, and
    /// condition bounds are honest next steps when the market estimator
    /// itself exists — each is a design trade-off (a signed sale is also
    /// *evidence* of demand) that deserves its own decision first.
    pub fn is_outlier(&self) -> bool {
        self.is_media_only
    }
}

/// A price estimate derived from sale history.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PriceEstimate {
    /// `None` when there is not enough data to say anything.
    ///
    /// `Some(0)` would mean "worthless"; `None` means "unknown". Keep them
    /// distinct all the way to the renderer.
    pub median: Option<Money>,
    /// Number of sales the estimate used, after filtering.
    pub sample_size: usize,
    /// `0.0..=1.0`: how much to trust `median`.
    ///
    /// Sizing candidates when an estimator lands here: saturating on sample
    /// size (`min(1.0, n / 20)`), age-weighting with an explicit decay
    /// constant, or dispersion-aware confidence that discounts a median whose
    /// quartiles disagree. What it must never do is look confident on a thin
    /// sample.
    pub confidence: f64,
}

impl PriceEstimate {
    /// The "no data" estimate.
    pub const UNKNOWN: Self = Self {
        median: None,
        sample_size: 0,
        confidence: 0.0,
    };

    /// Whether we can say anything at all.
    pub fn is_known(&self) -> bool {
        self.median.is_some()
    }
}
/// A price estimate plus the window it describes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarketValue {
    pub price: PriceEstimate,
    /// Days covered by the sales used.
    pub window_days: i32,
}

impl MarketValue {
    /// An explicitly unknown market value, with no implied window.
    pub const UNKNOWN: Self = Self {
        price: PriceEstimate::UNKNOWN,
        window_days: 0,
    };
}

// The estimator is deliberately absent — a rule-based estimate that
// confidently mis-prices a pressing is worse than `UNKNOWN`, and picking the
// statistic (median vs trimmed mean vs IQR screen) is a decision that should
// be made against real sale data, not shipped blind. Until one exists,
// `snapshots_for` reports unknown estimates and carries the raw `sales` so
// the renderer can show the evidence instead of a fiction.
//
// What this module gives the future estimator:
// - `Sale::is_outlier` — the only filter rule so far (media-only).
// - `is_recent`/`days_between` — the age cut, clock-injected for determinism.
// - `MarketValue` — the estimate plus the window it describes.

/// Whether a sale falls inside the recent window relative to `now` (unix secs).
///
/// Pure and explicit about its clock: the caller passes "now", so the function
/// is deterministic and needs no mocking.
pub fn is_recent(sale: &Sale, now: i64, window_days: i32) -> bool {
    let cutoff = now - i64::from(window_days) * 86_400;
    sale.sold_at >= cutoff
}

/// The absolute days between two unix timestamps.
pub fn days_between(from: i64, to: i64) -> i32 {
    let seconds = (to - from).abs();
    i32::try_from(seconds / 86_400).unwrap_or(i32::MAX)
}

/// A release plus everything the market knows about it.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketSnapshot {
    pub release_id: ReleaseId,
    pub value: MarketValue,
    /// Every sale seen, including rejected ones.
    ///
    /// Kept so the renderer can explain *why* an estimate is thin.
    pub sales: Vec<Sale>,
}

impl MarketSnapshot {
    /// The "we know nothing" snapshot.
    pub fn unknown(release_id: ReleaseId) -> Self {
        Self {
            release_id,
            value: MarketValue::UNKNOWN,
            sales: Vec::new(),
        }
    }
}

/// Group sales by pressing, preserving first-seen order.
///
/// # Why this is here and not in the adapter
///
/// "What is this collection worth" needs per-pressing sales. Deciding the
/// grouping rule (first-seen order, or one entry per owned release) is a
/// domain decision about *what a collection is*, so it belongs next to
/// [`crate::domain::collection::Collection`]'s rules. The adapter's only job
/// is fetching.
pub fn snapshots_for<'a>(
    releases: impl Iterator<Item = &'a Release>,
    sales_by_release: &'a std::collections::HashMap<ReleaseId, Vec<Sale>>,
    now: i64,
    _window_days: i32,
) -> Vec<MarketSnapshot> {
    releases
        .map(|release| {
            let sales = sales_by_release
                .get(&release.id)
                .cloned()
                .unwrap_or_default();
            // No estimator yet: an unknown estimate is the honest answer an
            // absent statistic must give. The raw sales ride along so the
            // renderer can show the evidence instead of a fiction.
            let price = PriceEstimate::UNKNOWN;
            let window_days = sales
                .iter()
                .map(|sale| days_between(sale.sold_at, now))
                .max()
                .unwrap_or(0);
            MarketSnapshot {
                release_id: release.id,
                value: MarketValue { price, window_days },
                sales,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn money(minor_units: i64, currency: Currency) -> Money {
        Money {
            minor_units,
            currency,
        }
    }

    fn sale(minor_units: i64, days_ago: i64, condition: u8) -> Sale {
        Sale {
            price: money(minor_units, Currency::Eur),
            sold_at: 1_700_000_000 - days_ago * 86_400,
            condition,
            is_signed: false,
            is_media_only: false,
        }
    }

    #[test]
    fn money_formats_with_the_right_precision_and_symbol() {
        assert_eq!(money(1999, Currency::Eur).to_string(), "€19.99");
        assert_eq!(money(5, Currency::Usd).to_string(), "$0.05");
        assert_eq!(money(1200, Currency::Jpy).to_string(), "¥1200.00");
    }

    #[test]
    fn currency_exponents_are_correct() {
        assert_eq!(Currency::Jpy.exponent(), 0);
        assert_eq!(Currency::Eur.exponent(), 2);
    }

    #[test]
    fn is_recent_is_relative_to_the_supplied_clock() {
        let now = 1_700_000_000;
        let fresh = sale(1000, 10, 5);
        let ancient = sale(1000, 400, 5);
        assert!(is_recent(&fresh, now, 30));
        assert!(!is_recent(&ancient, now, 30));
    }

    #[test]
    fn days_between_is_absolute_and_floors() {
        assert_eq!(days_between(0, 86_400 * 3), 3);
        assert_eq!(days_between(86_400 * 3, 0), 3);
        assert_eq!(days_between(0, 86_399), 0);
    }

    #[test]
    fn media_only_sales_are_excluded_by_default() {
        let mut s = sale(1000, 5, 5);
        assert!(!s.is_outlier());
        s.is_media_only = true;
        assert!(s.is_outlier());
    }

    #[test]
    fn unknown_estimates_distinguish_no_data_from_zero() {
        assert!(!PriceEstimate::UNKNOWN.is_known());
        assert_eq!(PriceEstimate::UNKNOWN.median, None);
        let zero = PriceEstimate {
            median: Some(money(0, Currency::Eur)),
            sample_size: 1,
            confidence: 1.0,
        };
        assert!(zero.is_known(), "€0 is a value; None is an absence");
    }

    #[test]
    fn snapshots_are_unknown_but_carried_with_evidence() {
        // The estimator is deliberately absent, so a pressing that *has*
        // sales still prices as unknown — the snapshot carries the raw sales
        // instead of a fiction, and a pressing with none is unknown too.
        let releases = [release_with_id(10, "A"), release_with_id(11, "B")];
        let mut sales = std::collections::HashMap::new();
        sales.insert(ReleaseId::new(10).unwrap(), vec![sale(1000, 5, 5)]);

        let snapshots = snapshots_for(releases.iter(), &sales, 1_700_000_000, 30);
        assert_eq!(snapshots.len(), 2);
        assert!(
            !snapshots[0].value.price.is_known(),
            "no estimator: unknown, not invented"
        );
        assert_eq!(snapshots[0].sales.len(), 1, "the evidence rides along");
        assert!(
            !snapshots[1].value.price.is_known(),
            "no sales means unknown"
        );
        assert_eq!(snapshots[1].release_id, ReleaseId::new(11).unwrap());
    }

    /// A bare release the snapshot grouping only needs for its id.
    fn release_with_id(id: u64, title: &str) -> crate::domain::release::Release {
        crate::domain::release::Release {
            id: ReleaseId::new(id).unwrap(),
            title: title.into(),
            year: None,
            artists: vec![],
            formats: vec![],
            labels: vec![],
            thumb: String::new(),
            resource_url: String::new(),
            country: None,
            master_id: None,
        }
    }
}
