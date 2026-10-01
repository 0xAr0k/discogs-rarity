//! The valuation domain: combining rarity and market into one answer.
//!
//! # What a valuation actually is
//!
//! Not "the value of a record". It is:
//!
//! > Given what this pressing is, how scarce it looks, what comparable copies
//! > sold for recently, and what condition *this* copy is in — what is a
//! > reasonable range for **this** copy today?
//!
//! Three corrections separate that from a raw sale median:
//!
//! 1. **Condition.** A VG+ copy is worth less than a NM one of the same
//!    pressing. Sales history is a mix of conditions; the median is for the
//!    "typical" copy, not yours.
//! 2. **Multiplicity.** Three copies of one pressing are worth more than one,
//!    but not always three times more — buyers discount lots.
//! 3. **Confidence.** A €40 estimate from two sales is a guess and must be
//!    presented as one.
//!
//! # Why this is its own domain
//!
//! Because it is the part most likely to change with feedback from real use,
//! and it should be changeable without touching HTTP, JSON, or rarity. If the
//! formatter starts importing `reqwest`, one of these modules is in the wrong
//! place.

use crate::error::DomainError;

use super::identity::ReleaseId;
use super::market::{Currency, Money, PriceEstimate};
use super::rarity::Rarity;

/// The condition of the copy being valued, on Discogs' 1–10 scale.
///
/// # Why an enum and not a `u8`
///
/// The scale has a *name* at every step, and the names are what collectors
/// argue about. `Condition::VeryGoodPlus` documents itself; `7` does not. An
/// enum also makes `match` exhaustive, so adding a grade later is a compile
/// error you can find rather than a silent default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Condition {
    #[default]
    Mint,
    NearMint,
    ExcellentPlus,
    VeryGoodPlus,
    VeryGood,
    GoodPlus,
    Good,
    Fair,
    Poor,
}

impl Condition {
    /// All grades in descending order.
    pub const ALL: [Self; 9] = [
        Self::Mint,
        Self::NearMint,
        Self::ExcellentPlus,
        Self::VeryGoodPlus,
        Self::VeryGood,
        Self::GoodPlus,
        Self::Good,
        Self::Fair,
        Self::Poor,
    ];

    /// The numeric grade Discogs uses, 1 = mint, 10 = poor.
    pub const fn grade(self) -> u8 {
        match self {
            Self::Mint => 1,
            Self::NearMint => 2,
            Self::ExcellentPlus => 3,
            Self::VeryGoodPlus => 4,
            Self::VeryGood => 5,
            Self::GoodPlus => 6,
            Self::Good => 7,
            Self::Fair => 8,
            Self::Poor => 9,
        }
    }

    /// The label as it appears in sale data.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mint => "Mint",
            Self::NearMint => "Near Mint",
            Self::ExcellentPlus => "Excellent Plus",
            Self::VeryGoodPlus => "Very Good Plus",
            Self::VeryGood => "Very Good",
            Self::GoodPlus => "Good Plus",
            Self::Good => "Good",
            Self::Fair => "Fair",
            Self::Poor => "Poor",
        }
    }
}

impl TryFrom<u8> for Condition {
    type Error = DomainError;

    fn try_from(grade: u8) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|condition| condition.grade() == grade)
            .ok_or(DomainError::OutOfRange {
                what: "condition grade",
                value: f64::from(grade),
            })
    }
}

impl std::fmt::Display for Condition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A money range, low to high.
///
/// # Why a range and not a point
///
/// The honest output of a sale-history estimate is a spread. `[€35, €60]`
/// communicates more than `€47.50`, which implies a precision nobody has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceRange {
    pub low: Money,
    pub high: Money,
}

impl PriceRange {
    /// A degenerate range representing one price.
    pub fn exact(value: Money) -> Self {
        Self {
            low: value,
            high: value,
        }
    }

