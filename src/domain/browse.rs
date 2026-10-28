//! The dig domain: browsing by artist, release or label, and what the
//! terminal shows about each.
//!
//! # Why a second domain module
//!
//! The collection flow answers "what do I own and what is it worth". This
//! module answers a different question — "what is out there, how rare is it,
//! and what is it going for" — where the unit is a *release in the wild*, not
//! a pressing you own. Same layering rules ([`crate::domain::mod`]): pure
//! functions, no I/O, no `unwrap`, newtypes at the edges.
//!
//! # The relationship to `domain::rarity`
//!
//! [`RaritySignals`] in `domain::rarity` would model the *inputs* to a rarity
//! judgement about an owned pressing; the judgement itself — and any want
//! pressure beyond the bounded score below — is scoring work that has its own
//! decision to make first. The dig view needs something that pipeline does not
//! yet ship: a bounded want-pressure score for arbitrary releases
//! ([`want_pressure`]). It is deliberately a separate function in a separate
//! module; if a future `classify` subsumes it, replace the call sites and
//! delete this.

use std::fmt;

use super::identity::{ArtistId, LabelId, ReleaseId};
use super::market::Money;
use super::release::Release;
use crate::error::DomainError;

/// The three kinds of Discogs entity the digger can look up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityKind {
    Artist,
    Release,
    Label,
}

impl EntityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Artist => "artist",
            Self::Release => "release",
            Self::Label => "label",
        }
    }

    /// Parse a `type` parameter or a stored wantlist kind.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "artist" => Some(Self::Artist),
            "release" => Some(Self::Release),
            "label" => Some(Self::Label),
            _ => None,
        }
    }
}

impl fmt::Display for EntityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Community copy counts from the release endpoint (`community.have/want`).
///
/// `None` fields in the UI mean "not fetched yet" — never `0`, which would
/// read as "Discogs says nobody owns this".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CommunityStats {
    pub have: u64,
    pub want: u64,
}

/// Want pressure on a `0.0..=1.0` scale: how many people want a release
/// relative to how many report owning it.
///
/// # Why this shape and not `want÷have`
///
/// The ratio explodes when `have` is small (a grail with 2 owners and 50
/// wanters is "25×"), which then needs an arbitrary cap. `want÷(want+have)` is
/// bounded, monotonic in `want`, and readable: 0.5 means "as many wanters as
/// owners". `want + have == 0` means Discogs has no data, not "worthless", so
/// the caller distinguishes absent stats from zeroed ones.
///
/// ```text
/// pressure = want / (want + have)
/// ```
pub fn want_pressure(stats: CommunityStats) -> f64 {
    let total = stats.have.saturating_add(stats.want);
    if total == 0 {
        return 0.0;
    }
    // Counts far past 2^53 lose integer precision in the cast; want/have
    // counts of that size are not a realistic Discogs payload.
    stats.want as f64 / total as f64
}

/// Whether a release list row is one of "the vinyls".
///
/// # Why inclusion rather than exclusion
///
/// The Bun prototype (`index.ts` in the old `diggers-api` repo) excluded
/// formats containing `File` and `CD`. The digger's ask is narrower — "all the
/// vinyls they released" — and a format the API labels `Vinyl` is exactly
/// that; the exclusion rule would let `Cassette` and `Vinyl`-less oddities
/// through. Rows whose format string never says `Vinyl` are filtered out at
/// the adapter boundary, so a vinyl crate digger never scrolls past CDs.
pub fn is_vinyl_format(format: &str) -> bool {
    format.to_ascii_lowercase().contains("vinyl")
}

