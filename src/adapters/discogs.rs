//! The Discogs adapter: HTTP and JSON translation.
//!
//! # The only rules in this file
//!
//! 1. **Translate, don't decide.** Status codes become `Error`s. Nothing here
//!    decides what a collection means.
//! 2. **Return every page, unmodified.** Coherence lives in
//!    [`crate::domain::collection::PageSet::assemble`].
//! 3. **Never spend the wire's budget faster than [`Pacing::cap`]**: the
//!    pacing gate is a token bucket — bursts, then refills at the
//!    documented per-minute rate. Spending faster would just earn 429s.
//!
//! # The wire types are private
//!
//! [`RawCollectionResponse`] and friends never leave this module. Everything
//! crossing the boundary is already a domain type, so a change in Discogs'
//! schema cannot ripple into the app. That is the whole reward for having a
//! port.
//!
//! # One HTTP client, two sources
//!
//! The collection source and the digger's [`DiscogsCatalogSource`] share the
//! same endpoint family, the same token handling, the same rate-limit rules.
//! That machinery lives in [`DiscogsHttp`]; each source embeds it and adds
//! only the endpoints it owns. Two adapters stay two adapters — the shared
//! part is transport, never meaning.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::{
    StatusCode, Url,
    blocking::Client,
    header::{self, HeaderMap, HeaderValue},
};

use crate::{
    adapters::system::ThreadSleeper,
    domain::{
        browse::{
            ArtistProfile, CatalogPage, CatalogRow, CommunityStats, EntityKind, LabelProfile,
            ReleaseDetail, SearchPage, SearchResult, Track, is_vinyl_format,
        },
        collection::Page,
        identity::{ArtistId, FolderId, LabelId, PageSize, ReleaseId, Username},
        market::{Currency, Money, Sale},
        release::{Artist, Format, Label, Release},
    },
    error::{Error, Result},
    ports::{CatalogSource, Clock, CollectionSource, Pacing, SaleSource, Sleeper},
};

/// Where the API lives.
const API_ROOT: &str = "https://api.discogs.com/";
/// How many times to retry a 429 before giving up.
const RATE_LIMIT_RETRIES: u32 = 3;
/// Items per page for catalog listings; Discogs caps at 100.
const CATALOG_PER_PAGE: u64 = 100;
/// Items per page for search. 50 keeps answer pages readable.
const SEARCH_PER_PAGE: u64 = 50;
/// Currency forced on the release endpoint so `lowest_price` is comparable.
const DETAIL_CURRENCY: &str = "USD";

// ---------------------------------------------------------------------------
// Shared transport. Token handling, pacing, 429 policy: nothing here decides
// what any response *means*.
// ---------------------------------------------------------------------------

/// The HTTP machinery both Discogs sources embed.
struct DiscogsHttp {
    client: Client,
    base_url: Url,
    pacing: Pacing,
    sleeper: ThreadSleeper,
    /// Whether the Authorization header was installed; `authenticated()` has
    /// to read state we set, because `Client` does not expose its headers.
    authenticated: bool,
    /// The request budget, shared by every caller — see [`Self::pace`].
    budget: Mutex<RateBudget>,
}

/// How many requests can fire without waiting, however long the session
/// has been quiet. Eight matches the scheduler's cap for enrichment reads.
/// Doc-tested arithmetic in [`accrual_wait`].
const BURST: f64 = 8.0;

/// The pacing gate's account: tokens left, and when the refill last ran.
#[derive(Debug)]
struct RateBudget {
    tokens: f64,
    last: std::time::Instant,
}

/// Seconds until one more token accrues, at `cap` tokens per minute.
///
/// The arithmetic, worked once so the gate stays testable without a
/// sandbox clock: a deficit of `1 − tokens` claims at `cap / 60` tokens
/// per second. `cap = 60`, empty bucket → one full second; `cap = 25` →
/// 2.4s; surplus tokens → no wait at all.
fn accrual_wait(tokens: f64, cap: f64) -> std::time::Duration {
    let deficit = (1.0 - tokens).max(0.0);
    std::time::Duration::from_secs_f64(deficit * 60.0 / cap)
}

