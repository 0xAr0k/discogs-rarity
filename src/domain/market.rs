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
//! - a **statistic** that resists a few wild values ([`PriceEstimate::median`],
//!   trimmed mean, percentiles),
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
    /// # TODO(you) — exercise 2a
    ///
    /// The current rule is deliberately minimal: media-only sales only, because
    /// "look at photos only" listings frequently describe an item the seller
    /// has never handled. Extend it:
    ///
    /// 1. **Signed / promo / autograph.** Usually a different good entirely.
    ///    But consider: a signed sale is *evidence* of demand. Perhaps it
    ///    belongs in a separate `signed_market_value` rather than being
    ///    discarded. Design decision, your call — write down which you chose.
    /// 2. **Price outliers**, using something like IQR: compute Q1/Q3, flag
    ///    sales outside `Q1 - 1.5*IQR .. Q3 + 1.5*IQR`. Watch for small
    ///    samples — with 4 sales the quartiles are meaningless, so only apply
    ///    the rule above a minimum count.
    /// 3. **Condition bounds**, if you want a single "fair" number: exclude
    ///    grades below a floor (M and VG- pressings are priced differently).
    ///
    /// Note the type: `bool`. Keep it that way. "This sale is excluded and
    /// here is why" is better modelled as a returned reason, but only add that
    /// when you actually need to display it.
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
    /// # TODO(you) — exercise 2b
    ///
    /// Suggested starting points, pick one and justify it in a comment:
    ///
    /// - Saturating on sample size: `min(1.0, n / 20)`. Simple, honest.
    /// - Age-weighted: recent sales count more. Needs a decay constant, and
    ///   that constant is a judgement call you will have to defend.
    /// - Dispersion-aware: if the middle 50% spans a huge range, the median is
    ///   not a good description of this market. Multiply the size-based
    ///   confidence by agreement between quartiles.
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

/// The median of a set of amounts in minor units.
///
/// # Why median and not mean
///
/// One typo — a €400 listing for a €4 record — drags a mean into fiction. The
/// median ignores up to half the sample, so a handful of bad values cannot
/// move it. Mean also has no natural place to *land*: the median of a set of
/// prices is itself a price somebody actually paid.
///
/// The implementation sorts a copy and indexes the middle, handling even
/// lengths by averaging the two central values and rounding **half away from
/// zero** so the result is never biased upward for negatives.
///
/// # TODO(you) — exercise 2c
///
/// Three follow-ups worth doing yourself:
///
/// 1. Return `Option<Money>` for the empty case rather than taking an
///    `assert!`. A function that can panic on valid input is a bug.
/// 2. Consider a **trimmed mean** instead: drop the top and bottom 10% before
///    averaging. More data-efficient than the median for small samples.
/// 3. Consider [`MarketValue::window_days`]: what if the only sales are from
///    2009? That is not a current valuation. Filtering by age before
///    estimating belongs *here*, not in the adapter.
pub fn median(samples: &[i64], currency: Currency) -> Option<Money> {
    // TODO(you): return `None` for an empty slice. See the doc comment.
    let _ = (samples, currency);
    todo!("exercise 2c: median in minor units, Option for the empty case")
}