    /// The midpoint, rounding toward the higher value on a half.
    pub fn midpoint(&self) -> Money {
        // Round half *away from zero*, for both signs.
        //
        // Plain `/` truncates toward zero: 2125 / 2 == 1062, losing the half
        // in the low direction. Doing it by magnitude keeps the rule symmetric
        // and obvious, which matters here because a range can legitimately
        // span zero (a seller paying a buyer, say).
        let sum = self.low.minor_units.saturating_add(self.high.minor_units);
        let magnitude = sum.unsigned_abs();
        let rounded = magnitude / 2 + u64::from(!magnitude.is_multiple_of(2));
        Money {
            minor_units: if sum < 0 {
                -(rounded as i64)
            } else {
                rounded as i64
            },
            currency: self.low.currency,
        }
    }

    /// Order the endpoints, tolerating swapped input.
    pub fn sorted(self) -> Self {
        if self.low.minor_units <= self.high.minor_units {
            self
        } else {
            Self {
                low: self.high,
                high: self.low,
            }
        }
    }
}

/// A complete valuation of one pressing the user owns.
#[derive(Debug, Clone, PartialEq)]
pub struct Valuation {
    pub release_id: ReleaseId,
    /// How many copies are included in these figures.
    pub copies: u32,
    /// Condition of the copy being valued.
    pub condition: Condition,
    /// Rarity assessment, when one could be made.
    pub rarity: Option<Rarity>,
    /// Market estimate for a typical copy, when one could be made.
    pub market: Option<PriceEstimate>,
    /// What this copy is judged to be worth, when it could be judged.
    ///
    /// `None` means "unknown", never "worthless".
    pub value: Option<PriceRange>,
    /// `0.0..=1.0`: trust in `value`.
    pub confidence: f64,
}

impl Valuation {
    /// The "we know nothing, and we are not going to pretend" valuation.
    pub fn unknown(release_id: ReleaseId, copies: u32, condition: Condition) -> Self {
        Self {
            release_id,
            copies,
            condition,
            rarity: None,
            market: None,
            value: None,
            confidence: 0.0,
        }
    }

    /// One-line summary for the text renderer.
    pub fn summary(&self) -> String {
        // TODO(you): make this read well. Right now it joins optionals with
        // separators by hand, which is the shape of a bug — consider
        // `Option::iter().map(..).collect::<Vec<_>>().join(" · ")`.
        let rarity = match &self.rarity {
            Some(rarity) => format!("rarity {}", rarity.band().as_str()),
            None => "rarity unknown".to_owned(),
        };
        let value = match self.value {
            Some(range) => format!("value {}", range.midpoint()),
            None => "value unknown".to_owned(),
        };
        format!(
            "{} × {} · {rarity} · {value} · confidence {:.0}%",
            self.release_id,
            self.copies,
            self.confidence * 100.0
        )
    }
}

/// A valuation of a whole collection.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectionValuation {
    pub valuations: Vec<Valuation>,
    /// Total value of everything, in one currency.
    ///
    /// `None` when no valuation could be made — and deliberately **not** a sum
    /// that treats missing entries as zero. See [`CollectionValuation::coverage`].
    pub total: Option<Money>,
    /// Fraction of owned pressings that received a value, `0.0..=1.0`.
    pub coverage: f64,
}

impl CollectionValuation {
    /// Sum the valuations, reporting how much of the collection was covered.
    ///
    /// # The rule this must not break
    ///
    /// A sum that adds zero for unvalued entries is a lie: "your collection is
    /// worth €40" when €40 is the price of the one item we could price and 400
    /// others are unknown. Always report the total *together with* coverage.
    ///
    /// # TODO(you) — exercise 3b
    ///
    /// 1. Filter to valuations with a `value`. If none have one, the total is
    ///    `None`, not zero.
    /// 2. **Currency problem:** summing requires one currency. Options:
    ///    - Convert with an explicit rate table passed in as an argument.
    ///    - Only total when every valuation shares a currency, else report
    ///      per-currency subtotals.
    ///
    ///    The second is less useful but honest and needs no rates. Decide which
    ///    fits this app, and note the choice in a comment. If you pick the
    ///    first, the rate source and its date belong in the type, not in a
    ///    constant.
    /// 3. Multiplicity: multiply each pressing's value by `copies`. Consider
    ///    whether a lot of identical copies should be discounted — three NM
    ///    copies of the same record is a worse deal per copy than one.
    /// 4. `coverage` = valued pressings / total pressings.
    ///
    /// The currency is an *input*, not something discovered from the data — which
    /// is exactly why it is a parameter and not a field on the struct.
    pub fn total(&self, _currency: Currency) -> Option<Money> {
        todo!("exercise 3b: total the collection, or refuse to")
    }

