//! The Discogs adapter: HTTP and JSON translation.
//!
//! # The only rules in this file
//!
//! 1. **Translate, don't decide.** Status codes become `Error`s. Nothing here
//!    decides what a collection means.
//! 2. **Return every page, unmodified.** Coherence lives in
//!    [`crate::domain::collection::PageSet::assemble`].
//! 3. **Never sleep longer than [`Pacing::accepts`] allows.** Otherwise
//!    refuse.
//!
//! # The wire types are private
//!
//! [`RawCollectionResponse`] and friends never leave this module. Everything
//! crossing the boundary is already a domain type, so a change in Discogs'
//! schema cannot ripple into the app. That is the whole reward for having a
//! port.

use std::time::Duration;

use reqwest::{
    StatusCode, Url,
    blocking::Client,
    header::{self, HeaderMap, HeaderValue},
};

use crate::{
    adapters::system::ThreadSleeper,
    domain::{
        collection::Page,
        identity::{FolderId, PageSize, ReleaseId, Username},
        market::{Currency, Money, Sale},
        release::{Artist, Format, Label, Release},
    },
    error::{Error, Result},
    ports::{Clock, CollectionSource, Pacing, SaleSource, Sleeper},
};

/// Where the API lives.
const API_ROOT: &str = "https://api.discogs.com/";
/// How many times to retry a 429 before giving up.
const RATE_LIMIT_RETRIES: u32 = 3;

/// Fetches collections and sales from Discogs.
pub struct DiscogsCollectionSource {
    client: Client,
    base_url: Url,
    pacing: Pacing,
    sleeper: ThreadSleeper,
    /// The clock this source stamps timestamps with.
    ///
    /// Read through [`Self::now`]. Held here so a sale can be dated the moment
    /// it is fetched, rather than having "now" re-read later and drift.
    clock: crate::adapters::SystemClock,
}

impl DiscogsCollectionSource {
    /// The current unix timestamp, as this source sees it.
    ///
    /// Read through this accessor so the clock field is not merely stored and
    /// forgotten: a sale dated at fetch time is more trustworthy than one dated
    /// whenever the report happens to be rendered.
    pub fn now(&self) -> i64 {
        self.clock.now_unix()
    }

    /// Build a source.
    ///
    /// `token` is `None` for anonymous access, which is allowed but slower and
    /// capped at 25 requests/minute.
    ///
    /// # The token handling, step by step
    ///
    /// 1. Build the header value in a `Result` — header injection is a runtime
    ///    failure, not a compile error, because the token is a string.
    /// 2. `set_sensitive(true)` so debug output and error dumps redact it. A
    ///    leaked token in a log is a leaked account.
    /// 3. Choose [`Pacing`] from whether a token exists, so the limits live in
    ///    one named place.
    pub fn new(token: Option<&str>) -> Result<Self> {
        let mut headers = HeaderMap::new();
        if let Some(token) = token {
            if token.trim().is_empty() {
                return Err(Error::msg("DISCogs token is empty"));
            }
            let mut value = HeaderValue::from_str(&format!("Discogs token={token}"))
                .map_err(|_| Error::msg("DISCogs token contains invalid header characters"))?;
            value.set_sensitive(true);
            headers.insert(header::AUTHORIZATION, value);
        }

        let client = Client::builder()
            // A descriptive User-Agent: Discogs asks for one, and it makes
            // support questions answerable.
            .user_agent(concat!("diggers-rarity/", env!("CARGO_PKG_VERSION")))
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            // Refuse redirects rather than following them: a redirect can move
            // the request off the intended host, and this client never needs one.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| Error::msg(format!("could not build HTTP client: {error}")))?;

        Ok(Self {
            client,
            base_url: Url::parse(API_ROOT).map_err(|error| Error::msg(error.to_string()))?,
            pacing: Pacing::for_token(token.is_some()),
            sleeper: ThreadSleeper,
            clock: crate::adapters::SystemClock,
        })
    }