/// One row of an artist or label release table.
///
/// [`CatalogRow::stats`] and [`CatalogRow::lowest_price`] start as `None` and
/// are filled **in place** when the release detail has been fetched (each row
/// links to the same full-detail fetch the details screen uses — one fetch,
/// one cache entry, no re-parse).
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogRow {
    pub id: ReleaseId,
    /// `true` for `type: master` rows the wire leaves as album templates.
    /// Masters open through their `main_release` (which this id holds), so
    /// unlike a plain release id they say *how* the row came to be.
    pub is_master: bool,
    pub title: String,
    pub artist: String,
    pub year: Option<u32>,
    pub format: String,
    pub label: String,
    pub catno: String,
    pub stats: Option<CommunityStats>,
    pub lowest_price: Option<Money>,
    /// The enrichment claim, not a content claim: this row has had its one
    /// detail fetch. Rows whose detail carries no community and no price
    /// would otherwise look unfilled forever and re-enter the enrich
    /// channel every tick — the marker is what makes "enrich once" true.
    pub enriched: bool,
}

impl CatalogRow {
    /// The want-pressure score on the display scale `0..=100`, `None` until
    /// the stats are known today (wire stats or an enrichment).
    pub fn rarity_display(&self) -> Option<u16> {
        self.stats
            .map(|s| (want_pressure(s) * 100.0).round().min(100.0) as u16)
    }
}

/// One page of rows, carrying the server's own pagination claims.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogPage {
    pub page: u32,
    pub pages: u32,
    pub rows: Vec<CatalogRow>,
}

/// One page of search hits. Search returns *mixed* entity kinds, so it gets
/// a page type of its own: a [`CatalogPage`] of rows would force both callers
/// to pretend every hit is a release row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchPage {
    pub page: u32,
    pub pages: u32,
    pub hits: Vec<SearchResult>,
}

/// An artist, as the details screen shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistProfile {
    pub id: ArtistId,
    pub name: String,
    pub realname: Option<String>,
    pub profile: Option<String>,
    pub urls: Vec<String>,
    pub namevariations: Vec<String>,
    pub aliases: Vec<String>,
    pub members: Vec<String>,
}

/// A record label, as the details screen shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelProfile {
    pub id: LabelId,
    pub name: String,
    pub profile: Option<String>,
    pub contact_info: Option<String>,
    pub urls: Vec<String>,
    pub sublabels: Vec<String>,
    pub parent_label: Option<String>,
}

/// One tracklist entry. `position` keeps Discogs' free-form numbering
/// (`A1`, `CD-2`, `4`, `Video`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub position: String,
    pub title: String,
    pub duration: String,
}

/// Everything the release endpoint says about one release.
///
/// # Why a wrapper, not a fat struct
///
/// [`Release`] already exists and the collection flow owns it; duplicating its
/// ten fields into a browse-specific struct would be a second definition of
/// "what a release is" that drifts. The detail wraps the domain type it
/// extends. Fields both flows need stay on `Release`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseDetail {
    pub release: Release,
    /// Free-text release date, e.g. `"1969-08-30"` or `"1969"`. Discogs
    /// formats this howsoever the submitter typed it.
    pub released: Option<String>,
    pub genres: Vec<String>,
    pub styles: Vec<String>,
    pub tracklist: Vec<Track>,
    /// Barcode / matrix numbers, pre-joined for display.
    pub identifiers: Vec<String>,
    pub notes: Option<String>,
    pub community: Option<CommunityStats>,
    /// `(average, count)` when anyone rated it.
    pub community_rating: Option<(f64, u32)>,
    /// Cheapest live listing, in USD (`?curr_abbr=USD`). Not a market median —
    /// `docs/DISCOGS.md` § value explains the honest gap.
    pub lowest_price: Option<Money>,
    pub num_for_sale: Option<u32>,
}

impl ReleaseDetail {
    /// `Artist — Title (year)`, matching [`Release::describe`]'s habits.
    pub fn describe(&self) -> String {
        let artist = self
            .release
            .artists
            .first()
            .map(|a| a.name.as_str())
            .unwrap_or("Unknown artist");
        match self.release.year {
            Some(year) => format!("{artist} — {} ({year})", self.release.title),
            None => format!("{artist} — {}", self.release.title),
        }
    }

    /// The want-pressure display score, `None` when Discogs has no stats.
    pub fn rarity_display(&self) -> Option<u16> {
        self.community
            .map(|s| (want_pressure(s) * 100.0).round().min(100.0) as u16)
    }
}