    /// Fraction of pressings that received a value.
    pub fn coverage(&self) -> f64 {
        if self.valuations.is_empty() {
            return 0.0;
        }
        let valued = self
            .valuations
            .iter()
            .filter(|valuation| valuation.value.is_some())
            .count();
        valued as f64 / self.valuations.len() as f64
    }
}

/// Adjust a market estimate for the condition of *this* copy.
///
/// # The reasoning
///
/// [`PriceEstimate::median`] describes a typical sold copy. Yours is usually
/// better or worse than typical. The multiplier encodes how much that matters.
///
/// # TODO(you) — exercise 3a
///
/// A defensible first version: a lookup table of per-grade multipliers
/// relative to the median's assumed condition. Something like:
///
/// | Grade | Multiplier |
/// |-------|-----------|
/// | Mint, Near Mint | 1.15 |
/// | Excellent Plus | 1.05 |
/// | Very Good Plus | 1.00 |
/// | Very Good | 0.85 |
/// | Good Plus | 0.70 |
/// | Good | 0.55 |
/// | Fair, Poor | 0.35 |
///
/// Then:
///
/// - `None` in, `None` out — never invent a value from nothing.
/// - Clamp to `0.0..=1.0` after the filter step's own confidence is applied,
///   or just here.
/// - Consider blending by confidence: with a single sale, a condition
///   correction is mostly fiction. `adjusted = median * (1 + c*(m-1))` where
///   `c` is confidence shrinks the adjustment toward 1.0 when evidence is
///   thin. That is a nice use of the confidence you already computed.
///
/// Signature hint: return `Option<Money>` and keep the rounding to minor
/// units explicit.
pub fn apply_condition(estimate: PriceEstimate, condition: Condition) -> Option<Money> {
    let _ = (estimate, condition);
    todo!("exercise 3a: condition-adjust a market estimate")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eur(minor_units: i64) -> Money {
        Money {
            minor_units,
            currency: Currency::Eur,
        }
    }

    #[test]
    fn condition_grades_round_trip() {
        for condition in Condition::ALL {
            assert_eq!(Condition::try_from(condition.grade()), Ok(condition));
            assert!(!condition.as_str().is_empty());
        }
    }

    #[test]
    fn condition_rejects_out_of_scale_grades() {
        assert!(Condition::try_from(0).is_err());
        assert!(Condition::try_from(10).is_err());
        assert!(Condition::try_from(11).is_err());
    }

    #[test]
    fn conditions_are_ordered_from_mint_to_poor() {
        assert!(Condition::Mint < Condition::NearMint);
        assert!(Condition::NearMint < Condition::Poor);
    }

    #[test]
    fn price_range_midpoint_rounds_to_the_higher_half() {
        // 1050 and 1075 -> 2125 / 2 = 1062.5, rounds up to 1063.
        let range = PriceRange {
            low: eur(1050),
            high: eur(1075),
        };
        assert_eq!(range.midpoint().minor_units, 1063);
    }

    #[test]
    fn price_range_sorts_swapped_endpoints() {
        let sorted = PriceRange {
            low: eur(5000),
            high: eur(1000),
        }
        .sorted();
        assert_eq!(sorted.low.minor_units, 1000);
        assert_eq!(sorted.high.minor_units, 5000);
    }

    #[test]
    fn exact_range_has_equal_endpoints() {
        let range = PriceRange::exact(eur(1999));
        assert_eq!(range.low, range.high);
        assert_eq!(range.midpoint().minor_units, 1999);
    }

    #[test]
    fn unknown_valuation_does_not_invent_a_value() {
        let valuation = Valuation::unknown(ReleaseId::new(1).unwrap(), 1, Condition::Mint);
        assert_eq!(valuation.value, None);
        assert_eq!(valuation.confidence, 0.0);
        assert!(valuation.summary().contains("unknown"));
    }

    #[test]
    fn coverage_is_zero_for_an_empty_collection() {
        let valuation = CollectionValuation {
            valuations: vec![],
            total: None,
            coverage: 0.0,
        };
        assert_eq!(valuation.coverage(), 0.0);
    }

    #[test]
    fn coverage_counts_only_valued_entries() {
        let valuation = CollectionValuation {
            valuations: vec![
                Valuation::unknown(ReleaseId::new(1).unwrap(), 1, Condition::Mint),
                Valuation {
                    value: Some(PriceRange::exact(eur(1000))),
                    ..Valuation::unknown(ReleaseId::new(2).unwrap(), 1, Condition::Mint)
                },
            ],
            total: None,
            coverage: 0.0,
        };
        assert!(
            (valuation.coverage() - 0.5).abs() < 1e-9,
            "got {}",
            valuation.coverage()
        );
    }

    // ---- exercise 3a gates ---------------------------------------------

    #[test]
    fn condition_adjustment_of_an_unknown_estimate_stays_unknown() {
        assert_eq!(
            apply_condition(PriceEstimate::UNKNOWN, Condition::Mint),
            None
        );
    }

    #[test]
    fn condition_adjustment_keeps_a_better_copy_above_the_median() {
        let estimate = PriceEstimate {
            median: Some(eur(1000)),
            sample_size: 10,
            confidence: 1.0,
        };
        let nm = apply_condition(estimate, Condition::NearMint).unwrap();
        let vg = apply_condition(estimate, Condition::VeryGood).unwrap();
        assert!(
            nm.minor_units > vg.minor_units,
            "a better copy must not be worth less: {nm} vs {vg}"
        );
    }

    #[test]
    fn condition_adjustment_decreases_monotonically_with_wear() {
        let estimate = PriceEstimate {
            median: Some(eur(1000)),
            sample_size: 20,
            confidence: 1.0,
        };
        let values: Vec<i64> = Condition::ALL
            .into_iter()
            .filter_map(|c| apply_condition(estimate, c))
            .map(|money| money.minor_units)
            .collect();
        for pair in values.windows(2) {
            assert!(
                pair[0] >= pair[1],
                "value must not increase with wear: {values:?}"
            );
        }
    }

    #[test]
    fn condition_adjustment_never_goes_negative() {
        let estimate = PriceEstimate {
            median: Some(eur(100)),
            sample_size: 1,
            confidence: 0.01,
        };
        for condition in Condition::ALL {
            let money = apply_condition(estimate, condition).unwrap();
            assert!(
                money.minor_units >= 0,
                "{condition} produced a negative value"
            );
        }
    }

    // ---- exercise 3b gates ---------------------------------------------

    #[test]
    fn total_is_none_when_nothing_could_be_valued() {
        let valuation = CollectionValuation {
            valuations: vec![Valuation::unknown(
                ReleaseId::new(1).unwrap(),
                3,
                Condition::Mint,
            )],
            total: None,
            coverage: 0.0,
        };
        assert_eq!(
            valuation.total(Currency::Eur),
            None,
            "an unvalued collection must not total €0"
        );
    }

    #[test]
    fn total_multiplies_by_copies() {
        let valuation = CollectionValuation {
            valuations: vec![Valuation {
                copies: 3,
                value: Some(PriceRange::exact(eur(1000))),
                ..Valuation::unknown(ReleaseId::new(1).unwrap(), 3, Condition::Mint)
            }],
            total: None,
            coverage: 0.0,
        };
        assert_eq!(valuation.total(Currency::Eur), Some(eur(3000)));
    }

    #[test]
    fn total_ignores_unvalued_pressings_rather_than_counting_them_as_zero() {
        let valuation = CollectionValuation {
            valuations: vec![
                Valuation {
                    value: Some(PriceRange::exact(eur(1000))),
                    ..Valuation::unknown(ReleaseId::new(1).unwrap(), 1, Condition::Mint)
                },
                Valuation::unknown(ReleaseId::new(2).unwrap(), 1, Condition::Mint),
            ],
            total: None,
            coverage: 0.0,
        };
        assert_eq!(
            valuation.total(Currency::Eur),
            Some(eur(1000)),
            "the unknown pressing must contribute nothing, not a zero-priced row"
        );
    }
}