impl DiscogsHttp {
    /// Build the shared transport for a token (or none).
    ///
    /// # The token handling, step by step
    ///
    /// 1. Build the header value in a `Result` — header injection is a runtime
    ///    failure, not a compile error, because the token is a string.
    /// 2. `set_sensitive(true)` so debug output and error dumps redact it. A
    ///    leaked token in a log is a leaked account.
    /// 3. Choose [`Pacing`] from whether a token exists, so the limits live in
    ///    one named place.
    fn from_token(token: Option<&str>) -> Result<Self> {
        let mut headers = HeaderMap::new();
        if let Some(token) = token {
            if token.trim().is_empty() {
                return Err(Error::msg("DISCOGS token is empty"));
            }
            let mut value = HeaderValue::from_str(&format!("Discogs token={token}"))
                .map_err(|_| Error::msg("DISCOGS token contains invalid header characters"))?;
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
            authenticated: token.is_some(),
            budget: Mutex::new(RateBudget {
                tokens: BURST,
                last: std::time::Instant::now(),
            }),
        })
    }

    /// Spend one request token — the pacing gate.
    ///
    /// # The bucket, not the gap
    ///
    /// The budget is a token bucket at the documented wire rate
    /// (`Pacing::cap` requests per minute, burst capacity [`BURST`]): it
    /// starts *full*, so opening an artist and the first wave of
    /// enrichment fire at once, then the refill spreads the rest evenly
    /// across the minute — bursts, but never faster than the per-minute
    /// window the server actually enforces. The old gap rule (D-15) made
    /// the limit a *minimum spacing*; the bucket keeps the same honest
    /// per-minute total while spending it in waves.
    ///
    /// # Why one shared gate
    ///
    /// Every request passes through the same mutex — user work and
    /// enrichment threads alike — so the budget is one number, not one per
    /// thread (`docs/DECISIONS.md`, D-15/D-18). A poisoned lock means a
    /// request thread panicked; the tokens are still valid numbers, so
    /// take them.
    fn pace(&self) {
        let cap = f64::from(self.pacing.cap);
        loop {
            let wait = {
                let mut budget = self
                    .budget
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let now = Instant::now();
                // Refill by elapsed time — one token per request-slot the
                // per-minute budget grants — and never above the burst
                // capacity, so idle minutes cannot hoard a flood.
                let elapsed = now.duration_since(budget.last).as_secs_f64();
                budget.tokens = (budget.tokens + elapsed * cap / 60.0).min(BURST);
                budget.last = now;
                if budget.tokens >= 1.0 {
                    budget.tokens -= 1.0;
                    None
                } else {
                    Some(accrual_wait(budget.tokens, cap))
                }
            };
            match wait {
                Some(wait) => self.sleeper.sleep(wait),
                None => return,
            }
        }
    }

    /// Path under the API root. Segments, never string concatenation.
    fn url_for(&self, path: &[&str]) -> Result<Url> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|_| Error::msg("API base URL cannot be a base"))?
            .pop_if_empty()
            .extend(path);
        Ok(url)
    }

    /// Whether a token is configured (search demands one).
    fn authenticated(&self) -> bool {
        self.authenticated
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
                        "{label}: access denied ({status}); the resource may be private, \
                         or DISCOGS_TOKEN may be missing or wrong"
                    )));
                }
                StatusCode::NOT_FOUND => {
                    return Err(Error::msg(format!("{label}: not found; check the id")));
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
}

/// The discogs collection + sales source: what the collection CLI uses.
pub struct DiscogsCollectionSource {
    http: DiscogsHttp,
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
    pub fn new(token: Option<&str>) -> Result<Self> {
        Ok(Self {
            http: DiscogsHttp::from_token(token)?,
            clock: crate::adapters::SystemClock,
        })
    }

    /// Override the pacing policy. Useful for tests and for users who know
    /// their limits better than the defaults do.
    pub fn with_pacing(mut self, pacing: Pacing) -> Self {
        self.http.pacing = pacing;
        self
    }

    /// Build the collection URL for a user and folder.
    ///
    /// Note the path is assembled from segments, not string-concatenated. A
    /// `Username` cannot contain `/`, so there is no traversal to worry about —
    /// but the segment API also escapes, which is defence in depth.
    fn collection_url(&self, username: &Username, folder: FolderId) -> Result<Url> {
        self.http.url_for(&[
            "users",
            username.as_str(),
            "collection",
            "folders",
            &folder.get().to_string(),
            "releases",
        ])
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
                self.http.pace();
            }
            let mut page_url = url.clone();
            {
                let mut query = page_url.query_pairs_mut();
                query.append_pair("page", &number.to_string());
                query.append_pair("per_page", "100");
            }

            let value = self.http.get_json(&page_url, "collection page")?;
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
        // `page_size` is not threaded into `walk_pages` (its `per_page` is
        // hard-coded below): the value is accepted for signature stability,
        // not silently applied — if a user flag ever feeds it, it must
        // actually reach the URL.
        let _ = page_size;
        self.walk_pages(&url)
    }
}

impl SaleSource for DiscogsCollectionSource {
    fn sales_for(&self, release_id: ReleaseId) -> Result<Vec<Sale>> {
        let url = self
            .http
            .url_for(&["releases", &release_id.get().to_string()])?;
        let value = self.http.get_json(&url, "release sales")?;
        sales_from_wire(value, release_id)
    }
}

/// Reduce a release's sale-history payload to domain sales.
///
/// One conversion for both sources (the collection report reads it too), so
/// the shape is learned in exactly one place. Unreadable rows are dropped:
/// one bad datum must not blank a page of evidence.
fn sales_from_wire(value: serde_json::Value, release_id: ReleaseId) -> Result<Vec<Sale>> {
    let raw: RawSalesResponse = serde_json::from_value(value)
        .map_err(|error| Error::msg(format!("release sales {}: {error}", release_id.get())))?;
    Ok(raw
        .history
        .into_iter()
        .filter_map(|entry| entry.try_into_sale().ok())
        .collect())
}

// ---------------------------------------------------------------------------
// The digger's catalog source: artists, labels, release details, search.
// ---------------------------------------------------------------------------

/// Reads the Discogs database for the digger TUI.
///
/// # Session cache
///
/// The worker requests the *same* release detail for two jobs: enriching a
/// table row and rendering the details screen. The cache makes the second
/// fetch a lookup. `Arc<ReleaseDetail>` entries keep cache hits zero-copy at
/// the boundary; the one owned clone happens exactly where the port hands a
/// value on (`release_detail`, whose signature predates the cache — an `Arc`
/// port would be the next step if clones ever show up in a profile).
///
/// Session-scoped on purpose: a cache that never expires is a cache that
/// lies about marketplace prices. `docs/DECISIONS.md`, D-07.
pub struct DiscogsCatalogSource {
    http: DiscogsHttp,
    cache: Mutex<HashMap<ReleaseId, std::sync::Arc<ReleaseDetail>>>,
    /// The same session cache for sale rows: reopening a release sheet is
    /// free, and a session-old price is as good as the detail it sits beside.
    sales_cache: Mutex<HashMap<ReleaseId, std::sync::Arc<Vec<Sale>>>>,
}

