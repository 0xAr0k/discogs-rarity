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
//! deserves human judgement — yours to write.
//!
//! # Vocabulary
//!
//! - A [`RaritySignal`] is one observable fact, in [0, 1], with a name.
//! - [`RaritySignals`] is the set of facts about one pressing.
//! - A [`Rarity`] is what *you* decide those facts add up to.

use crate::error::DomainError;

use super::release::Release;

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

/// Every fact we can observe about one pressing's rarity.
///
/// # Field philosophy
///
/// Everything is `Option`. The alternative — filling in zeros — makes "nobody
/// owns any" indistinguishable from "we have no data", and those lead to
/// opposite conclusions about what to buy next.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RaritySignals {
    /// How few owners there are relative to a reference cohort, `0..=1`.
    ///
    /// `Discogs.community.have`: a rough census, not a count of physical
    /// copies, and heavily skewed by who bothers to add records.
    pub scarcity: Option<f64>,
    /// How many people want it, normalized, `0..=1`.
    ///
    /// Interest, not scarcity. High demand with high supply is a widely owned
    /// popular record, which is the opposite of what a buyer wants.
    pub demand: Option<f64>,
    /// Whether the pressing is an early or limited edition, `0..=1`.
    ///
    /// `Reissue`/`Limited Edition`/`First pressing` in the format descriptions.
    pub edition: Option<f64>,
    /// Whether the pressing is known to be discontinued, `0..=1`.
    pub discontinued: Option<f64>,
    /// Whether it is still in production, inferred from recent sales.
    pub active_pressing: Option<f64>,
}

/// What the signals add up to.
///
/// # Deliberately opaque
///
/// `Rarity` is a value, not a rule. It carries its components so the reason
/// travels with the number, and it does not expose the arithmetic — because
/// that arithmetic is the judgement call you own.
///
/// See `docs/EXERCISES.md`, exercise 1.
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
        // TODO(you): replace with your own thresholds once `classify` exists.
        // Suggested first cut: <0.25 Common, <0.5 Uncommon, <0.75 Rare,
        // else Grail. Consider making the cutoffs configurable rather than
        // hard-coded — a personal collection and a shop have different needs.
        match self.score {
            s if s < 0.25 => RarityBand::Common,
            s if s < 0.5 => RarityBand::Uncommon,
            s if s < 0.75 => RarityBand::Rare,
            _ => RarityBand::Grail,
        }
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

/// Extract the rarity signals that are derivable from release metadata alone.
///
/// # What this can and cannot know
///
/// Metadata gives us *edition* signals — "First pressing", "Limited Edition",
/// "Reissue" — with no network call. It gives us **nothing** about how many
/// copies exist. That is deliberate: a purely local function is testable with
/// zero fixtures and cannot be wrong about the internet.
///
/// # TODO(you) — exercise 1a
///
/// Implement the edition signal. Sketch:
///
/// 1. Gather every format description for the release (all formats, joined).
/// 2. Assign a magnitude per keyword, strongest evidence first:
///    - `"First pressing"` / `"First Pressing"` → highest
///    - `"Limited Edition"`, `"Numbered"` → high
///    - `"Reissue"`, `"Unofficial Release"` → near zero
///    - `"Original"` → high-ish
/// 3. Combine: take the **max** across formats, not the sum. A limited vinyl
///    bonus alongside a plain CD should not exceed 1.0.
/// 4. Return `None` when no description matched — unmeasurable, not zero.
///
/// Watch out for: case sensitivity (`"first pressing"` vs `"First Pressing"`),
/// and substring false positives (`"Not First Pressing"`). Consider
/// `eq_ignore_ascii_case` on split tokens over whole-substring `contains`.
#[allow(clippy::unnecessary_wraps)]
pub fn edition_signal(_release: &Release) -> Option<RaritySignal> {
    todo!("exercise 1a: derive the edition signal from format descriptions")
}

