//! The digger's state, messages, and effects: an Elm-style loop, written the
//! way [`crate::app`] describes orchestration, minus the print step.
//!
//! # The loop, in one diagram
//!
//! ```text
//!          Msg                     Effect
//!  input ───────▶ update(model) ───────▶ worker ───▶ ports ───▶ Discogs
//!                    │ pure                ▲        (worker owns the
//!                    ▼                     └── Msg ◀─ adapters and clocks)
//!                    Model ◀────────── full state, one screen in focus
//! ```
//!
//! `update` is pure: it never reads the clock, the network, or the file
//! system, and it only returns effects for the worker to run. `view` reads
//! state and draws. That is the whole shape — see `docs/UX.md` for what each
//! screen does and `docs/DECISIONS.md`, D-10, for why not async.
//!
//! # Ordering guarantees we lean on
//!
//! The worker executes effects strictly in arrival order and the main loop
//! applies replies in worker order. When a *stale* reply can still arrive
//! (the user moved on), the screen it belongs to records a `pending` token
//! and the reply is dropped unless the token matches.

use std::collections::{HashMap, HashSet};

use crossterm::event::KeyEvent;

use crate::domain::{
    browse::{
        ArtistProfile, CatalogRow, CommunityStats, EntityKind, LabelProfile, ReleaseDetail,
        SearchResult, WantEntry,
    },
    identity::ReleaseId,
    market::Money,
};

/// The kind filter the home screen toggles: `None` is "any".
pub type Filter = Option<EntityKind>;

/// The home screen: one input line, one toggle, some help text.
#[derive(Debug, Default)]
pub struct HomeScreen {
    pub input: String,
    pub filter: Filter,
}

/// Where a release detail was asked for, and the token that proves the
/// screen it answers is still focused (staleness check: a reply that arrives
/// after the user moved on carries its token, and the token no longer
/// matches — the reply is dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenRequest {
    /// Enrich one row of the entity table; token is the release id.
    Enrich(ReleaseId),
    /// Enrich one search hit in place; token is the release id it answers.
    EnrichHit(ReleaseId),
    /// Open from the entity table; token is the release id it should answer.
    FromEntity(ReleaseId),
    /// Open from a search hit; tokens are the expected release id and the
    /// hit's row index at open time.
    FromSearch(ReleaseId, usize),
    /// Open from a wantlist line; same token pair.
    FromWantlist(ReleaseId, usize),
}

/// The search results screen.
#[derive(Debug, Default)]
pub struct SearchScreen {
    pub query: String,
    pub filter: Filter,
    pub page: u32,
    pub pages: u32,
    pub hits: Vec<SearchResult>,
    pub selected: usize,
    /// Release ids with a hit enrichment in flight — the same double-request
    /// guard the entity table keeps. Only release hits enrich: artists and
    /// labels have no community stats any endpoint would give them.
    pub inflight: HashSet<ReleaseId>,
    /// How the sheet is ordered. Sorting is page-local — the wire returns
    /// one page at a time, so "rare" means *this page's* rarest first; the
    /// block title says so. `1` selects rare directly, `0` refetches the
    /// page in wire order, `s` cycles.
    pub sort: SortMode,
}

/// Whose release table this is, and what to say above it.
#[derive(Debug)]
pub enum Profile {
    Artist(ArtistProfile),
    Label(LabelProfile),
}

impl Profile {
    /// The display name, for the breadcrumb.
    pub fn name(&self) -> &str {
        match self {
            Self::Artist(profile) => &profile.name,
            Self::Label(profile) => &profile.name,
        }
    }
}

/// The artist or label screen: profile + one page of the release table.
/// Constructed with a real kind by `update::open_entity`; the hand-written
/// `Default` exists only so `Model` can derive its own.
#[derive(Debug)]
pub struct EntityScreen {
    pub kind: EntityKind,
    pub id: u64,
    pub profile: Option<Profile>,
    pub rows: Vec<CatalogRow>,
    pub page: u32,
    pub pages: u32,
    pub selected: usize,
    /// While the profile fetch is in flight. Rows may land first; the table
    /// fills as soon as it can, the header when it is ready.
    pub profile_loading: bool,
    /// Release ids with an enrichment fetch in flight — also the guard that
    /// keeps one row from being requested twice.
    pub inflight: HashSet<ReleaseId>,
    /// How the sheet is ordered; `s` cycles it. Page-local, like the
    /// search screen's: sorting can only ever order rows already loaded.
    pub sort: SortMode,
}

impl Default for EntityScreen {
    fn default() -> Self {
        Self {
            kind: EntityKind::Artist,
            id: 0,
            profile: None,
            rows: Vec::new(),
            page: 1,
            pages: 0,
            selected: 0,
            profile_loading: false,
            inflight: HashSet::new(),
            sort: SortMode::Sheet,
        }
    }
}