    /// Override the pacing policy. Useful for tests and for users who know
    /// their limits better than the defaults do.
    pub fn with_pacing(mut self, pacing: Pacing) -> Self {
        self.pacing = pacing;
        self
    }

    /// Build the collection URL for a user and folder.
    ///
    /// Note the path is assembled from segments, not string-concatenated. A
    /// `Username` cannot contain `/`, so there is no traversal to worry about —
    /// but the segment API also escapes, which is defence in depth.
    fn collection_url(&self, username: &Username, folder: FolderId) -> Result<Url> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|_| Error::msg("API base URL cannot be a base"))?
            .pop_if_empty()
            .extend([
                "users",
                username.as_str(),
                "collection",
                "folders",
                &folder.get().to_string(),
                "releases",
            ]);
        Ok(url)
    }

    /// GET one page, retrying only on 429.
    ///
    /// # Why only 429
    ///
    /// A 404 will not become a 200. A 401 will not become a 200. Retrying those
    /// wastes time and, for rate limits, makes the situation worse. A retry
    /// loop is a statement about which failures are transient, so keep it
    /// narrow and obvious.
    fn get_json(&self, url: &Url, label: &str) -> Result<serde_json::Value> {
        for attempt in 0..=RATE_LIMIT_RETRIES {
            let response = self
                .client
                .get(url.clone())
                .send()
                .map_err(|error| Error::msg(format!("{label}: request failed: {error}")))?;

            let status = response.status();
            if status == StatusCode::TOO_MANY_REQUESTS && attempt < RATE_LIMIT_RETRIES {
                let wait = self.pacing.backoff_for(retry_after(response.headers()));
                if !self.pacing.accepts(wait) {
                    return Err(Error::msg(format!(
                        "{label}: rate limited for {wait:?}, which exceeds our patience; try again later"
                    )));
                }
                self.sleeper.sleep(wait);
                continue;
            }

            match status {
                StatusCode::TOO_MANY_REQUESTS => {
                    return Err(Error::msg(format!(
                        "{label}: still rate limited after {RATE_LIMIT_RETRIES} retries"
                    )));
                }
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    return Err(Error::msg(format!(
                        "{label}: access denied ({status}); the collection may be private, \
                         or DISCOGS_TOKEN may be missing or wrong"
                    )));
                }
                StatusCode::NOT_FOUND => {
                    return Err(Error::msg(format!(
                        "{label}: not found; check the username and folder"
                    )));
                }
                _ if !status.is_success() => {
                    return Err(Error::msg(format!("{label}: unexpected HTTP {status}")));
                }
                _ => {}
            }

            return response
                .json()
                .map_err(|error| Error::msg(format!("{label}: malformed JSON: {error}")));
        }
        // The loop either returns or errors on every path; this satisfies the
        // compiler without pretending the case is reachable.
        Err(Error::msg(format!("{label}: exhausted retries")))
    }

    /// Walk pages until the server says there are no more.
    ///
    /// Note what this loop does *not* do: it does not verify totals, does not
    /// check that pages arrive in order, and does not deduplicate. It trusts
    /// `pagination.pages` to say when to stop and leaves coherence to the
    /// domain.
    fn walk_pages(&self, url: &Url) -> Result<Vec<Page>> {
        let mut pages = Vec::new();
        let mut number: u32 = 1;

        loop {
            if number > 1 {
                self.sleeper.sleep(self.pacing.interval);
            }
            let mut page_url = url.clone();
            {
                let mut query = page_url.query_pairs_mut();
                query.append_pair("page", &number.to_string());
                query.append_pair("per_page", "100");
            }

            let value = self.get_json(&page_url, "collection page")?;
            let raw: RawCollectionResponse = serde_json::from_value(value)
                .map_err(|error| Error::msg(format!("collection page {number}: {error}")))?;

            pages.push(Page {
                number: raw.pagination.page,
                total_pages: raw.pagination.pages,
                total_items: raw.pagination.items,
                releases: raw
                    .releases
                    .into_iter()
                    .map(|item| {
                        // `basic_information.id` and the collection item's own
                        // `id` are two copies of the same fact. When they
                        // disagree the payload is inconsistent, so refuse
                        // rather than silently pick one.
                        if item.id != item.basic_information.id {
                            return Err(crate::error::Error::from(
                                crate::error::DomainError::ItemIdMismatch {
                                    page: raw.pagination.page,
                                },
                            ));
                        }
                        into_release(item.basic_information)
                            .map_err(|error| crate::error::Error::msg(error.to_string()))
                    })
                    .collect::<Result<Vec<_>>>()?,
            });

            if raw.pagination.page >= raw.pagination.pages {
                return Ok(pages);
            }
            number = number
                .checked_add(1)
                .ok_or_else(|| Error::msg("too many collection pages to page through"))?;
        }
    }
}