/// One search hit, already resolved to something openable.
///
/// Masters are resolved to their `main_release` by the adapter when the API
/// provides one, and dropped otherwise (`docs/DECISIONS.md`, Q-02) — a
/// `SearchResult` is always one of the three entities the digger can open.
///
/// `stats` and `enriched` mirror [`CatalogRow`]: the search wire carries no
/// community counts, so a release hit starts grey (`stats: None`) and is
/// filled **in place** by the model's enrichment, its `enriched` marker
/// making "asked once" true even when the detail has no community block.
/// Artist and label hits never enrich — no endpoint publishes want/have
/// counts for them, so `stats: None` there is *permanent*, not pending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub id: u64,
    pub kind: EntityKind,
    pub title: String,
    pub year: Option<String>,
    pub formats: Vec<String>,
    pub labels: Vec<String>,
    pub catno: Option<String>,
    pub stats: Option<CommunityStats>,
    pub enriched: bool,
}

impl SearchResult {
    /// The want-pressure display score, `Some` only for a release hit whose
    /// stats are known. The view owns the "not fetched yet" vs "never
    /// exists" distinction; this stays the plain number-or-nothing answer.
    pub fn rarity_display(&self) -> Option<u16> {
        self.stats
            .map(|s| (want_pressure(s) * 100.0).round().min(100.0) as u16)
    }
}

/// A wantlist entry: something worth owning one day.
///
/// The name is captured when the entry is added and kept as-is; if Discogs
/// later deletes the entity, the entry still opens by ID and fails loudly
/// rather than silently emptying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantEntry {
    pub kind: EntityKind,
    /// Unwrapped ID: wantlist entries span all three kinds, and the file on
    /// disk keys on `(kind, id)`.
    pub id: u64,
    pub name: String,
    pub added_at: i64,
}

/// What the home-screen input means.
///
/// # The grammar
///
/// - `a:108713` / `artist:108713` → that artist, whatever the toggle says
/// - `r:249504` / `release:249504` → that release
/// - `l:1` / `label:1` → that label
/// - bare digits → the entity kind selected by the home toggle
/// - anything else → a search query
///
/// Ids over zero; ids parse to `u64`; everything else is a query.
///
/// An id bigger than Discogs uses (say, a 30-digit number) still parses as
/// `u64` until about 1.8 × 10¹⁹ — those become a *browse* of a nonexistent
/// entry and the API answers "not found". That is honest; a query typed as a
/// number ("999999999999999999999999999999") is text again, as text should be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Browse(EntityKind, u64),
    Search(String),
}

/// Ids over zero; ids parse to `u64`; everything else is a query.
pub fn parse_intent(raw: &str, fallback_kind: EntityKind) -> Intent {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Intent::Search(String::new());
    }

    let (kind, rest) = prefixed(trimmed).unwrap_or((fallback_kind, trimmed));

    if let Ok(id) = rest.trim().parse::<u64>()
        && id > 0
    {
        return Intent::Browse(kind, id);
    }
    Intent::Search(trimmed.to_string())
}

fn prefixed(s: &str) -> Option<(EntityKind, &str)> {
    for (prefix, kind) in [
        ("a:", EntityKind::Artist),
        ("artist:", EntityKind::Artist),
        ("r:", EntityKind::Release),
        ("release:", EntityKind::Release),
        ("l:", EntityKind::Label),
        ("label:", EntityKind::Label),
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return Some((kind, rest));
        }
    }
    None
}