/// The release details screen.
#[derive(Debug)]
pub struct ReleaseScreen {
    pub detail: ReleaseDetail,
    pub scroll: usize,
}

/// The wantlist screen.
#[derive(Debug, Default)]
pub struct WantlistScreen {
    pub selected: usize,
    /// Row awaiting a delete confirmation; `y` proceeds, `n`/other cancels.
    pub confirm: Option<usize>,
}

/// The session's remembered detail fills, keyed by release id.
pub type DetailFills = HashMap<ReleaseId, DetailFill>;

/// One fetched detail's row-visible fields, remembered for the session.
///
/// When a page (re)loads, rows are dressed from this cache before the
/// screen paints, so switching pages does not grey out prices and counts
/// the session already fetched — the adapter's HTTP cache makes the refetch
/// free, but without this the *screen* still replays the fetch. Session-
/// scoped like the adapter cache: prices age, so nothing survives a run.
/// How the sheet is ordered. `Sheet` is the wire's own order; `1`/`2`
/// select the sorted claims directly and `0` returns to the wire's order
/// by refetching the current page — the original order is not recorded,
/// so that is the only honest way back. A fresh entity or search screen
/// starts at `Sheet`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    /// The wire's own order.
    #[default]
    Sheet,
    /// Rarest first; unknowns keep their sheet order at the bottom.
    Rare,
    /// Priciest first; unknowns keep their sheet order at the bottom.
    Price,
}

impl SortMode {
    /// The next mode on the entity screen, where price exists. Once
    /// sorting has begun, the wheel turns between the two sortable claims.
    pub fn next(self) -> Self {
        match self {
            Self::Sheet | Self::Price => Self::Rare,
            Self::Rare => Self::Price,
        }
    }

    /// The next mode on the search screen, where no price exists —
    /// sorting starts at rare and stays there.
    pub fn next_hits(self) -> Self {
        Self::Rare
    }
    /// The name the sheet's block title shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Sheet => "sheet",
            Self::Rare => "rare",
            Self::Price => "price",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DetailFill {
    pub stats: Option<CommunityStats>,
    pub lowest_price: Option<Money>,
    /// What the detail fetch published about the pressing itself. The
    /// compact rows often leave these blank — masters always do — and a
    /// dressed row is marked enriched, so the fill is the only chance to
    /// close those gaps.
    pub format: String,
    pub label: String,
    pub catno: String,
}

/// Everything left of `update`'s algorithm.
#[derive(Debug, Default)]
pub struct Model {
    pub home: HomeScreen,
    pub search: SearchScreen,
    pub entity: EntityScreen,
    pub release: Option<ReleaseScreen>,
    pub wantlist: WantlistScreen,

    /// Screen stack, home at the floor.
    pub stack: Vec<Screen>,

    /// The local wantlist, replicated from the store's truth on load and
    /// updated by replies. The store owns persistence; this owns display.
    pub wants: Vec<WantEntry>,

    /// The one release open waiting on a reply. A detail that arrives
    /// without exactly this token is dropped — the user has moved on.
    pub pending_open: Option<OpenRequest>,

    /// Details fetched this session, keyed by release id. Rows and hits
    /// arriving fresh from a page load are dressed from here; see
    /// [`DetailFill`] for why.
    pub detail_fills: DetailFills,

    /// Network jobs in flight, for the spinner. `0` means quiet.
    pub busy: u32,

    /// Spinner tick counter, bumped by every [`Msg::Tick`].
    pub ticks: u64,

    /// Terminal size, set by the runtime before every draw.
    pub viewport: (u16, u16),

    /// Transient status line: errors from [`Msg::Failed`], confirmations from
    /// want replies. Cleared on navigation.
    pub status: Option<Status>,
}

/// A short-lived line in the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Error(String),
    Info(String),
}

impl Model {
    /// The focused screen, if any.
    pub fn screen(&self) -> Option<&Screen> {
        self.stack.last()
    }

    /// The focused screen, mutably.
    pub fn screen_mut(&mut self) -> Option<&mut Screen> {
        self.stack.last_mut()
    }

    /// Push a screen.
    pub fn push(&mut self, screen: Screen) {
        self.stack.push(screen);
    }

    /// Pop one screen, home at the floor, so `Esc` never empties the stack.
    pub fn pop(&mut self) {
        match self.stack.last() {
            Some(Screen::Home) | None => {}
            Some(_) => {
                _ = self.stack.pop();
            }
        }
    }

    /// Register an in-flight release a detail reply must answer for.
    pub fn await_open(&mut self, request: OpenRequest) {
        self.pending_open = Some(request);
    }

    /// Whether the wantlist already names this entity (per the model's
    /// replica; the store's reply is the source of truth).
    pub fn wants_contains(&self, kind: EntityKind, id: u64) -> bool {
        self.wants
            .iter()
            .any(|want| want.kind == kind && want.id == id)
    }