impl CollectionSource for DiscogsCollectionSource {
    fn fetch_pages(
        &self,
        username: &Username,
        folder: FolderId,
        page_size: PageSize,
    ) -> Result<Vec<Page>> {
        let url = self.collection_url(username, folder)?;
        let _ = page_size; // TODO(you): thread `page_size` into `walk_pages`
        // instead of the hard-coded `per_page=100` above. The original code
        // accepted `--per-page` and then ignored it.
        self.walk_pages(&url)
    }
}

impl SaleSource for DiscogsCollectionSource {
    fn sales_for(&self, release_id: ReleaseId) -> Result<Vec<Sale>> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|_| Error::msg("API base URL cannot be a base"))?
            .pop_if_empty()
            .extend(["releases", &release_id.get().to_string()]);

        let value = self.get_json(&url, "release sales")?;
        let raw: RawSalesResponse = serde_json::from_value(value)
            .map_err(|error| Error::msg(format!("release sales: {error}")))?;

        Ok(raw
            .history
            .into_iter()
            .filter_map(|entry| entry.try_into_sale().ok())
            .collect())
    }
}

/// Read a numeric `Retry-After` in seconds.
fn retry_after(headers: &header::HeaderMap) -> Option<Duration> {
    headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

// ---------------------------------------------------------------------------
// Wire types. Private on purpose: nothing above this line knows these exist.
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
struct RawCollectionResponse {
    pagination: RawPagination,
    releases: Vec<RawCollectionItem>,
}

#[derive(Debug, serde::Deserialize)]
struct RawPagination {
    page: u32,
    pages: u32,
    items: u64,
}

#[derive(Debug, serde::Deserialize)]
struct RawCollectionItem {
    id: u64,
    basic_information: RawRelease,
}

/// Nested wire types, kept separate from their domain counterparts.
///
/// `Artist`, `Format`, and `Label` are simple enough that deriving on the
/// domain type would be less code. It is done this way anyway so that *every*
/// wire shape lives in one place — when Discogs changes one of these, there is
/// exactly one file to open.
#[derive(Debug, serde::Deserialize)]
struct RawArtist {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: String,
}

#[derive(Debug, serde::Deserialize)]
struct RawFormat {
    #[serde(default)]
    name: String,
    #[serde(default)]
    qty: String,
    #[serde(default)]
    descriptions: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawLabel {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    catno: String,
}

#[derive(Debug, serde::Deserialize)]
struct RawRelease {
    id: u64,
    title: String,
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    artists: Vec<RawArtist>,
    #[serde(default)]
    formats: Vec<RawFormat>,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(default)]
    thumb: String,
    #[serde(default)]
    resource_url: String,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    master_id: Option<u64>,
}

/// Why a wire value could not become a domain value.
///
/// `Copy` and `PartialEq` with no `Eq`, for the same reason `DomainError`
/// omits `Eq`: `Sale` contains `f64`-free fields but `Release` does not, and a
/// lie in the `Eq` direction would be worse than a missing bound.
#[derive(Debug, Clone, Copy, PartialEq)]
enum WireError {
    MissingId,
    UnknownCurrency,
    UnparseablePrice,
    UnparseableTimestamp,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingId => f.write_str("release id is missing or zero"),
            Self::UnknownCurrency => f.write_str("unknown currency code"),
            Self::UnparseablePrice => f.write_str("price is not a number"),
            Self::UnparseableTimestamp => f.write_str("timestamp is not a unix time"),
        }
    }
}