/// Validate a raw numeric ID from typed input, mapped to a domain newtype.
pub fn id_of(kind: EntityKind, raw: u64) -> Result<u64, DomainError> {
    let valid = match kind {
        EntityKind::Artist => ArtistId::new(raw).is_some(),
        EntityKind::Label => LabelId::new(raw).is_some(),
        EntityKind::Release => ReleaseId::new(raw).is_some(),
    };
    if valid {
        Ok(raw)
    } else {
        Err(DomainError::ZeroReleaseId)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn want_pressure_is_bounded_and_monotonic() {
        assert_eq!(want_pressure(CommunityStats { have: 0, want: 0 }), 0.0);
        for (want, have) in [
            (1, 0),
            (7, 3),
            (50, 2),
            (5, 95),
            (0, 100),
            (u64::MAX, u64::MAX),
        ] {
            let p = want_pressure(CommunityStats { have, want });
            assert!((0.0..=1.0).contains(&p), "want {want} have {have} gave {p}");
        }
        let (w, h) = (3_u64, 9_u64);
        let base = want_pressure(CommunityStats { have: h, want: w });
        let more = want_pressure(CommunityStats {
            have: h,
            want: w + 1,
        });
        assert!(more >= base, "more wants must never lower the pressure");
        assert_eq!(want_pressure(CommunityStats { have: 50, want: 50 }), 0.5);
    }

    #[test]
    fn only_vinyl_formats_pass() {
        for good in [
            "Vinyl",
            "Vinyl, LP, Album",
            "2x Vinyl",
            "Vinyl, 12\", 45 RPM",
        ] {
            assert!(is_vinyl_format(good), "{good} is vinyl");
        }
        for bad in [
            "Cassette",
            "CD, Album",
            "File, MP3",
            "Acetate",
            "8-Track Cartridge",
        ] {
            assert!(!is_vinyl_format(bad), "{bad} is not vinyl");
        }
    }

    #[test]
    fn intents_parse_by_prefix_over_toggle() {
        assert_eq!(
            parse_intent("a:108713", EntityKind::Label),
            Intent::Browse(EntityKind::Artist, 108713)
        );
        assert_eq!(
            parse_intent("r:249504", EntityKind::Artist),
            Intent::Browse(EntityKind::Release, 249504)
        );
        assert_eq!(
            parse_intent("label:1", EntityKind::Artist),
            Intent::Browse(EntityKind::Label, 1)
        );
        assert_eq!(
            parse_intent("108713", EntityKind::Artist),
            Intent::Browse(EntityKind::Artist, 108713)
        );
        assert_eq!(
            parse_intent(" 249504 ", EntityKind::Release),
            Intent::Browse(EntityKind::Release, 249504)
        );
    }

    #[test]
    fn queries_are_queries_not_errors() {
        assert_eq!(
            parse_intent("Mobb Deep", EntityKind::Artist),
            Intent::Search("Mobb Deep".into())
        );
        assert_eq!(
            parse_intent("r:abc", EntityKind::Artist),
            Intent::Search("r:abc".into())
        );
        assert_eq!(
            parse_intent("99999999999999999", EntityKind::Artist),
            Intent::Browse(EntityKind::Artist, 99_999_999_999_999_999),
            "still a u64: browse and let the API say not found"
        );
        assert_eq!(
            parse_intent("999999999999999999999999999999", EntityKind::Artist),
            Intent::Search("999999999999999999999999999999".into()),
            "bigger than u64 is text, not an error"
        );
        assert_eq!(
            parse_intent("a:0", EntityKind::Artist),
            Intent::Search("a:0".into())
        );
        assert_eq!(
            parse_intent("", EntityKind::Artist),
            Intent::Search(String::new())
        );
    }

    #[test]
    fn id_of_validates_via_the_newtypes() {
        assert!(id_of(EntityKind::Artist, 108713).is_ok());
        assert!(id_of(EntityKind::Artist, 0).is_err());
        assert!(id_of(EntityKind::Release, 1_000_000_001).is_err());
        assert!(id_of(EntityKind::Label, 1).is_ok());
    }

    #[test]
    fn rarity_display_rounds_and_caps() {
        let mut row = CatalogRow {
            id: ReleaseId::new(1).unwrap(),
            is_master: false,
            title: "t".into(),
            artist: "a".into(),
            year: None,
            format: "Vinyl".into(),
            label: String::new(),
            catno: String::new(),
            stats: None,
            lowest_price: None,
            enriched: false,
        };
        assert_eq!(row.rarity_display(), None, "unenriched stays None");
        row.stats = Some(CommunityStats { have: 50, want: 50 });
        assert_eq!(row.rarity_display(), Some(50));
        row.stats = Some(CommunityStats { have: 2, want: 50 });
        assert_eq!(row.rarity_display(), Some(96));
    }
}