impl DiscogsCatalogSource {
    /// Build a source. Anonymous is allowed; only [`CatalogSource::search`]
    /// will refuse it.
    pub fn new(token: Option<&str>) -> Result<Self> {
        Ok(Self {
            http: DiscogsHttp::from_token(token)?,
            cache: Mutex::new(HashMap::new()),
            sales_cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn with_pacing(mut self, pacing: Pacing) -> Self {
        self.http.pacing = pacing;
        self
    }

    /// The cached detail for `id`, when this session already fetched it.
    pub fn cached_detail(&self, id: ReleaseId) -> Option<std::sync::Arc<ReleaseDetail>> {
        self.cache.lock().ok()?.get(&id).map(std::sync::Arc::clone)
    }

    fn cache_detail(&self, id: ReleaseId, detail: ReleaseDetail) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(id, std::sync::Arc::new(detail));
        }
    }

    /// One paced catalog fetch: the shared gap gate, then the request.
    fn paced_get_json(&self, url: Url, label: &str) -> Result<serde_json::Value> {
        self.http.pace();
        self.http.get_json(&url, label)
    }
}

impl CatalogSource for DiscogsCatalogSource {
    fn artist(&self, id: u64) -> Result<ArtistProfile> {
        let url = self.http.url_for(&["artists", &id.to_string()])?;
        let value = self.paced_get_json(url, "artist")?;
        let raw: RawArtistDetail = serde_json::from_value(value)
            .map_err(|error| Error::msg(format!("artist {id}: {error}")))?;
        raw.into_profile()
            .map_err(|error| Error::msg(format!("artist {id}: {error}")))
    }

    fn label(&self, id: u64) -> Result<LabelProfile> {
        let url = self.http.url_for(&["labels", &id.to_string()])?;
        let value = self.paced_get_json(url, "label")?;
        let raw: RawLabelDetail = serde_json::from_value(value)
            .map_err(|error| Error::msg(format!("label {id}: {error}")))?;
        raw.into_profile()
            .map_err(|error| Error::msg(format!("label {id}: {error}")))
    }

    fn release_detail(&self, id: ReleaseId) -> Result<ReleaseDetail> {
        if let Some(cached) = self.cached_detail(id) {
            // One clone per cache hit, at the port boundary — see the struct
            // comment for why none of the interior work re-clones.
            return Ok((*cached).clone());
        }
        let mut url = self.http.url_for(&["releases", &id.get().to_string()])?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("curr_abbr", DETAIL_CURRENCY);
        }
        let value = self.paced_get_json(url, "release detail")?;
        let raw: RawReleaseDetail = serde_json::from_value(value)
            .map_err(|error| Error::msg(format!("release detail {id}: {error}")))?;
        let detail = raw
            .into_detail()
            .map_err(|error| Error::msg(format!("release detail {id}: {error}")))?;
        self.cache_detail(id, detail.clone());
        Ok(detail)
    }

    /// One page of an artist's releases, sorted newest year first (the dig
    /// reads chronologically).
    fn artist_releases(&self, id: u64, page: u32) -> Result<CatalogPage> {
        let mut url = self
            .http
            .url_for(&["artists", &id.to_string(), "releases"])?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("page", &page.to_string())
                .append_pair("per_page", &CATALOG_PER_PAGE.to_string())
                .append_pair("sort", "year")
                .append_pair("sort_order", "desc");
        }
        let value = self.paced_get_json(url, "artist releases")?;
        let raw: RawArtistReleases = serde_json::from_value(value)
            .map_err(|error| Error::msg(format!("artist releases {id}: {error}")))?;
        into_catalog_page(raw.pagination, raw.releases)
    }

    /// The label endpoint's rows have no `type` field — everything is a
    /// release — so they go through their own wire type and join the shared
    /// mapping one step later.
    fn label_releases(&self, id: u64, page: u32) -> Result<CatalogPage> {
        let mut url = self
            .http
            .url_for(&["labels", &id.to_string(), "releases"])?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("page", &page.to_string())
                .append_pair("per_page", &CATALOG_PER_PAGE.to_string());
        }
        let value = self.paced_get_json(url, "label releases")?;
        let raw: RawLabelReleases = serde_json::from_value(value)
            .map_err(|error| Error::msg(format!("label releases {id}: {error}")))?;
        into_label_catalog_page(raw.pagination, raw.releases)
    }

    /// Name search across the whole database.
    ///
    /// # Why this refuses anonymously
    ///
    /// Discogs' search endpoint requires OAuth or a personal token; an
    /// anonymous call is a 401 dressed as a confusing UI state. Refusing up
    /// front, with a message naming the environment variable, is the honest
    /// translation.
    fn search(&self, query: &str, kind: Option<EntityKind>, page: u32) -> Result<SearchPage> {
        if !self.http.authenticated() {
            return Err(Error::msg(
                "search needs a Discogs token; set DISCOGS_TOKEN and restart",
            ));
        }
        let mut url = self.http.url_for(&["database", "search"])?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("q", query);
            if let Some(kind) = kind {
                pairs.append_pair("type", kind.as_str());
            }
            pairs.append_pair("page", &page.to_string());
            pairs.append_pair("per_page", &SEARCH_PER_PAGE.to_string());
        }
        let value = self.paced_get_json(url, "search")?;
        let raw: RawSearchResponse = serde_json::from_value(value)
            .map_err(|error| Error::msg(format!("search: {error}")))?;
        into_search_page(raw.pagination, raw.results)
    }

    fn release_sales(&self, id: ReleaseId) -> Result<Vec<Sale>> {
        if let Some(cached) = self
            .sales_cache
            .lock()
            .ok()
            .and_then(|c| c.get(&id).cloned())
        {
            // One clone per cache hit, at the port boundary — same deal as
            // `cached_detail`.
            return Ok((*cached).clone());
        }
        let url = self.http.url_for(&["releases", &id.get().to_string()])?;
        let value = self.paced_get_json(url, "release sales")?;
        let sales = sales_from_wire(value, id)?;
        if let Ok(mut cache) = self.sales_cache.lock() {
            cache.insert(id, std::sync::Arc::new(sales.clone()));
        }
        Ok(sales)
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
    /// Who sold it. The wire has shown this both as a bare username string
    /// and as an object with `username` inside; accept either, show `…` for
    /// neither. `Option` for anything the wire may omit — the sale happened
    /// regardless of whether we get a name.
    #[serde(default)]
    seller: Option<WireSeller>,
}