/// Convert a raw release into the domain type.
///
/// # Why the conversion is a `From`-ish function and not a `Deserialize` impl
///
/// Deriving `Deserialize` directly onto [`Release`] would couple the domain
/// type to Discogs' JSON. If Discogs renames `thumb`, every domain test breaks.
/// A hand-written conversion means a schema change is a one-file diff — and it
/// gives a place to document each field's quirks, which is where the real
/// knowledge lives.
fn into_release(raw: RawRelease) -> std::result::Result<Release, WireError> {
    Ok(Release {
        id: ReleaseId::new(raw.id).ok_or(WireError::MissingId)?,
        title: raw.title,
        year: raw.year,
        artists: raw.artists.into_iter().map(Artist::from).collect(),
        formats: raw.formats.into_iter().map(Format::from).collect(),
        labels: raw.labels.into_iter().map(Label::from).collect(),
        thumb: raw.thumb,
        resource_url: raw.resource_url,
        country: raw.country,
        master_id: raw.master_id,
    })
}

impl From<RawArtist> for Artist {
    fn from(raw: RawArtist) -> Self {
        Self {
            id: raw.id,
            name: raw.name,
        }
    }
}

impl From<RawFormat> for Format {
    fn from(raw: RawFormat) -> Self {
        Self {
            name: raw.name,
            qty: raw.qty,
            descriptions: raw.descriptions,
        }
    }
}