/// Combine all measured signals into one rarity judgement.
///
/// # TODO(you) — exercise 1b
///
/// This is the heart of the app and it is yours. Constraints worth honouring:
///
/// - **Never exceed `0.0..=1.0`.** Clamp or reject.
/// - **Absence is not zero.** If only `scarcity` is known, the score should not
///   be dragged toward 0 by the missing signals; weight by *measured* signals,
///   not by all of them.
/// - **Be explainable.** Return the components alongside the score so the text
///   output can justify the number.
/// - **Weight scarcity above demand.** A pressing that is scarce *and* wanted
///   is what a collection is missing. Abundant and wanted is just popular.
///
/// Start with a weighted mean over the measured components, then read
/// `src/domain/release.rs` tests for the invariants that must hold.
///
/// Consider instead (and say so in a comment): a geometric mean, a minimum
/// (bottleneck) rule, or a hand-tuned lookup table. All are defensible;
/// pretending there is one correct answer is not.
pub fn classify(signals: &RaritySignals) -> Rarity {
    let _ = signals;
    todo!("exercise 1b: combine signals into a Rarity you can defend")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::identity::ReleaseId;
    use crate::domain::release::{Artist, Format, Label};

    fn release_with(descriptions: &[&str]) -> Release {
        Release {
            id: ReleaseId::new(1).unwrap(),
            title: "Kind of Blue".into(),
            year: Some(1959),
            artists: vec![Artist {
                id: 1,
                name: "Miles Davis".into(),
            }],
            formats: vec![Format {
                name: "Vinyl".into(),
                qty: "1".into(),
                descriptions: descriptions.iter().map(|d| (*d).to_owned()).collect(),
            }],
            labels: vec![Label {
                id: 1,
                name: "Columbia".into(),
                catno: "CL 1355".into(),
            }],
            thumb: String::new(),
            resource_url: String::new(),
            country: None,
            master_id: None,
        }
    }

    /// Convenience for building a signal in tests.
    #[allow(dead_code, reason = "a helper for the exercise tests you will add")]
    fn signal(name: &'static str, magnitude: f64) -> Option<RaritySignal> {
        RaritySignal::new(name, magnitude).ok()
    }

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

    // ---- exercise 1a gates ---------------------------------------------
    // These fail with `todo!` until you implement `edition_signal`.

    #[test]
    fn first_pressing_scores_above_a_plain_pressing() {
        let first = edition_signal(&release_with(&["First Pressing"]));
        let plain = edition_signal(&release_with(&["LP", "Album"]));
        // A plain pressing carries no edition information at all.
        assert_eq!(plain, None, "no edition keywords means unmeasurable");
        let first = first.expect("`First Pressing` must be recognised");
        assert!(first.magnitude > 0.5, "got {}", first.magnitude);
    }

    #[test]
    fn reissue_scores_lower_than_first_pressing() {
        let first = edition_signal(&release_with(&["First Pressing"])).unwrap();
        let reissue = edition_signal(&release_with(&["Reissue"])).unwrap();
        assert!(
            reissue.magnitude < first.magnitude,
            "a reissue must not outrank a first pressing"
        );
    }

    #[test]
    fn edition_detection_ignores_case() {
        assert_eq!(
            edition_signal(&release_with(&["first pressing"])),
            edition_signal(&release_with(&["First Pressing"]))
        );
    }

    #[test]
    fn edition_magnitude_stays_within_bounds_when_formats_disagree() {
        // A limited bonus format alongside a plain one must not push past 1.0.
        let mixed = release_with(&["Limited Edition", "Numbered"]);
        let signal = edition_signal(&mixed).unwrap();
        assert!(
            (0.0..=1.0).contains(&signal.magnitude),
            "magnitude {} escaped 0..=1",
            signal.magnitude
        );
    }

    // ---- exercise 1b gates ---------------------------------------------

    #[test]
    fn classify_keeps_the_score_in_range() {
        for scarcity in [0.0, 0.5, 1.0] {
            let signals = RaritySignals {
                scarcity: Some(scarcity),
                ..RaritySignals::default()
            };
            let rarity = classify(&signals);
            assert!(
                (0.0..=1.0).contains(&rarity.score),
                "score {} escaped 0..=1",
                rarity.score
            );
        }
    }

    #[test]
    fn classify_does_not_treat_missing_signals_as_zero() {
        let high_scarcity_only = classify(&RaritySignals {
            scarcity: Some(1.0),
            ..RaritySignals::default()
        });
        let with_more_signals = classify(&RaritySignals {
            scarcity: Some(1.0),
            demand: Some(0.0),
            edition: Some(0.0),
            discontinued: Some(0.0),
            active_pressing: Some(0.0),
        });
        assert!(
            (high_scarcity_only.score - with_more_signals.score).abs() < 1e-9,
            "unknown signals must not drag a known one toward zero: {} vs {}",
            high_scarcity_only.score,
            with_more_signals.score
        );
    }

    #[test]
    fn more_scarcity_never_scores_lower() {
        let lower = classify(&RaritySignals {
            scarcity: Some(0.2),
            demand: Some(0.5),
            ..RaritySignals::default()
        });
        let higher = classify(&RaritySignals {
            scarcity: Some(0.9),
            demand: Some(0.5),
            ..RaritySignals::default()
        });
        assert!(higher.score >= lower.score, "scoring must be monotonic");
    }

    #[test]
    fn classify_reports_the_components_it_used() {
        let rarity = classify(&RaritySignals {
            scarcity: Some(0.7),
            edition: Some(0.4),
            ..RaritySignals::default()
        });
        assert_eq!(rarity.measured_count(), 2);
        assert!(
            rarity
                .components
                .iter()
                .all(|c| (0.0..=1.0).contains(&c.magnitude))
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
