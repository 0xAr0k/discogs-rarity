//! The release domain: what a pressing *is*.
//!
//! # Pressing is not album
//!
//! This is the single most important concept in the app and the easiest to get
//! wrong. A Discogs *master* is the abstract work ("Kind of Blue"). A Discogs
//! *release* is one concrete physical pressing of it — a specific year, label,
//! catalogue number, country, and format.
//!
//! Two copies of the same album on different labels are **not** duplicates; a
//! Japanese first press may be worth fifty times a US reissue. So the app's
//! unit of ownership is `Release`, never "album", and deduplication is keyed
//! on [`ReleaseId`] alone. Getting this wrong makes the valuation output
//! confidently wrong, which is worse than no output.

use std::fmt;

use super::identity::ReleaseId;

/// A pressing, as Discogs describes it.
///
/// # Deserialize is not free of domain rules
///
/// `year` is optional upstream: many entries have no known year. The type
/// models that honestly with `Option<u32>` rather than a sentinel `0`. A
/// sentinel that means "unknown" is a bug waiting for someone to add it to a
/// sum.
/// # Why no `Deserialize` derive
///
/// The Discogs adapter converts `RawRelease` → `Release` by hand rather than
/// deriving. A derive here would tie this domain type to one vendor's JSON: a
/// renamed field would break every domain test. The manual conversion is the
/// seam where schema churn gets absorbed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub id: ReleaseId,
    pub title: String,
    /// `None` when Discogs has no year for this pressing.
    pub year: Option<u32>,
    pub artists: Vec<Artist>,
    pub formats: Vec<Format>,
    pub labels: Vec<Label>,
    /// Cover art URL; empty when Discogs has none.
    pub thumb: String,
    pub resource_url: String,
    /// Country of the pressing. `None` when unspecified — significant, because
    /// pressings are often country-exclusive and that is a rarity signal.
    pub country: Option<String>,
    /// The abstract work this pressing belongs to, when Discogs links one.
    ///
    /// Useful for grouping ("I own three pressings of Kind of Blue") but *not*
    /// an identity: two pressings sharing a master are still two pressings.
    pub master_id: Option<u64>,
}

impl Release {
    /// The catalogue number of the first label, when there is one.
    ///
    /// Cat numbers are the closest thing the data has to a fingerprint, but
    /// they are not unique: labels reuse them across artists and eras. Treat
    /// this as a *hint*, never as an identity.
    pub fn catalogue_number(&self) -> Option<&str> {
        self.labels.first().map(|label| label.catno.as_str())
    }

    /// How many physical copies of this pressing this release counts as.
    ///
    /// `1` for a normal release, `2` for a double LP, and so on — derived from
    /// the first format quantity rather than summing every format, because a
    /// 2xCD + 1xDVD set is one item in a collection, not four.
    pub fn unit_count(&self) -> u32 {
        self.formats
            .first()
            .and_then(|format| format.qty.parse::<u32>().ok())
            .filter(|qty| *qty > 0)
            .unwrap_or(1)
    }

    /// A one-line human label, e.g. `Miles Davis — Kind of Blue (1959)`.
    pub fn describe(&self) -> String {
        let artist = self
            .artists
            .first()
            .map(|artist| artist.name.as_str())
            .unwrap_or("Unknown artist");
        match self.year {
            Some(year) => format!("{artist} — {} ({year})", self.title),
            None => format!("{artist} — {}", self.title),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artist {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pub name: String,
    /// Quantity as a string upstream ("1", "2", …). Kept as a string so the
    /// wire stays lossless; [`Release::unit_count`] parses defensively.
    pub qty: String,
    pub descriptions: Vec<String>,
}

impl Format {
    /// Whether this is a physical carrier, as opposed to a digital or
    /// placeholder entry.
    ///
    /// Digital entries in a physical collection are usually mistakes, and they
    /// distort both value and rarity. This is a first cut, not a rule set:
    /// opening the list to every carrier the wire can name is a decision for
    /// when a real collection does it to us.
    pub fn is_physical(&self) -> bool {
        const DIGITAL: [&str; 6] = [
            "File",
            "Digital",
            "Online",
            "Streaming",
            "Unofficial",
            "Flexi",
        ];
        let haystack = format!("{} {}", self.name, self.descriptions.join(" "));
        !DIGITAL.iter().any(|word| haystack.contains(word))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub id: u64,
    pub name: String,
    pub catno: String,
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name, self.catno)
    }
}

/// How many copies of each pressing a user owns, and which pressing won a tie.
///
/// # Why the *last* copy's details win
///
/// The original code overwrote earlier entries with later ones for the same
/// ID. The reason is worth keeping explicit: Discogs collection entries can
/// carry per-copy metadata (notes, ratings, "have"/"want"), and the entry that
/// carries the most information is not reliably first. The rule is arbitrary —
/// so it is documented here rather than hidden in a `HashMap` assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedRelease {
    /// Number of copies of this pressing in the collection.
    pub copies: u32,
    /// The details kept after deduplication.
    pub release: Release,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A release with only the fields a test cares about, so each test states
    /// its own premise instead of relying on a shared fat fixture.
    fn release_with(formats: Vec<Format>) -> Release {
        Release {
            id: ReleaseId::new(1).unwrap(),
            title: "Kind of Blue".into(),
            year: Some(1959),
            artists: vec![Artist {
                id: 1,
                name: "Miles Davis".into(),
            }],
            formats,
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

    #[test]
    fn unit_count_reads_the_first_format_quantity() {
        let single = release_with(vec![Format {
            name: "Vinyl".into(),
            qty: "1".into(),
            descriptions: vec![],
        }]);
        assert_eq!(single.unit_count(), 1);

        let double = release_with(vec![
            Format {
                name: "Vinyl".into(),
                qty: "2".into(),
                descriptions: vec!["LP".into()],
            },
            Format {
                name: "CD".into(),
                qty: "1".into(),
                descriptions: vec![],
            },
        ]);
        assert_eq!(double.unit_count(), 2, "only the first format counts");
    }

    #[test]
    fn unit_count_falls_back_to_one_on_junk_quantities() {
        let weird = release_with(vec![Format {
            name: "Vinyl".into(),
            qty: "not a number".into(),
            descriptions: vec![],
        }]);
        assert_eq!(weird.unit_count(), 1, "unparseable qty must not yield 0");
    }

    #[test]
    fn describe_handles_a_missing_year_without_lying() {
        let mut undated = release_with(vec![]);
        undated.year = None;
        assert_eq!(undated.describe(), "Miles Davis — Kind of Blue");
    }

    #[test]
    fn catalogue_number_is_none_when_there_are_no_labels() {
        let mut unlabelled = release_with(vec![]);
        unlabelled.labels.clear();
        assert_eq!(unlabelled.catalogue_number(), None);
    }

    #[test]
    fn digital_formats_are_recognised() {
        let digital = Format {
            name: "File".into(),
            qty: "1".into(),
            descriptions: vec![],
        };
        assert!(!digital.is_physical());

        let vinyl = Format {
            name: "Vinyl".into(),
            qty: "1".into(),
            descriptions: vec!["LP, Album".into()],
        };
        assert!(vinyl.is_physical());
    }
}