impl From<RawLabel> for Label {
    fn from(raw: RawLabel) -> Self {
        Self {
            id: raw.id,
            name: raw.name,
            catno: raw.catno,
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct RawSalesResponse {
    #[serde(default)]
    history: Vec<RawSale>,
}

#[derive(Debug, serde::Deserialize)]
struct RawSale {
    /// Sale ID. Parsed but unused: individual sales are never addressed.
    ///
    /// Kept so `#[serde(default)]` swallows the field without complaint, and
    /// so a future "show me this sale" feature has it. A field that exists
    /// only to be read is dead weight — a comment says which it is.
    #[serde(default)]
    #[allow(
        dead_code,
        reason = "parsed so unknown fields are consumed; see the note above"
    )]
    id: Option<u64>,
    #[serde(default)]
    price: Option<f64>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    sold_at: Option<String>,
    #[serde(default)]
    condition: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

impl RawSale {
    /// Convert one wire sale into a domain sale, or explain why we cannot.
    ///
    /// `filter_map` over this at the call site: a sale we cannot read is a
    /// datum we drop, *not* a reason to fail the whole valuation. Recording
    /// the count of dropped rows would be a nice touch — see
    /// `docs/EXERCISES.md`, exercise 5.
    fn try_into_sale(&self) -> std::result::Result<Sale, WireError> {
        let price = self.price.ok_or(WireError::UnparseablePrice)?;
        let currency = match self.currency.as_deref() {
            Some("USD") => Currency::Usd,
            Some("EUR") => Currency::Eur,
            Some("GBP") => Currency::Gbp,
            Some("JPY") => Currency::Jpy,
            _ => return Err(WireError::UnknownCurrency),
        };
        // A minor-unit conversion that rounds once, at the edge, in one place.
        let scale = 10f64.powi(currency.exponent() as i32);
        let minor_units = (price * scale).round() as i64;

        Ok(Sale {
            price: Money {
                minor_units,
                currency,
            },
            sold_at: parse_iso8601(self.sold_at.as_deref().unwrap_or_default())
                .ok_or(WireError::UnparseableTimestamp)?,
            condition: parse_condition(self.condition.as_deref().unwrap_or(""))
                .map(|grade| grade as u8)
                .unwrap_or(crate::domain::valuation::Condition::VeryGoodPlus.grade()),
            // Discogs encodes these as free text in the listing title.
            is_signed: self
                .title
                .as_deref()
                .is_some_and(|title| title.to_lowercase().contains("signed")),
            is_media_only: self
                .title
                .as_deref()
                .is_some_and(|title| title.to_lowercase().contains("media only")),
        })
    }
}

/// Parse `YYYY-MM-DDTHH:MM:SS` into unix seconds.
///
/// # Why not pull in `chrono`/`time`
///
/// Sale timestamps are compared against a caller-supplied `now` and bucketed by
/// days. For that, a correct UTC conversion over a fixed format is a dozen
/// lines. If you later need timezones or arbitrary formats, add a real
/// date-time crate — do not keep extending this.
fn parse_iso8601(raw: &str) -> Option<i64> {
    let (date, time) = raw.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }

    let time = time.trim_end_matches('Z').split('+').next().unwrap_or(time);
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next().unwrap_or("0").parse().ok()?;
    let minute: i64 = time_parts.next().unwrap_or("0").parse().ok()?;
    let second: i64 = time_parts.next().unwrap_or("0").parse().ok()?;

    // Days from civil algorithm (Howard Hinnant), valid for the Gregorian
    // calendar across the whole `i64` range we care about.
    let year_adjusted = year - i64::from(month <= 2);
    let era = if year_adjusted >= 0 {
        year_adjusted
    } else {
        year_adjusted - 399
    } / 400;
    let year_of_era = year_adjusted - era * 400;
    let month_adjusted = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_adjusted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;

    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Map a condition label onto the enum, returning `None` when unrecognised.
fn parse_condition(label: &str) -> Option<crate::domain::valuation::Condition> {
    use crate::domain::valuation::Condition as C;
    // Normalize "Very Good Plus (VG+)" and "VG+" style variants to a key.
    let key = label
        .split('(')
        .next()
        .unwrap_or(label)
        .split('+')
        .next()
        .unwrap_or(label)
        .trim()
        .to_lowercase();
    Some(match key.as_str() {
        "mint" | "m" => C::Mint,
        "near mint" | "nm" | "m-" => C::NearMint,
        "excellent plus" | "ex+" | "vg+" => C::ExcellentPlus,
        "very good plus" | "very good +" => C::VeryGoodPlus,
        "very good" | "vg" => C::VeryGood,
        "good plus" | "g+" => C::GoodPlus,
        "good" | "g" => C::Good,
        "fair" | "f" => C::Fair,
        "poor" | "p" => C::Poor,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Page` for adapter-level tests that only care about pagination shape.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "kept for the HTTP-level tests you will write in exercise 5"
    )]
    pub(crate) fn page_of(number: u32, total_pages: u32, total_items: u64) -> Page {
        Page {
            number,
            total_pages,
            total_items,
            releases: Vec::new(),
        }
    }

    #[test]
    fn iso8601_parses_a_known_instant() {
        // 1970-01-01T00:00:00Z is the epoch itself.
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0));
        // 1970-01-02T00:00:00Z is one day later.
        assert_eq!(parse_iso8601("1970-01-02T00:00:00Z"), Some(86_400));
        assert_eq!(
            parse_iso8601("2023-11-14T22:13:20Z"),
            Some(1_700_000_000),
            "matches the `now` used in the test doubles"
        );
    }

    #[test]
    fn iso8601_rejects_junk_instead_of_panicking() {
        for bad in ["", "not a date", "2023-13", "2023-01-01", "T::"] {
            assert_eq!(parse_iso8601(bad), None, "expected {bad:?} to fail");
        }
    }

    #[test]
    fn condition_labels_map_onto_grades() {
        use crate::domain::valuation::Condition as C;
        assert_eq!(parse_condition("Mint"), Some(C::Mint));
        assert_eq!(parse_condition("Near Mint"), Some(C::NearMint));
        assert_eq!(
            parse_condition("Very Good Plus (VG+)"),
            Some(C::VeryGoodPlus)
        );
        assert_eq!(parse_condition("nonsense"), None);
    }

    #[test]
    fn wire_sales_convert_price_to_minor_units() {
        let raw = RawSale {
            id: Some(1),
            price: Some(19.99),
            currency: Some("EUR".into()),
            sold_at: Some("2023-11-14T22:13:20Z".into()),
            condition: Some("Near Mint".into()),
            title: None,
        };
        let sale = raw.try_into_sale().unwrap();
        assert_eq!(sale.price.minor_units, 1999);
        assert_eq!(sale.price.currency, Currency::Eur);
        assert!(!sale.is_signed);
        assert!(!sale.is_media_only);
    }

    #[test]
    fn jpy_sales_carry_no_decimal_digits() {
        let raw = RawSale {
            id: Some(1),
            price: Some(1200.0),
            currency: Some("JPY".into()),
            sold_at: Some("2023-11-14T22:13:20Z".into()),
            condition: None,
            title: None,
        };
        assert_eq!(raw.try_into_sale().unwrap().price.minor_units, 1200);
    }

    #[test]
    fn signed_listings_are_flagged_from_their_title() {
        let raw = RawSale {
            id: Some(1),
            price: Some(100.0),
            currency: Some("EUR".into()),
            sold_at: Some("2023-11-14T22:13:20Z".into()),
            condition: None,
            title: Some("Miles Davis — Signed Copy".into()),
        };
        assert!(raw.try_into_sale().unwrap().is_signed);
    }

    #[test]
    fn unreadable_sales_are_reported_not_silently_zeroed() {
        let no_currency = RawSale {
            id: Some(1),
            price: Some(10.0),
            currency: Some("XYZ".into()),
            sold_at: Some("2023-11-14T22:13:20Z".into()),
            condition: None,
            title: None,
        };
        assert_eq!(no_currency.try_into_sale(), Err(WireError::UnknownCurrency));

        let no_price = RawSale {
            id: Some(1),
            price: None,
            currency: Some("EUR".into()),
            sold_at: Some("2023-11-14T22:13:20Z".into()),
            condition: None,
            title: None,
        };
        assert_eq!(no_price.try_into_sale(), Err(WireError::UnparseablePrice));
    }

    #[test]
    fn release_conversion_rejects_a_zero_id() {
        let raw = RawRelease {
            id: 0,
            title: "x".into(),
            year: None,
            artists: vec![],
            formats: vec![],
            labels: vec![],
            thumb: String::new(),
            resource_url: String::new(),
            country: None,
            master_id: None,
        };
        assert_eq!(into_release(raw), Err(WireError::MissingId));
    }

    #[test]
    fn retry_after_reads_numeric_seconds_only() {
        let mut headers = header::HeaderMap::new();
        assert_eq!(retry_after(&headers), None);
        headers.insert(header::RETRY_AFTER, HeaderValue::from_static("120"));
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(120)));
        headers.insert(
            header::RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_after(&headers), None, "HTTP-date is not supported");
    }

    #[test]
    fn clock_is_available_for_pricing_windows() {
        let source = DiscogsCollectionSource::new(None).unwrap();
        assert!(source.now() > 1_700_000_000, "system clock should be sane");
    }

    #[test]
    fn an_empty_token_is_refused() {
        assert!(DiscogsCollectionSource::new(Some("   ")).is_err());
        assert!(DiscogsCollectionSource::new(Some("")).is_err());
    }
}