/// A wire seller, either shape.
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum WireSeller {
    Bare(String),
    Named {
        #[serde(default)]
        username: String,
    },
}

impl WireSeller {
    fn into_shop(self) -> Option<String> {
        match self {
            Self::Bare(name) => Some(name).filter(|name| !name.is_empty()),
            Self::Named { username } => Some(username).filter(|name| !name.is_empty()),
        }
    }
}

impl RawSale {
    /// Convert one wire sale into a domain sale, or explain why we cannot.
    ///
    /// `filter_map` over this at the call site: a sale we cannot read is a
    /// datum we drop, *not* a reason to fail the whole valuation.
    fn try_into_sale(self) -> std::result::Result<Sale, WireError> {
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
            seller: self.seller.and_then(|shop| shop.into_shop()),
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

// ---------------------------------------------------------------------------
// Digger wire types: artist/label details, catalog listing rows, release
// details, search.
// ---------------------------------------------------------------------------

/// A `{ name }` reference used for aliases and sublabels. The upstream `id`
/// is not declared: serde ignores unknown fields, and nothing above the wire
/// needs it — when that changes, the field comes back here first.
#[derive(Debug, serde::Deserialize)]
struct RawNameRef {
    #[serde(default)]
    name: String,
}

fn names(refs: Vec<RawNameRef>) -> Vec<String> {
    refs.into_iter().map(|r| r.name).collect()
}

#[derive(Debug, serde::Deserialize)]
struct RawArtistDetail {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    realname: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    urls: Vec<String>,
    #[serde(default)]
    namevariations: Vec<String>,
    #[serde(default)]
    aliases: Vec<RawNameRef>,
    #[serde(default)]
    members: Vec<RawNameRef>,
}

impl RawArtistDetail {
    fn into_profile(self) -> std::result::Result<ArtistProfile, WireError> {
        Ok(ArtistProfile {
            id: ArtistId::new(self.id).ok_or(WireError::MissingId)?,
            name: self.name,
            realname: self.realname,
            profile: self.profile,
            urls: self.urls,
            namevariations: self.namevariations,
            aliases: names(self.aliases),
            members: names(self.members),
        })
    }
}

#[derive(Debug, serde::Deserialize)]
struct RawLabelDetail {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    contact_info: Option<String>,
    #[serde(default)]
    urls: Vec<String>,
    #[serde(default)]
    sublabels: Vec<RawNameRef>,
    #[serde(default)]
    parent_label: Option<RawNameRef>,
}

impl RawLabelDetail {
    fn into_profile(self) -> std::result::Result<LabelProfile, WireError> {
        Ok(LabelProfile {
            id: LabelId::new(self.id).ok_or(WireError::MissingId)?,
            name: self.name,
            profile: self.profile,
            contact_info: self.contact_info,
            urls: self.urls,
            sublabels: names(self.sublabels),
            parent_label: self.parent_label.map(|p| p.name),
        })
    }
}

/// One row of `/artists/{id}/releases`. Carries `type` (`release`|`master`).
///
/// The compact artist form is asymmetric: `release` rows publish `format`,
/// `label`, `catno`; `master` rows publish only `main_release` and a
/// community summary under `stats.community.{in_wantlist, in_collection}` —
/// and their ids are master ids, not release ids.
#[derive(Debug, serde::Deserialize)]
struct RawArtistRelease {
    #[serde(default)]
    id: u64,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    artist: String,
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    catno: Option<String>,
    #[serde(default)]
    main_release: Option<u64>,
    #[serde(default)]
    stats: Option<RawStats>,
}

#[derive(Debug, serde::Deserialize)]
struct RawStats {
    #[serde(default)]
    community: Option<RawStatsCommunity>,
}

#[derive(Debug, serde::Deserialize)]
struct RawStatsCommunity {
    #[serde(rename = "in_wantlist", default)]
    want: u64,
    #[serde(rename = "in_collection", default)]
    have: u64,
}

impl RawStatsCommunity {
    /// The wire counts wantlist owners and collection owners; the domain's
    /// want pressure (`want÷(want+have)`) needs no more vocabulary. Mapping
    /// here means the table's want column is real on page one — the detail
    /// fetch still confirms it (and brings the price) later.
    fn into_stats(self) -> CommunityStats {
        CommunityStats {
            want: self.want,
            have: self.have,
        }
    }
}

/// Artist releases endpoint: rows carry `type` (`release` | `master`).
#[derive(Debug, serde::Deserialize)]
struct RawArtistReleases {
    pagination: RawPagination,
    #[serde(default)]
    releases: Vec<RawArtistRelease>,
}

#[derive(Debug, serde::Deserialize)]
struct RawLabelRelease {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    artist: String,
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    catno: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawLabelReleases {
    pagination: RawPagination,
    #[serde(default)]
    releases: Vec<RawLabelRelease>,
}

/// Vinyl filter (D-07/D-04): the digger asked for *the vinyls*. Applied here
/// so nothing above this line ever sees a CD row and needs an opinion.
///
/// Masters keep their rows two ways the filter cannot judge:
/// their `format` is not published on this endpoint (unknown is not "CD"),
/// and their openable id is the `main_release` — a master id is not a
/// release id, so without it the row would open nothing. A master that
/// `main_release`s nowhere is dropped, the search page's rule (Q-02).
fn into_catalog_page(
    pagination: RawPagination,
    rows: Vec<RawArtistRelease>,
) -> Result<CatalogPage> {
    let page = pagination;
    let mut mapped = Vec::with_capacity(
        usize::try_from(page.items)
            .unwrap_or(0)
            .min(CATALOG_PER_PAGE as usize),
    );
    for raw in rows {
        let is_master = raw.kind.as_deref() == Some("master");
        // Judge only the formats the wire publishes. A `release` row without
        // one is unknown too, not a CD by default.
        if !is_master
            && let Some(format) = raw.format.as_ref()
            && !is_vinyl_format(format)
        {
            continue;
        }
        let id_u64 = if is_master {
            raw.main_release
        } else {
            Some(raw.id)
        };
        let id_u64 = match id_u64.filter(|&id| id != 0) {
            Some(id) => id,
            // A master that names no release can never be opened; saying so
            // row by row would be noise. (A `release` row whose id is 0 is a
            // wire violation and fails the page below.)
            None if is_master => continue,
            None => {
                return Err(Error::msg(format!(
                    "release id {} on page {}",
                    raw.id, page.page
                )));
            }
        };
        let id = ReleaseId::new(id_u64)
            .ok_or_else(|| Error::msg(format!("release id {} on page {}", id_u64, page.page)))?;
        let stats = raw.stats.and_then(|s| s.community).map(|c| c.into_stats());
        mapped.push(CatalogRow {
            id,
            is_master,
            title: raw.title,
            artist: raw.artist,
            year: raw.year,
            format: raw.format.unwrap_or_default(),
            label: raw.label.unwrap_or_default(),
            catno: raw.catno.unwrap_or_default(),
            stats,
            lowest_price: None,
            enriched: false,
        });
    }
    Ok(CatalogPage {
        page: page.page,
        pages: page.pages,
        rows: mapped,
    })
}

/// The label endpoint's rows have no `type` field: everything is a release.
fn into_label_catalog_page(
    pagination: RawPagination,
    rows: Vec<RawLabelRelease>,
) -> Result<CatalogPage> {
    let artist_rows = rows
        .into_iter()
        .map(|r| RawArtistRelease {
            id: r.id,
            kind: None,
            title: r.title,
            artist: r.artist,
            year: r.year,
            format: r.format,
            label: None,
            catno: r.catno,
            main_release: None,
            stats: None,
        })
        .collect();
    into_catalog_page(pagination, artist_rows)
}

#[derive(Debug, serde::Deserialize)]
struct RawCommunity {
    #[serde(default)]
    have: u64,
    #[serde(default)]
    want: u64,
    #[serde(default)]
    rating: Option<RawRating>,
}

#[derive(Debug, serde::Deserialize)]
struct RawRating {
    #[serde(default)]
    count: u64,
    #[serde(default)]
    average: f64,
}

#[derive(Debug, serde::Deserialize)]
struct RawTrack {
    #[serde(default)]
    position: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    duration: String,
}

#[derive(Debug, serde::Deserialize)]
struct RawIdentifier {
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawReleaseDetail {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    released: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    artists: Vec<RawArtist>,
    #[serde(default)]
    formats: Vec<RawFormat>,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(default)]
    genres: Vec<String>,
    #[serde(default)]
    styles: Vec<String>,
    #[serde(default)]
    tracklist: Vec<RawTrack>,
    #[serde(default)]
    identifiers: Vec<RawIdentifier>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    community: Option<RawCommunity>,
    #[serde(default)]
    lowest_price: Option<f64>,
    #[serde(default)]
    num_for_sale: Option<u64>,
}

impl RawReleaseDetail {
    fn into_detail(self) -> std::result::Result<ReleaseDetail, WireError> {
        let (community, community_rating) = match self.community {
            Some(c) => (
                Some(CommunityStats {
                    have: c.have,
                    want: c.want,
                }),
                c.rating
                    .filter(|r| r.count > 0)
                    .map(|r| (r.average, u32::try_from(r.count).unwrap_or(u32::MAX))),
            ),
            None => (None, None),
        };
        Ok(ReleaseDetail {
            release: into_release(RawRelease {
                id: self.id,
                title: self.title,
                year: self.year,
                artists: self.artists,
                formats: self.formats,
                labels: self.labels,
                thumb: String::new(),
                resource_url: String::new(),
                country: self.country,
                master_id: None,
            })?,
            released: self.released,
            genres: self.genres,
            styles: self.styles,
            tracklist: self
                .tracklist
                .into_iter()
                .map(|t| Track {
                    position: t.position,
                    title: t.title,
                    duration: t.duration,
                })
                .collect(),
            identifiers: self
                .identifiers
                .into_iter()
                .map(|i| match (i.value, i.description) {
                    (Some(v), Some(d)) if !d.is_empty() => format!("{}: {v} [{d}]", i.r#type),
                    (Some(v), _) => format!("{}: {v}", i.r#type),
                    (None, _) => i.r#type,
                })
                .collect(),
            notes: self.notes,
            community,
            community_rating,
            lowest_price: self.lowest_price.map(money_usd),
            num_for_sale: self
                .num_for_sale
                .map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
        })
    }
}

/// Minor-unit money for the detail endpoint, which is pinned to USD.
///
/// One conversion, one rounding, at the edge — the same rule `RawSale` uses.
fn money_usd(price: f64) -> Money {
    let scale = 10f64.powi(Currency::Usd.exponent() as i32);
    Money {
        minor_units: (price * scale).round() as i64,
        currency: Currency::Usd,
    }
}

#[derive(Debug, serde::Deserialize)]
struct RawSearchResponse {
    pagination: RawPagination,
    #[serde(default)]
    results: Vec<RawSearchResult>,
}

/// A mixed search hit. Masters may carry `main_release` (or `master_id` on
/// some payloads); without a resolvable release the row is dropped — the
/// digger cannot open a master, and pretending otherwise shows a broken row.
#[derive(Debug, serde::Deserialize)]
struct RawSearchResult {
    #[serde(default)]
    id: u64,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    year: Option<String>,
    #[serde(default)]
    format: Vec<String>,
    #[serde(default)]
    label: Vec<String>,
    #[serde(default)]
    catno: Option<String>,
    #[serde(default)]
    master_id: Option<u64>,
    #[serde(default)]
    main_release: Option<u64>,
}

fn into_search_page(
    pagination: RawPagination,
    results: Vec<RawSearchResult>,
) -> Result<SearchPage> {
    // Search returns releases, artists, labels *and* masters in one page. The
    // digger's table shows entities it can open, so this is a filter with a
    // rule worth stating: masters resolve to their main release (Q-02) or go,
    // and release hits that never say `Vinyl` go too — the crate rule, one
    // sentence in `docs/DECISIONS.md`, D-04.
    //
    // Resolving masters can collide with a direct hit on the same release —
    // Discogs indexes both — so the page is deduplicated by (kind, id), first
    // occurrence wins. Duplicated rows on one page are wire noise, not signal.
    let page = pagination;
    let mut mapped = Vec::with_capacity(results.len());
    let mut seen = std::collections::HashSet::new();
    for raw in results {
        if raw.id == 0 {
            continue;
        }
        let (id, kind) = match raw.kind.as_str() {
            "artist" => (raw.id, EntityKind::Artist),
            "label" => (raw.id, EntityKind::Label),
            "release" => (raw.id, EntityKind::Release),
            "master" => match raw.main_release.or(raw.master_id) {
                Some(resolved) if resolved > 0 => (resolved, EntityKind::Release),
                _ => continue,
            },
            _ => continue,
        };
        if !seen.insert((kind, id)) {
            continue;
        }
        // The digger asked for *the vinyls* (D-07/D-04), and the entity
        // path already filters at the boundary — the search path does the
        // same to its release hits: whatever the wire says its formats are
        // and none of them says `Vinyl`, the hit is not one. Hits that
        // publish no formats — resolved masters, mostly — have nothing to
        // judge; unknown is not "CD", so they stay.
        if kind == EntityKind::Release
            && !raw.format.is_empty()
            && !raw.format.iter().any(|format| is_vinyl_format(format))
        {
            continue;
        }
        mapped.push(SearchResult {
            id,
            kind,
            title: raw.title,
            year: raw.year,
            formats: raw.format,
            labels: raw.label,
            catno: raw.catno,
            // The search wire never publishes community counts: release hits
            // start unenriched and the model fills them in place; artists and
            // labels keep `None` for good.
            stats: None,
            enriched: false,
        });
    }
    Ok(SearchPage {
        page: page.page,
        pages: page.pages,
        hits: mapped,
    })
}
#[cfg(test)]
mod tests {
    use super::*;

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
    fn sale_sellers_arrive_as_a_bare_string_or_a_username_object() {
        let payload = serde_json::json!({
            "history": [
                { "price": 5.0, "currency": "USD", "sold_at": "2023-11-14T22:13:20Z",
                  "condition": "Very Good Plus (VG+)", "seller": "Shop One" },
                { "price": 15.0, "currency": "USD", "sold_at": "2023-11-14T22:13:20Z",
                  "condition": "Excellent Plus (EX+)", "seller": { "username": "Shop Two" } }
            ]
        });
        let sales = sales_from_wire(payload, ReleaseId::new(1).unwrap()).unwrap();
        assert_eq!(sales.len(), 2);
        assert_eq!(sales[0].seller.as_deref(), Some("Shop One"));
        assert_eq!(sales[1].seller.as_deref(), Some("Shop Two"));
    }

    #[test]
    fn a_sale_without_a_named_seller_stays_unknown_not_shopless() {
        let payload = serde_json::json!({
            "history": [
                { "price": 5.0, "currency": "USD", "sold_at": "2023-11-14T22:13:20Z" },
                { "price": 6.0, "currency": "USD", "sold_at": "2023-11-14T22:13:20Z", "seller": "" }
            ]
        });
        let sales = sales_from_wire(payload, ReleaseId::new(1).unwrap()).unwrap();
        assert_eq!(sales[0].seller, None, "absent on the wire is unknown");
        assert_eq!(sales[1].seller, None, "an empty username is not a shop");
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
            seller: None,
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
            seller: None,
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
            seller: None,
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
            seller: None,
        };
        assert_eq!(no_currency.try_into_sale(), Err(WireError::UnknownCurrency));

        let no_price = RawSale {
            id: Some(1),
            price: None,
            currency: Some("EUR".into()),
            sold_at: Some("2023-11-14T22:13:20Z".into()),
            condition: None,
            title: None,
            seller: None,
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

    // -- Digger mappings ------------------------------------------------------

    /// `/artists/{id}/releases` payload as the wire actually shapes it:
    /// vinyl release (with format/label/catno), CD release (filtered), a
    /// master carrying only `main_release` + wire community stats, and a
    /// master that `main_release`s nowhere (dropped — Q-02).
    fn artist_rows_payload() -> serde_json::Value {
        serde_json::json!({
            "pagination": { "page": 1, "pages": 3, "items": 255 },
            "releases": [
                { "id": 249504, "type": "release", "title": "The Vinyl",
                  "artist": "Someone", "year": 1994, "format": "Vinyl, LP, Album",
                  "label": "Diggers Ltd", "catno": "DIG-001",
                  "stats": { "community": { "in_wantlist": 90, "in_collection": 40 } } },
                { "id": 249505, "type": "release", "title": "The CD",
                  "artist": "Someone", "year": 1995, "format": "CD, Album" },
                { "id": 249506, "type": "master", "title": "The Master",
                  "artist": "Someone", "year": 1996, "main_release": 249_510,
                  "stats": { "community": { "in_wantlist": 700, "in_collection": 300 } } },
                { "id": 249507, "type": "master", "title": "Nowhere",
                  "artist": "Someone", "year": 1997 }
            ]
        })
    }

    #[test]
    fn artist_rows_are_vinyl_only_and_masters_open_their_release() {
        let payload = artist_rows_payload();
        let raw: RawArtistReleases = serde_json::from_value(payload).unwrap();
        let page = into_catalog_page(raw.pagination, raw.releases).unwrap();

        assert_eq!(
            page.rows.len(),
            2,
            "the CD row is filtered; the main-release-less master is dropped"
        );
        assert_eq!(page.pages, 3);

        let vinyl = &page.rows[0];
        assert_eq!(vinyl.id.get(), 249_504);
        assert!(!vinyl.is_master);
        assert_eq!(vinyl.format, "Vinyl, LP, Album");
        assert_eq!(vinyl.catno, "DIG-001");
        assert_eq!(vinyl.year, Some(1994));
        assert_eq!(vinyl.stats, Some(CommunityStats { want: 90, have: 40 }));
        assert!(!vinyl.enriched, "wire stats do not claim an enrichment");

        // A master row keeps its `is_master` claim but holds the id that can
        // actually be opened — the same rule the search page applies (Q-02).
        let master = &page.rows[1];
        assert!(master.is_master);
        assert_eq!(master.id.get(), 249_510, "master → main_release");
        assert_eq!(master.format, "", "masters publish no format; unknown");
        assert_eq!(
            master.stats,
            Some(CommunityStats {
                want: 700,
                have: 300
            })
        );
    }

    #[test]
    fn label_rows_map_through_the_shared_vinyl_filter() {
        let payload = serde_json::json!({
            "pagination": { "page": 1, "pages": 1, "items": 2 },
            "releases": [
                { "id": 5, "title": "On wax", "artist": "A Band",
                  "year": 2001, "format": "Vinyl, 12\"", "catno": "LAB-5" },
                { "id": 6, "title": "On tape", "artist": "A Band",
                  "year": 2002, "format": "Cassette", "catno": "LAB-5T" }
            ]
        });
        let raw: RawLabelReleases = serde_json::from_value(payload).unwrap();
        let page = into_label_catalog_page(raw.pagination, raw.releases).unwrap();

        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].title, "On wax");
        assert!(!page.rows[0].is_master, "label rows are never masters");
        assert_eq!(page.rows[0].catno, "LAB-5");
        assert!(page.rows[0].stats.is_none(), "label rows carry no summary");
    }

    #[test]
    fn search_resolves_masters_and_drops_stale_kinds() {
        let payload = serde_json::json!({
            "pagination": { "page": 1, "pages": 1, "items": 5 },
            "results": [
                { "id": 108713, "type": "artist", "title": "The Digger" },
                { "id": 1, "type": "label", "title": "Some Label" },
                { "id": 249504, "type": "release", "title": "A Release", "year": "1994" },
                { "id": 777, "type": "master", "title": "A Master", "main_release": 249504 },
                { "id": 778, "type": "master", "title": "Masterless" },
                { "id": 9, "type": "playlist", "title": "Not openable" },
                { "id": 10, "type": "release", "title": "No id", "id": 0 }
            ]
        });
        let raw: RawSearchResponse = serde_json::from_value(payload).unwrap();
        let page = into_search_page(raw.pagination, raw.results).unwrap();

        let kinds: Vec<(EntityKind, u64)> =
            page.hits.iter().map(|hit| (hit.kind, hit.id)).collect();
        assert_eq!(
            kinds,
            vec![
                (EntityKind::Artist, 108_713),
                (EntityKind::Label, 1),
                (EntityKind::Release, 249_504),
            ],
            "the master resolves to its main release and the playlist is dropped"
        );
        assert_eq!(page.hits[2].year.as_deref(), Some("1994"));
    }

    #[test]
    fn search_drops_non_vinyl_release_hits_but_keeps_unknown_formats() {
        let payload = serde_json::json!({
            "pagination": { "page": 1, "pages": 1, "items": 5 },
            "results": [
                // The vinyls stay.
                { "id": 11, "type": "release", "title": "On Wax", "format": ["Vinyl", "12\""] },
                // CD, cassette, digital files go — the whole point.
                { "id": 12, "type": "release", "title": "Compact", "format": ["CD", "Album"] },
                { "id": 13, "type": "release", "title": "On Tape", "format": ["Cassette"] },
                { "id": 14, "type": "release", "title": "The File", "format": ["File", "MP3"] },
                // A master publishes nothing: unknown is not "CD" — it stays.
                { "id": 15, "type": "master", "title": "The Work", "main_release": 155 }
            ]
        });
        let raw: RawSearchResponse = serde_json::from_value(payload).unwrap();
        let page = into_search_page(raw.pagination, raw.results).unwrap();

        let ids: Vec<u64> = page.hits.iter().map(|hit| hit.id).collect();
        assert_eq!(
            ids,
            vec![11, 155],
            "vinyl only — and the format-less master is not convicted of being CD"
        );
    }

    #[test]
    fn detail_maps_community_prices_and_tracklist() {
        let payload = serde_json::json!({
            "id": 249504,
            "title": "The Now-Tales",
            "year": 1994,
            "released": "1994-06-14",
            "country": "US",
            "artists": [ { "id": 108713, "name": "The Digger", "join": "" } ],
            "formats": [ { "name": "Vinyl", "qty": "2", "descriptions": ["LP", "Album"] } ],
            "labels": [ { "id": 1, "name": "Diggers Ltd", "catno": "DIG-001" } ],
            "genres": [ "Electronic" ],
            "styles": [ "Deep House" ],
            "tracklist": [
                { "position": "A1", "title": "Shift Work", "duration": "5:00" }
            ],
            "identifiers": [
                { "type": "Barcode", "value": "5012345678900", "description": "" },
                { "type": "Matrix / Runout", "value": "DIG-001-A", "description": "etched" }
            ],
            "notes": "Double LP in gatefold.",
            "community": {
                "have": 125, "want": 812,
                "rating": { "count": 42, "average": 4.3 }
            },
            "lowest_price": 29.99,
            "num_for_sale": 7
        });
        let raw: RawReleaseDetail = serde_json::from_value(payload).unwrap();
        let detail = raw.into_detail().unwrap();

        let stats = detail.community.expect("community maps");
        assert_eq!(
            stats,
            CommunityStats {
                have: 125,
                want: 812
            }
        );
        assert_eq!(
            detail.rarity_display(),
            // want 812 / have 125 -> 0.866... rounds to 87 on the 0..=100 scale
            Some(87),
            "a grail-shaped want/have lands high"
        );
        let rating = detail.community_rating.expect("rating maps");
        assert_eq!(rating, (4.3, 42));

        let price = detail.lowest_price.expect("price maps");
        assert_eq!(price.minor_units, 2_999, "$29.99 -> 2999 minor units");
        assert_eq!(price.currency, Currency::Usd);
        assert_eq!(detail.num_for_sale, Some(7));

        assert_eq!(detail.tracklist.len(), 1);
        assert_eq!(detail.tracklist[0].position, "A1");
        assert_eq!(detail.identifiers[0], "Barcode: 5012345678900");
        assert_eq!(detail.identifiers[1], "Matrix / Runout: DIG-001-A [etched]");
        assert_eq!(detail.release.id.get(), 249_504);
        assert_eq!(detail.release.title, "The Now-Tales");
    }

    #[test]
    fn detail_with_no_community_leaves_stats_and_rating_none() {
        let payload = serde_json::json!({
            "id": 1, "title": "Bare", "year": null,
            "lowest_price": null, "num_for_sale": null
        });
        let raw: RawReleaseDetail = serde_json::from_value(payload).unwrap();
        let detail = raw.into_detail().unwrap();

        assert!(detail.community.is_none());
        assert!(detail.community_rating.is_none());
        assert!(
            detail.rarity_display().is_none(),
            "absent stats stay absent"
        );
        assert!(detail.lowest_price.is_none());
        assert!(detail.release.year.is_none(), "null year stays unknown");
    }

    #[test]
    fn money_usd_rounds_half_up_at_the_edge() {
        assert_eq!(money_usd(29.99).minor_units, 2_999);
        assert_eq!(money_usd(0.0).minor_units, 0);
        assert_eq!(money_usd(1_200.0).minor_units, 120_000);
    }

    #[test]
    fn catalog_source_refuses_anonymous_search_before_any_http() {
        let source = DiscogsCatalogSource::new(None).unwrap();
        let error = source.search("anything", None, 1).expect_err("must refuse");
        assert!(
            error.to_string().contains("DISCOGS_TOKEN"),
            "the message should name the variable: {error}"
        );
    }
}