/// Reduce raw sales to a defensible price estimate.
///
/// # TODO(you) — exercise 2d
///
/// Pipeline to implement, in order:
///
/// 1. `sales.iter().filter(|s| !s.is_outlier()).collect()`
/// 2. Sort by `sold_at` **descending** to find the newest sale.
/// 3. Apply an age cutoff — how recent is "recent"? Make it a parameter rather
///    than a constant; a shop and an archive want different windows.
/// 4. `median` over the survivors.
/// 5. `sample_size` = survivors *after* filtering, and `confidence` per
///    exercise 2b.
///
/// Signature hint: take the cutoff as an argument so the caller decides the
/// policy and the function stays pure.
pub fn estimate_from_sales(sales: &[Sale], recent_window_days: i32) -> PriceEstimate {
    let _ = (sales, recent_window_days);
    todo!("exercise 2d: filter, age-cut, median, confidence")
}

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
    window_days: i32,
) -> Vec<MarketSnapshot> {
    releases
        .map(|release| {
            let sales = sales_by_release
                .get(&release.id)
                .cloned()
                .unwrap_or_default();
            let price = if sales.is_empty() {
                PriceEstimate::UNKNOWN
            } else {
                estimate_from_sales(&sales, window_days)
            };
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

    // ---- exercise 2c gates ---------------------------------------------

    #[test]
    fn median_of_nothing_is_none_not_a_panic() {
        assert_eq!(median(&[], Currency::Eur), None);
    }

    #[test]
    fn median_of_one_sample_is_that_sample() {
        assert_eq!(
            median(&[1999], Currency::Eur),
            Some(money(1999, Currency::Eur))
        );
    }

    #[test]
    fn median_ignores_a_single_wild_outlier() {
        // The reason the median exists: a €400 typo must not become €133.
        let samples = [1000, 1050, 1100, 40_000, 1200];
        let estimate = median(&samples, Currency::Eur).unwrap();
        assert_eq!(
            estimate,
            money(1100, Currency::Eur),
            "the median should ignore the outlier"
        );
    }

    #[test]
    fn median_handles_an_even_sample_length() {
        assert_eq!(
            median(&[1000, 2000], Currency::Eur),
            Some(money(1500, Currency::Eur))
        );
    }

    #[test]
    fn median_does_not_mutate_its_input() {
        let samples = vec![3000, 1000, 2000];
        let before = samples.clone();
        median(&samples, Currency::Eur);
        assert_eq!(samples, before, "sorting must happen on a copy");
    }

    #[test]
    fn median_handles_negative_amounts_without_biasing_upward() {
        // Average of -10 and -30 is -20; a truncating average could yield -19.
        assert_eq!(
            median(&[-10, -30], Currency::Eur),
            Some(money(-20, Currency::Eur))
        );
    }

    // ---- exercise 2d gates ---------------------------------------------

    #[test]
    fn estimate_is_unknown_without_sales() {
        assert_eq!(estimate_from_sales(&[], 30), PriceEstimate::UNKNOWN);
    }

    #[test]
    fn estimate_reports_the_filtered_sample_size() {
        let sales = vec![sale(1000, 5, 5), sale(1100, 6, 5), sale(1200, 7, 5)];
        let estimate = estimate_from_sales(&sales, 30);
        assert_eq!(estimate.sample_size, 3);
        assert!(estimate.is_known());
    }

    #[test]
    fn estimate_ignores_sales_outside_the_window() {
        let now = 1_700_000_000;
        let sales = vec![
            sale(1000, 5, 5),
            Sale {
                sold_at: now - 400 * 86_400,
                ..sale(9999, 400, 5)
            },
        ];
        let estimate = estimate_from_sales(&sales, 30);
        assert_eq!(estimate.median, Some(money(1000, Currency::Eur)));
        assert_eq!(estimate.sample_size, 1);
    }

    #[test]
    fn estimate_keeps_only_usable_sales() {
        let mut media_only = sale(500, 5, 5);
        media_only.is_media_only = true;
        let sales = vec![sale(1000, 5, 5), sale(1100, 6, 5), media_only];
        let estimate = estimate_from_sales(&sales, 30);
        assert_eq!(estimate.sample_size, 2, "media-only sale must be dropped");
        assert_eq!(estimate.median, Some(money(1050, Currency::Eur)));
    }

    #[test]
    fn confidence_rises_with_sample_size_and_stays_in_range() {
        let mut previous = 0.0;
        for count in 1..=10 {
            let sales: Vec<Sale> = (0..count).map(|i| sale(1000 + i, i, 5)).collect();
            let estimate = estimate_from_sales(&sales, 30);
            assert!(
                (0.0..=1.0).contains(&estimate.confidence),
                "confidence {} escaped 0..=1",
                estimate.confidence
            );
            assert!(
                estimate.confidence >= previous,
                "confidence should not fall as evidence grows"
            );
            previous = estimate.confidence;
        }
    }

    #[test]
    fn a_single_sale_is_low_confidence() {
        let estimate = estimate_from_sales(&[sale(1000, 5, 5)], 30);
        assert!(
            estimate.confidence < 0.5,
            "one sale must not look certain, got {}",
            estimate.confidence
        );
    }

    // ---- snapshot grouping ---------------------------------------------

    #[test]
    fn snapshots_cover_every_release_in_order() {
        let releases = {
            let mut r = crate::domain::release::Release {
                id: ReleaseId::new(10).unwrap(),
                title: "A".into(),
                year: None,
                artists: vec![],
                formats: vec![],
                labels: vec![],
                thumb: String::new(),
                resource_url: String::new(),
                country: None,
                master_id: None,
            };
            let second = Release {
                id: ReleaseId::new(11).unwrap(),
                title: "B".into(),
                ..r.clone()
            };
            r.id = ReleaseId::new(10).unwrap();
            vec![r, second]
        };

        let mut sales = std::collections::HashMap::new();
        sales.insert(ReleaseId::new(10).unwrap(), vec![sale(1000, 5, 5)]);

        let snapshots = snapshots_for(releases.iter(), &sales, 1_700_000_000, 30);
        assert_eq!(snapshots.len(), 2);
        assert!(snapshots[0].value.price.is_known());
        assert!(
            !snapshots[1].value.price.is_known(),
            "no sales means unknown"
        );
        assert_eq!(snapshots[1].release_id, ReleaseId::new(11).unwrap());
    }
}