    /// Drop the status line — called on navigation, so stale errors do not
    /// trail the user to a new screen.
    pub fn clear_status(&mut self) {
        self.status = None;
    }
}

/// The screens, in stack order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Home,
    Search,
    Entity,
    Release,
    Wantlist,
}

/// What the runtime hands to `update`: every reply, every key, every tick.
///
/// Large payloads travel boxed: this enum is moved through channel queues
/// and `update`'s match on every frame, and a `ReleaseDetail` is fifty times
/// the size of a key press. Boxing is the "no double copy" rule applied to
/// the message type, not a style tic.
#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Tick,
    /// A search page arrived for the pending query.
    SearchLoaded(Box<crate::domain::browse::SearchPage>),
    /// An artist profile arrived.
    ArtistLoaded(u64, Box<crate::domain::browse::ArtistProfile>),
    /// A label profile arrived.
    LabelLoaded(u64, Box<crate::domain::browse::LabelProfile>),
    /// A page of release rows arrived.
    RowsLoaded {
        kind: EntityKind,
        id: u64,
        page: u32,
        data: Box<crate::domain::browse::CatalogPage>,
    },
    /// A release detail arrived — either for opening or for enrichment.
    DetailLoaded(OpenRequest, Box<crate::domain::browse::ReleaseDetail>),
    /// Enrichment could not fetch a detail. Quiet by design: the row stays
    /// unenriched, which is what it was before, and its inflight guard is
    /// cleared so a later tick may retry.
    EnrichFailed(ReleaseId),
    /// The wantlist replica refresh.
    WantlistLoaded(Vec<WantEntry>),
    /// An add finished. `added` is false when the store already had it.
    WantAdded {
        kind: EntityKind,
        id: u64,
        name: String,
        added_at: i64,
        added: bool,
    },
    /// A remove finished. `removed` is false when the store had nothing.
    WantRemoved {
        kind: EntityKind,
        id: u64,
        removed: bool,
    },
    /// Any other port failure. Named so the status line can say what
    /// happened. `job` marks a failure that closes a counted job: the mirror
    /// of the network effect that went out failing. File-talk messages
    /// (want add/remove) were never counted, so their failures must never
    /// un-count either — the spinner's honesty depends on both halves.
    Failed {
        job: bool,
        context: Option<String>,
        message: String,
    },
}

/// What `update` asks the worker to do. The worker runs them in order and
/// translates every outcome back into a [`Msg`].
#[derive(Debug, PartialEq)]
pub enum Effect {
    /// `GET /artists/{id}` — the profile above the table.
    LoadArtistProfile(u64),
    /// `GET /labels/{id}`.
    LoadLabelProfile(u64),
    /// One page of the artist's or label's releases.
    LoadRows {
        kind: EntityKind,
        id: u64,
        page: u32,
    },
    /// Open a release: full detail, pushed onto the stack on arrival.
    LoadDetail {
        release: ReleaseId,
        request: OpenRequest,
    },
    /// Enrich one table row in place with the detail's stats.
    Enrich(ReleaseId),
    /// Database search. Requires a token; the adapter refuses otherwise and
    /// the refusal arrives as [`Msg::Failed`].
    Search {
        query: String,
        kind: Filter,
        page: u32,
    },
    /// Refresh the wantlist replica from the store.
    LoadWantlist,
    /// Record a want. The worker stamps `added_at` (it owns the clock only
    /// for this: stamps at the moment of write, not at moment of intent).
    AddWant {
        kind: EntityKind,
        id: u64,
        name: String,
    },
    /// Un-record a want.
    RemoveWant { kind: EntityKind, id: u64 },
    /// Leave the app. Never executed by the worker; the main loop sees it in
    /// the effect list and closes the terminal.
    Quit,
}

impl Effect {
    /// Whether this effect touches the network, for spinner accounting.
    pub fn is_network(&self) -> bool {
        matches!(
            self,
            Self::LoadArtistProfile(_)
                | Self::LoadLabelProfile(_)
                | Self::LoadRows { .. }
                | Self::LoadDetail { .. }
                | Self::Enrich(_)
                | Self::Search { .. }
        )
    }

    /// Which queue this effect rides in (`docs/DECISIONS.md`, D-14). The
    /// worker drains user work before enrichment, so a search screen's
    /// hit backlog can never strand an artist open behind itself.
    pub fn class(&self) -> EffectClass {
        match self {
            Self::Enrich(_)
            | Self::LoadDetail {
                request: OpenRequest::Enrich(_) | OpenRequest::EnrichHit(_),
                ..
            } => EffectClass::Enrichment,
            _ => EffectClass::User,
        }
    }
}

/// The two worker queues (`docs/DECISIONS.md`, D-14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectClass {
    /// The operator is waiting for this: opens, profiles, rows, search.
    User,
    /// Background stat-filling; the screens show it best-effort.
    Enrichment,
}
