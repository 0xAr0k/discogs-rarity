//! The rarity domain: how hard is this pressing to find?
//!
//! # The honest problem
//!
//! "Rarity" is one of those words that looks computable and isn't. Discogs
//! gives you counts (`community.have`, `community.want`) but the numbers
//! describe *interest*, not *scarcity*, and they are self-reinforcing: a
//! pressing everyone wants accumulates more `want`s, which makes the data look
//! like evidence of rarity when it is mostly evidence of taste.
//!
//! So this module deliberately does **not** ship a "rarity score" pretending to
//! be objective. What it does instead is name the *inputs* to a judgement,
//! keep each signal separate, and make the combination step — the part that
//! deserves human judgement — an explicit, ownable piece of work.
//!
//! # Vocabulary
//!
//! - A [`RaritySignal`] is one observable fact, in [0, 1], with a name.
//! - [`Rarity`] is what *you* decide those facts add up to.

use crate::error::DomainError;

/// One observable fact bearing on rarity, normalized to `0.0..=1.0`.
///
/// # Why a struct and not an `f64`
///
/// `fn score(scarcity: f64, demand: f64, pressings: f64)` has three anonymous
/// numbers. A named struct makes the JSON output self-documenting: a user can
/// read *why* something is rare, which is the only reason to publish a score
/// at all. If a signal is unknown, `Option::None` says "we could not measure
/// this" — distinct from "we measured it as zero".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RaritySignal {
    /// Stable identifier, used in output and tests.
    pub name: &'static str,
    /// `0.0` = most common, `1.0` = rarest.
    pub magnitude: f64,
}

impl RaritySignal {
    /// Build a signal, rejecting values outside `0.0..=1.0`.
    ///
    /// The rejection matters more than it looks. A magnitude of `1.4` from a
    /// mis-scaled ratio would otherwise flow into an average and produce a
    /// score above the maximum — a quiet, plausible, wrong number.
    pub fn new(name: &'static str, magnitude: f64) -> Result<Self, DomainError> {
        let in_range = magnitude.is_finite() && (0.0..=1.0).contains(&magnitude);
        if in_range {
            Ok(Self { name, magnitude })
        } else {
            Err(DomainError::OutOfRange {
                what: name,
                value: magnitude,
            })
        }
    }

    /// `None` for an unmeasurable signal.
    pub fn unknown(name: &'static str) -> Option<Self> {
        Self::new(name, 0.0).ok()
    }
}

/// What the signals add up to.
///
/// # Deliberately opaque
///
/// `Rarity` is a value, not a rule. It carries its components so the reason
/// travels with the number, and it does not expose the arithmetic — because
/// that arithmetic is the judgement call it documents itself.
///
/// # Not produced yet
///
/// No combination function exists today, so no `Rarity` is ever constructed:
/// valuations report `rarity: None` and the renderers show it honestly. The
/// type and its band ([`band_of`]) stay because the renderers and the report
/// shape already speak in these terms — scoring rules land here when the
/// evidence pipeline (`community.have`/`want`) is built, with its own
/// decision record.
#[derive(Debug, Clone, PartialEq)]
pub struct Rarity {
    /// `0.0` = common, `1.0` = as rare as we can measure.
    pub score: f64,
    /// The signals that fed the score, in a stable order.
    pub components: Vec<RaritySignal>,
}

impl Rarity {
    /// Human-facing band, used by the text renderer.
    pub fn band(&self) -> RarityBand {
        band_of(self.score)
    }

    /// Signals that were actually measured.
    ///
    /// A score built from two signals is more trustworthy than one built from
    /// five where three were guesses. Exposing the count lets the renderer say
    /// so instead of presenting a bare number.
    pub fn measured_count(&self) -> usize {
        self.components.len()
    }
}

/// Coarse label for presentation only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RarityBand {
    Common,
    Uncommon,
    Rare,
    Grail,
}

impl RarityBand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Common => "common",
            Self::Uncommon => "uncommon",
            Self::Rare => "rare",
            Self::Grail => "grail",
        }
    }
}

/// Threshold policy for banding a `0.0..=1.0` score, stated once so the
/// collection report and the dig TUI cannot drift apart.
///
/// First cut, as noted on [`Rarity::band`]: configurable cutoffs would be a
/// personal-collection-vs-shop decision — see `docs/DECISIONS.md`, Q-05.
pub fn band_of(score: f64) -> RarityBand {
    match score {
        s if s < 0.25 => RarityBand::Common,
        s if s < 0.5 => RarityBand::Uncommon,
        s if s < 0.75 => RarityBand::Rare,
        _ => RarityBand::Grail,
    }
}

// The rarity inputs that are derivable without the network are the *edition*
// signals — "First pressing", "Limited Edition", "Reissue" in the format
// descriptions. They are deliberately unwritten: honest keyword evidence or a
// loud guess, and a guess that looks like a number is worse than `None`.
// Scoring rules (the combination step) land here too, when the evidence
// pipeline (`community.have`/`want`) is built, with its own decision record.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_reject_out_of_range_and_non_finite_values() {
        for bad in [-0.1, 1.1, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                RaritySignal::new("test", bad).is_err(),
                "expected {bad} to be rejected"
            );
        }
        assert!(RaritySignal::new("test", 0.0).is_ok());
        assert!(RaritySignal::new("test", 1.0).is_ok());
        assert!(RaritySignal::new("test", 0.5).is_ok());
    }

    #[test]
    fn the_out_of_range_error_names_the_signal() {
        let error = RaritySignal::new("scarcity", 2.0).unwrap_err();
        assert_eq!(
            error,
            DomainError::OutOfRange {
                what: "scarcity",
                value: 2.0
            }
        );
    }

    #[test]
    fn every_band_renders() {
        for score in [0.0, 0.3, 0.6, 0.9] {
            let rarity = Rarity {
                score,
                components: Vec::new(),
            };
            assert!(!rarity.band().as_str().is_empty());
        }
    }
}
