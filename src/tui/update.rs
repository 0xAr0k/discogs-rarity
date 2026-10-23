//! The pure half of the digger: `state + message → state + effects`.
//!
//! # What is allowed in here
//!
//! Arithmetic on the model, matching on the message, building effect vectors.
//! What is *not* allowed: the clock, the network, the disk, or the terminal.
//! Every consequence of a key press that is not pure model state shows up as
//! an [`Effect`] for [`super::runtime`] to carry out. That keeps the whole
//! behavior of the app testable without a terminal — the tests at the bottom
//! drive keys against a fresh [`Model`] and assert on state and effects only.
//!
//! # Keys, per screen
//!
//! | Screen | Keys |
//! |---|---|
//! | every | `F1` home, `F2` wantlist, `Ctrl+C` quit |
//! | home | type; `←`/`→` cycle the kind toggle; `Enter` go; `Esc` clears — twice to quit |
//! | search | `↑`/`↓` move; `←`/`→` page; `1` sort rare; `0` sheet order (refetches the page); `⏎` open; `w` want the hit; `Esc` back |
//! | entity | `↑`/`↓` move; `←`/`→` page; `⏎` open; `1` sort rare; `2` sort price; `0` sheet order (refetches the page); `s` cycle rare→price; `w` want the artist/label; `Esc` back |
//! | release | `↑`/`↓`/`PgUp`/`PgDn`/`Home` scroll; `w` want the release; `Esc` back |
//! | wantlist | `↑`/`↓` move; `Enter` open; `w` re-want; `d` then `y`/`n` confirm the delete |
//!
//! # Enrichment, in one paragraph
//!
//! Rows land with `stats: None` — the artist/label listing endpoints carry
//! no community counts (`docs/DISCOGS.md` § listings) — and often no
//! format/label/catno either: masters *never* publish the three. Whenever
//! rows load or the selection moves, the entity screen asks for details for
//! its rows around the selection — [`visible_range`] — one [`Effect::Enrich`]
//! per unenriched row, guarded by `EntityScreen::inflight` so a row is
//! never requested twice. Replies fill the row **in place**. Master rows
//! enrich like any other: their `id` is the *main release's* id (the
//! adapter translates, `docs/DECISIONS.md` Q-01's resolution), and the
//! detail closes the format/label/catno gaps the compact row left blank.
//!
//! The search screen runs the identical policy over its hits: one detail
//! per visible, still-grey **release** hit (`Effect::LoadDetail` with
//! [`OpenRequest::EnrichHit`], guarded by `SearchScreen::inflight`).
//! Artist and label hits are skipped *by kind* — no endpoint publishes
//! want/have counts for them, so the view shows their rarity as a
//! permanent `—`, not a pending `…`. Both schedulers follow focus: only
//! the screen the operator is looking at schedules, and the worker drains
//! user work (opens, profiles, rows) before enrichment —
//! `docs/DECISIONS.md`, D-14.
//!
//! # Conviction on arrival
//!
//! The vinyl rule filters at the boundary on the formats the wire *says*;
//! a hit that says nothing (resolved masters, quiet listings) passes as
//! unknown — unknown is not "CD". But its detail fetch is the better
//! claim: when the fill names the formats and none of them says `Vinyl`,
//! the row leaves the table where it stood, and the selection stays
//! pinned (`docs/DECISIONS.md`, D-18).
//!
//! # Sorting, in one sentence
//!
//! On an entity or search table, `1` sorts **the loaded page** rarest
//! first, `2` priciest (entity only — search hits publish no price), `0`
//! restores the wire's order by refetching the current page (sorting
//! cannot un-sort; the order is not recorded), and `s` cycles the sticky
//! rare↔price wheel. The wire hands the digger one page at a time, so a
//! "rarest overall" claim would be a lie unless every page were fetched
//! first; unknown values keep their wire order at the bottom (`unknown`
//! is not `zero`, so it must not sort as one), a fresh page sorts on
//! arrival, and the selection stays pinned to the row it was on.
//!
//! # Staleness rule
//!
//! The worker runs effects in arrival order, but replies still race the
//! user: open row 3, press Esc before the page turns, and row 3's screen
//! would push itself over whatever is focused now. So there is exactly one
//! pending open — [`Model::pending_open`] — and a [`Msg::DetailLoaded`]
//! replying to something else is dropped. Enrichment does not need this: it
//! only fills the row whose id it fetched, and a stale enrich that finds no
//! row to fill is a no-op by construction.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashSet;

use crate::domain::{
    browse::is_vinyl_format,
    browse::{self, CatalogPage, CatalogRow, EntityKind, ReleaseDetail, SearchResult, WantEntry},
    identity::ReleaseId,
};

use super::model::{
    DetailFill, DetailFills, Effect, Model, Msg, OpenRequest, Profile, Screen, SortMode, Status,
};

/// Advance the model by one message and report work to do, in order.
pub fn update(model: &mut Model, msg: Msg) -> Vec<Effect> {
    // Connectivity accounting: a terminal reply closes one job.
    if msg.is_terminal() {
        model.busy = model.busy.saturating_sub(1);
    }

    let effects = drive(model, msg);
    // …and every network effect opens one. Otherwise `busy` never rises and
    // the enrichment cap (`busy >= MAX_IN_FLIGHT`) is a fence that never
    // meets a sheep.
    model.busy += effects.iter().filter(|effect| effect.is_network()).count() as u32;
    effects
}

/// How many detail fetches may be in the air at once — the scheduler's
/// share of the wire budget, aligned with the pacing gate's burst
/// capacity (8, `adapters::discogs`). Higher only fills more cells per
/// wave; the token bucket is the real rate limiter.
const MAX_IN_FLIGHT: usize = 8;

/// The reducer's arms, without the accounting around them.
fn drive(model: &mut Model, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Tick => {
            model.ticks = model.ticks.wrapping_add(1);
            tick(model)
        }
        Msg::Key(key) => key_message(model, key),
        Msg::SearchLoaded(page) => search_loaded(model, *page),
        Msg::ArtistLoaded(id, profile) => artist_loaded(model, id, *profile),
        Msg::LabelLoaded(id, profile) => label_loaded(model, id, *profile),
        Msg::RowsLoaded {
            kind,
            id,
            page,
            data,
        } => rows_loaded(model, kind, id, page, *data),
        Msg::DetailLoaded(request, detail) => detail_loaded(model, request, *detail),
        Msg::EnrichFailed(id) => {
            // Quiet failure: clear the guard so a later tick may retry. An
            // id alone does not say which screen claimed it; both clears
            // are cheap and neither can unclaim someone else's fetch.
            model.entity.inflight.remove(&id);
            model.search.inflight.remove(&id);
            Vec::new()
        }
        Msg::WantlistLoaded(entries) => wantlist_loaded(model, entries),
        Msg::WantAdded {
            kind,
            id,
            name,
            added_at,
            added,
        } => want_added(model, kind, id, name, added_at, added),
        Msg::WantRemoved { kind, id, removed } => want_removed(model, kind, id, removed),
        Msg::Failed {
            context, message, ..
        } => failed(model, context, message),
    }
}

impl Msg {
    /// Whether this message closes a counted job — the mirror of
    /// [`Effect::is_network`], plus the wantlist's boot load, which the
    /// runtime counts by hand before the first key press. Every other
    /// success or failure must not, or the spinner never stops ("load"
    /// forever) after a single failed request.
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            Msg::SearchLoaded(_)
                | Msg::ArtistLoaded(..)
                | Msg::LabelLoaded(..)
                | Msg::RowsLoaded { .. }
                | Msg::DetailLoaded(..)
                | Msg::WantlistLoaded(_)
                | Msg::EnrichFailed(_)
                | Msg::Failed { job: true, .. }
        )
    }
}

// ---------------------------------------------------------------------------
// Simple replies.
// ---------------------------------------------------------------------------

fn failed(model: &mut Model, context: Option<String>, message: String) -> Vec<Effect> {
    model.status = Some(Status::Error(match context {
        Some(context) => format!("{context}: {message}"),
        None => message,
    }));
    Vec::new()
}

fn wantlist_loaded(model: &mut Model, entries: Vec<WantEntry>) -> Vec<Effect> {
    model.wants = entries;
    // Nothing about the focused screen assumes a particular size; renderers
    // clamp on their own.
    Vec::new()
}

fn want_added(
    model: &mut Model,
    kind: EntityKind,
    id: u64,
    name: String,
    added_at: i64,
    added: bool,
) -> Vec<Effect> {
    if added && !model.wants_contains(kind, id) {
        model.wants.push(WantEntry {
            kind,
            id,
            name: name.clone(),
            added_at,
        });
        // Same ordering rule as the store: newest first, name for ties.
        model
            .wants
            .sort_by(|a, b| b.added_at.cmp(&a.added_at).then(a.name.cmp(&b.name)));
    }
    // "Already wanted" is not an error; `name` is what the row will try to
    // want next time, and the honest status is the one that says so.
    model.status = Some(if added {
        Status::Info(format!("on your wantlist: {name}"))
    } else {
        Status::Info(format!("already wanted: {name}"))
    });
    Vec::new()
}

fn want_removed(model: &mut Model, kind: EntityKind, id: u64, removed: bool) -> Vec<Effect> {
    if removed {
        model
            .wants
            .retain(|want| !(want.kind == kind && want.id == id));
    }
    model.wantlist.confirm = None;
    Vec::new()
}

fn search_loaded(model: &mut Model, page: browse::SearchPage) -> Vec<Effect> {
    let hits = page.hits.len();
    if matches!(model.screen(), Some(Screen::Search)) {
        model.search.page = page.page;
        model.search.pages = page.pages;
        model.search.hits = page.hits;
        // A new page of rows invalidates the old page's in-flight claims:
        // whatever was fetching belongs to rows we just replaced.
        model.search.inflight.clear();
        model.search.selected = model.search.selected.min(hits.saturating_sub(1));
        dress_hits(&mut model.search.hits, &model.detail_fills);
        // Page dressing can convict: a cached detail that names CD closes
        // the `…` the row arrived with, and the D-07 rule applies at once,
        // before the table ever renders the row.
        convict_hits(&mut model.search.hits, &mut model.search.selected);
        apply_hit_sort(&mut model.search.hits, model.search.sort);
        return hit_enrichments(model);
    }
    Vec::new()
}

fn artist_loaded(
    model: &mut Model,
    id: u64,
    profile: crate::domain::browse::ArtistProfile,
) -> Vec<Effect> {
    if model.entity.id == id && model.entity.kind == EntityKind::Artist {
        model.entity.profile_loading = false;
        model.entity.profile = Some(Profile::Artist(profile));
    }
    Vec::new()
}

fn label_loaded(
    model: &mut Model,
    id: u64,
    profile: crate::domain::browse::LabelProfile,
) -> Vec<Effect> {
    if model.entity.id == id && model.entity.kind == EntityKind::Label {
        model.entity.profile_loading = false;
        model.entity.profile = Some(Profile::Label(profile));
    }
    Vec::new()
}

/// A page of the artist's or label's rows, assigned when the entity is still
/// the one on screen. The sequential worker guarantees ordering; if the user
/// has moved to a different entity, the reply is for a screen that no longer
/// exists and is dropped without ceremony.
fn rows_loaded(
    model: &mut Model,
    kind: EntityKind,
    id: u64,
    page: u32,
    data: CatalogPage,
) -> Vec<Effect> {
    if !(model.entity.kind == kind && model.entity.id == id) {
        return Vec::new();
    }
    model.entity.page = page;
    model.entity.pages = data.pages;
    model.entity.rows = data.rows;
    model.entity.selected = model
        .entity
        .selected
        .min(model.entity.rows.len().saturating_sub(1));
    dress_rows(&mut model.entity.rows, &model.detail_fills);
    // Same dressing-time conviction: master rows arrive without a format —
    // unknown is not "CD" — and a cached detail that turns out to name CD
    // removes the row rather than showing a CD on the vinyl table.
    convict_rows(&mut model.entity.rows, &mut model.entity.selected);
    apply_row_sort(&mut model.entity.rows, model.entity.sort);
    visible_enrichments(model)
}

/// A detail reply: an accepted open pushes the release screen; an enrich
/// reply fills its row in place; anything the pending token does not answer
/// is dropped. A detail is never absent here — failed fetches arrive as
/// [`Msg::Failed`] (opens) or [`Msg::EnrichFailed`] (enrichment) instead.
fn detail_loaded(model: &mut Model, request: OpenRequest, detail: ReleaseDetail) -> Vec<Effect> {
    match request {
        OpenRequest::Enrich(id) => {
            model.entity.inflight.remove(&id);
            // A stale enrich finds nothing to fill; that is the *point*.
            remember_detail(&mut model.detail_fills, &detail);
            fill_row(&mut model.entity.rows, &detail);
            // The detail is the better claim: a row the boundary admitted
            // as `unknown` leaves when the fill names no vinyl.
            convict_rows(&mut model.entity.rows, &mut model.entity.selected);
        }
        OpenRequest::EnrichHit(id) => {
            model.search.inflight.remove(&id);
            remember_detail(&mut model.detail_fills, &detail);
            fill_hit(&mut model.search.hits, &detail);
            convict_hits(&mut model.search.hits, &mut model.search.selected);
        }
        opening if model.pending_open.as_ref() == Some(&opening) => {
            model.pending_open = None;
            push_release_screen(model, detail);
        }
        _ => {}
    }
    Vec::new()
}

/// Fill one row from its detail, in place — stats, price, and the gaps the
/// compact row left blank. Masters publish no format/label/catno on the
/// listing endpoint, but their `id` already names the main release, so the
/// detail fetch closes those gaps the same request that counts the crowd.
fn fill_row(rows: &mut [CatalogRow], detail: &ReleaseDetail) {
    if let Some(row) = rows.iter_mut().find(|row| row.id == detail.release.id) {
        dress_row(
            row,
            &DetailFill {
                stats: detail.community,
                lowest_price: detail.lowest_price,
                format: format_of(detail.release.formats.as_slice()),
                label: label_of(detail.release.labels.as_slice()),
                catno: catalog_number(detail.release.labels.as_slice()),
            },
        );
    }
}

/// One detail's fields onto one row: numbers land as they are, the gap
/// fields only fill where the wire said nothing — a row that already names
/// its label keeps the listing's answer, which is the one the user sorted
/// by. `enriched` marks the *fetch* happened, so a community-less release
/// cannot loop the channel every tick.
fn dress_row(row: &mut CatalogRow, fill: &DetailFill) {
    row.stats = fill.stats;
    row.lowest_price = fill.lowest_price;
    if row.format.is_empty() {
        row.format = fill.format.clone();
    }
    if row.label.is_empty() {
        row.label = fill.label.clone();
    }
    if row.catno.is_empty() {
        row.catno = fill.catno.clone();
    }
    row.enriched = true;
}

/// The known-formats half of the vinyl rule (D-07, D-18): the formats are
/// published and none of them says `Vinyl`, so this row is *not one of the
/// vinyls*. Empty formats never convict — unknown is not "CD" — which is
/// exactly why a row can arrive unknown and be convicted later. Both the
/// entity row's display string and the hit's joined format names go
/// through here, so one sentence governs both tables.
fn convicts_non_vinyl(formats: &str) -> bool {
    !formats.is_empty() && !is_vinyl_format(formats)
}

/// Drop every row its filled formats convict: one that slipped in as
/// `unknown` leaves the table the moment its detail names the formats and
/// none of them says `Vinyl`. The selection stays pinned to the row it was
/// on — only removals *above* it move it, and it never points past the end.
fn convict_rows(rows: &mut Vec<CatalogRow>, selected: &mut usize) {
    let mut kept = Vec::new();
    let mut removed_above = 0;
    for (index, row) in std::mem::take(rows).into_iter().enumerate() {
        let convicted = convicts_non_vinyl(&row.format);
        if convicted {
            if index < *selected {
                removed_above += 1;
            }
            continue;
        }
        kept.push(row);
    }
    *rows = kept;
    *selected = (*selected)
        .saturating_sub(removed_above)
        .min(rows.len().saturating_sub(1));
}

/// The search-hit twin of [`convict_rows`]: only release hits convict —
/// artists and labels publish no formats on any endpoint.
fn convict_hits(hits: &mut Vec<SearchResult>, selected: &mut usize) {
    let mut kept = Vec::new();
    let mut removed_above = 0;
    for (index, hit) in std::mem::take(hits).into_iter().enumerate() {
        let convicted =
            hit.kind == EntityKind::Release && convicts_non_vinyl(&hit.formats.join(", "));
        if convicted {
            if index < *selected {
                removed_above += 1;
            }
            continue;
        }
        kept.push(hit);
    }
    *hits = kept;
    *selected = (*selected)
        .saturating_sub(removed_above)
        .min(hits.len().saturating_sub(1));
}

/// Fill one search hit from its detail, in place, gaps included. Same claim
/// rule as [`fill_row`]: the marker records the *fetch*, so a community-less
/// release cannot re-enter the channel every tick.
fn fill_hit(hits: &mut [SearchResult], detail: &ReleaseDetail) {
    if let Some(hit) = hits
        .iter_mut()
        .find(|hit| hit.id == detail.release.id.get())
    {
        hit.stats = detail.community;
        if hit.formats.is_empty() {
            hit.formats = detail
                .release
                .formats
                .iter()
                .map(|format| format.name.clone())
                .collect();
        }
        if hit.labels.is_empty() {
            hit.labels = detail
                .release
                .labels
                .iter()
                .map(|label| label.name.clone())
                .collect();
        }
        if hit.catno.is_none() {
            hit.catno = detail.release.catalogue_number().map(str::to_string);
        }
        hit.enriched = true;
    }
}

/// One fetched detail for future page loads: the session's claim about what
/// this release is. The gap fields ride along so reloaded pages stay filled.
fn remember_detail(fills: &mut DetailFills, detail: &ReleaseDetail) {
    fills.insert(
        detail.release.id,
        DetailFill {
            stats: detail.community,
            lowest_price: detail.lowest_price,
            format: format_of(detail.release.formats.as_slice()),
            label: label_of(detail.release.labels.as_slice()),
            catno: catalog_number(detail.release.labels.as_slice()),
        },
    );
}

/// Dress fresh rows with what the session already fetched, before any tick
/// asks for details again: a page switch shows prices and pressing facts,
/// not grey cells.
fn dress_rows(rows: &mut [CatalogRow], fills: &DetailFills) {
    for row in rows.iter_mut() {
        if let Some(fill) = fills.get(&row.id) {
            dress_row(row, fill);
        }
    }
}

/// Same dressing over search hits that just landed from the wire.
fn dress_hits(hits: &mut [SearchResult], fills: &DetailFills) {
    for hit in hits.iter_mut() {
        if hit.kind != EntityKind::Release {
            continue;
        }
        // ReleaseId::new — a hit id the adapter already validated; the
        // `else` path is a cheap skip, never a panic.
        if let Some(fill) = ReleaseId::new(hit.id).and_then(|id| fills.get(&id)) {
            hit.stats = fill.stats;
            if hit.formats.is_empty() && !fill.format.is_empty() {
                hit.formats = fill.format.split(", ").map(str::to_string).collect();
            }
            if hit.labels.is_empty() && !fill.label.is_empty() {
                hit.labels = fill.label.split(", ").map(str::to_string).collect();
            }
            if hit.catno.is_none() && !fill.catno.is_empty() {
                hit.catno = Some(fill.catno.clone());
            }
            hit.enriched = true;
        }
    }
}

/// The display form of a release's formats: `Vinyl, LP, Album`. Composed
/// here, not in the domain, because format display order is a table's
/// taste, not a fact about a record.
fn format_of(formats: &[crate::domain::release::Format]) -> String {
    formats
        .iter()
        .map(|format| {
            if format.descriptions.is_empty() {
                format.name.clone()
            } else {
                format!("{}, {}", format.name, format.descriptions.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// First label's name — the sheet's label column is one label wide; a
/// pressing with two labels shows the one that pressed it.
fn label_of(labels: &[crate::domain::release::Label]) -> String {
    labels
        .first()
        .map(|label| label.name.clone())
        .unwrap_or_default()
}

/// First label's catalogue number. Empty when the label didn't publish one,
/// which the view shows as `…`, not as a fake blank.
fn catalog_number(labels: &[crate::domain::release::Label]) -> String {
    labels
        .first()
        .map(|label| label.catno.clone())
        .unwrap_or_default()
}

fn push_release_screen(model: &mut Model, detail: ReleaseDetail) {
    model.release = Some(super::model::ReleaseScreen { detail, scroll: 0 });
    model.push(Screen::Release);
}

// ---------------------------------------------------------------------------
// Tick: the enrichment scheduler.
// ---------------------------------------------------------------------------

fn tick(model: &mut Model) -> Vec<Effect> {
    // Enrichment follows focus: only the screen the user is looking at
    // schedules work. A search screen left in the background must not
    // quietly pile fetches behind an artist open waiting on its rows.
    if model.busy < MAX_IN_FLIGHT as u32 {
        match model.screen() {
            Some(Screen::Entity) => visible_enrichments(model),
            Some(Screen::Search) => hit_enrichments(model),
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    }
}

/// One enrich effect per row around the selection that wants one. Fetched
/// rows and in-flight claims are skipped, and `inflight` is claimed before
/// the effect leaves, so re-entry claims nothing twice.
fn visible_enrichments(model: &mut Model) -> Vec<Effect> {
    let viewport = model.viewport.1;
    let range = visible_range(model.entity.selected, model.entity.rows.len(), viewport);
    let mut effects = Vec::new();
    for index in range {
        // The cap counts what is already in the air plus what this batch is
        // about to push — `busy` is only settled once `update` returns, so
        // the loop must carry the arithmetic itself or one tick claims every
        // visible row at once.
        if model.busy as usize + effects.len() >= MAX_IN_FLIGHT {
            break;
        }
        let Some(row) = model.entity.rows.get(index) else {
            continue;
        };
        if row.enriched || model.entity.inflight.contains(&row.id) {
            continue;
        }
        if model.entity.inflight.insert(row.id) {
            effects.push(Effect::Enrich(row.id));
        }
    }
    effects
}

/// The slice of rows around the selection that roughly fits the viewport.
/// Pure arithmetic. Ten rows of chrome is the view's measured layout cost —
/// breadcrumb, profile, status band — and at least two rows always enrich,
/// for terminals too small to bother with.
pub fn visible_range(selected: usize, total: usize, viewport: u16) -> std::ops::Range<usize> {
    let visible = usize::from(viewport.saturating_sub(10)).max(2);
    let start = selected.min(total.saturating_sub(1));
    let end = (start + visible).min(total);
    start..end
}

// ---------------------------------------------------------------------------
// Sorting the sheet, page-locally. `s` cycles the mode; a fresh page also
// sorts, so the order survives page turns. Sorting lives here, not in the
// view, because the enrich scheduler walks the same indices the table
// shows — two orders, one of them stale, would put stats on the wrong row.
// ---------------------------------------------------------------------------

/// Reorder the entity sheet in place. Rows the wire has not told us about
/// yet keep their sheet order at the bottom: unknown is not zero, so it
/// must not sort as one — and a stable sort keeps ties in wire order.
fn apply_row_sort(rows: &mut [CatalogRow], mode: SortMode) {
    match mode {
        SortMode::Sheet => {}
        SortMode::Rare => rows.sort_by(row_rare_order),
        SortMode::Price => rows.sort_by(row_price_order),
    }
}

/// The same over search hits, where only rarity exists — no endpoint
/// publishes a listing price for a bare search hit.
fn apply_hit_sort(hits: &mut [SearchResult], mode: SortMode) {
    if mode == SortMode::Rare {
        hits.sort_by(hit_rare_order);
    }
}

/// Rarity comparison: known first, best at top, unknowns last in order.
fn rare_order(known_a: Option<u16>, known_b: Option<u16>) -> std::cmp::Ordering {
    match (known_a, known_b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn row_rare_order(a: &CatalogRow, b: &CatalogRow) -> std::cmp::Ordering {
    rare_order(a.rarity_display(), b.rarity_display())
}

fn hit_rare_order(a: &SearchResult, b: &SearchResult) -> std::cmp::Ordering {
    rare_order(a.rarity_display(), b.rarity_display())
}

/// Price comparison. All `lowest_price` values are forced to USD by the
/// detail endpoint (`?curr_abbr=USD`), so minor units compare honestly.
fn row_price_order(a: &CatalogRow, b: &CatalogRow) -> std::cmp::Ordering {
    match (&a.lowest_price, &b.lowest_price) {
        (Some(a), Some(b)) => b.minor_units.cmp(&a.minor_units),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// One turn of the `s` wheel on the entity screen: cycle the mode, reorder,
/// and keep the same row under the selection — the press is about order,
/// not about moving the fingers down the sheet.
fn cycle_row_sort(model: &mut Model) -> Vec<Effect> {
    model.entity.sort = model.entity.sort.next();
    sort_rows_in_place(model)
}

fn cycle_hit_sort(model: &mut Model) -> Vec<Effect> {
    model.search.sort = model.search.sort.next_hits();
    sort_hits_in_place(model)
}

fn sort_rows_in_place(model: &mut Model) -> Vec<Effect> {
    let anchor = model
        .entity
        .rows
        .get(model.entity.selected)
        .map(|row| row.id);
    apply_row_sort(&mut model.entity.rows, model.entity.sort);
    if let Some(anchor) = anchor
        && let Some(index) = model.entity.rows.iter().position(|row| row.id == anchor)
    {
        model.entity.selected = index;
    }
    Vec::new()
}

fn sort_hits_in_place(model: &mut Model) -> Vec<Effect> {
    let anchor = model
        .search
        .hits
        .get(model.search.selected)
        .map(|hit| hit.id);
    apply_hit_sort(&mut model.search.hits, model.search.sort);
    if let Some(anchor) = anchor
        && let Some(index) = model.search.hits.iter().position(|hit| hit.id == anchor)
    {
        model.search.selected = index;
    }
    Vec::new()
}

/// The entity table's scheduler, run over the search hits: one detail per
/// visible, unenriched **release** hit, claimed in `inflight` before the
/// effect leaves so a row cannot be requested twice. Artist and label hits
/// are skipped by kind — no want/have numbers exist for them to fetch.
fn hit_enrichments(model: &mut Model) -> Vec<Effect> {
    let viewport = model.viewport.1;
    let range = visible_range(model.search.selected, model.search.hits.len(), viewport);
    let mut effects = Vec::new();
    for index in range {
        // Same in-batch cap arithmetic as [`visible_enrichments`]: `busy`
        // settles only when `update` returns, so this loop carries it.
        if model.busy as usize + effects.len() >= MAX_IN_FLIGHT {
            break;
        }
        let Some(hit) = model.search.hits.get(index) else {
            continue;
        };
        if hit.kind != EntityKind::Release || hit.enriched {
            continue;
        }
        // ReleaseId::new cannot fail for an id the adapter already let
        // through (ids over zero are its entry rule); the `else` path is
        // a cheap skip, never a panic.
        let Some(release_id) = ReleaseId::new(hit.id) else {
            continue;
        };
        if !model.search.inflight.insert(release_id) {
            continue;
        }
        effects.push(Effect::LoadDetail {
            release: release_id,
            request: OpenRequest::EnrichHit(release_id),
        });
    }
    effects
}

// ---------------------------------------------------------------------------
// Key routing.
// ---------------------------------------------------------------------------

fn key_message(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    if let Some(effects) = global_key(model, &key) {
        return effects;
    }
    match model.screen() {
        Some(Screen::Home) => home_key(model, key.code),
        Some(Screen::Search) => search_key(model, key.code),
        Some(Screen::Entity) => entity_key(model, key.code),
        Some(Screen::Release) => release_key(model, key.code),
        Some(Screen::Wantlist) => wantlist_key(model, key.code),
        None => {
            model.push(Screen::Home);
            Vec::new()
        }
    }
}

/// Keys that mean the same thing on every screen.
fn global_key(model: &mut Model, key: &KeyEvent) -> Option<Vec<Effect>> {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(vec![Effect::Quit]);
    }
    match key.code {
        KeyCode::F(2) => {
            model.clear_status();
            if !matches!(model.screen(), Some(Screen::Wantlist)) {
                model.push(Screen::Wantlist);
            }
            Some(Vec::new())
        }
        KeyCode::F(1) => {
            model.clear_status();
            model.stack.clear();
            model.push(Screen::Home);
            Some(Vec::new())
        }
        _ => None,
    }
}

fn home_key(model: &mut Model, code: KeyCode) -> Vec<Effect> {
    if let KeyCode::Char(c) = code {
        // Space is *text* here; the prompt takes free-form queries.
        if c.is_ascii_graphic() || c == ' ' {
            model.home.input.push(c);
            return Vec::new();
        }
    }
    match code {
        KeyCode::Backspace => {
            model.home.input.pop();
            Vec::new()
        }
        KeyCode::Esc => {
            if model.home.input.is_empty() {
                // Nothing to clear is the "sure?" phase: a bare Esc over an
                // empty prompt is how you leave the app.
                vec![Effect::Quit]
            } else {
                model.home.input.clear();
                Vec::new()
            }
        }
        KeyCode::Left | KeyCode::BackTab => {
            cycle_filter_backwards(&mut model.home.filter);
            Vec::new()
        }
        KeyCode::Right | KeyCode::Tab => {
            cycle_filter(&mut model.home.filter);
            Vec::new()
        }
        KeyCode::Enter => open_intent(model),
        _ => Vec::new(),
    }
}

/// `none` → artist → release → label → `none`, both directions.
fn cycle_filter(filter: &mut Option<EntityKind>) {
    *filter = match *filter {
        None => Some(EntityKind::Artist),
        Some(EntityKind::Artist) => Some(EntityKind::Release),
        Some(EntityKind::Release) => Some(EntityKind::Label),
        Some(EntityKind::Label) => None,
    };
}

fn cycle_filter_backwards(filter: &mut Option<EntityKind>) {
    *filter = match *filter {
        None => Some(EntityKind::Label),
        Some(EntityKind::Artist) => None,
        Some(EntityKind::Release) => Some(EntityKind::Artist),
        Some(EntityKind::Label) => Some(EntityKind::Release),
    };
}

/// The home screen's only real action: parse the prompt, speak the intent.
/// An empty prompt does nothing — a prompt only argues when it is asked to.
fn open_intent(model: &mut Model) -> Vec<Effect> {
    let intent = browse::parse_intent(
        &model.home.input,
        model.home.filter.unwrap_or(EntityKind::Artist),
    );
    let mut effects = Vec::new();
    match intent {
        browse::Intent::Browse(kind, id) => effects.extend(open_entity(model, kind, id)),
        browse::Intent::Search(query) => {
            model.search = super::model::SearchScreen {
                query: query.clone(),
                filter: model.home.filter,
                page: 1,
                pages: 0,
                hits: Vec::new(),
                selected: 0,
                inflight: HashSet::new(),
                sort: SortMode::Sheet,
            };
            model.push(Screen::Search);
            model.clear_status();
            effects.push(Effect::Search {
                query,
                kind: model.home.filter,
                page: 1,
            });
        }
    }
    model.home.input.clear();
    effects
}

/// One place for "go to this entity from anywhere" — home, search, or the
/// wantlist. Artist and label push the entity screen and fetch profile plus
/// the first page of rows; a release pushes nothing until its reply's pending
/// token asks it to.
///
/// # Who calls this with what
///
/// - home: any kind (a release open from home tokenizes as `FromEntity`).
/// - search: artist and label hits only; release hits open directly.
/// - wantlist: artist and label entries; release entries open directly.
fn open_entity(model: &mut Model, kind: EntityKind, id: u64) -> Vec<Effect> {
    let mut effects = Vec::new();
    match kind {
        EntityKind::Artist => {
            model.entity = super::model::EntityScreen {
                kind,
                id,
                ..Default::default()
            };
            model.push(Screen::Entity);
            model.clear_status();
            effects.push(Effect::LoadArtistProfile(id));
            effects.push(Effect::LoadRows {
                kind: EntityKind::Artist,
                id,
                page: 1,
            });
        }
        EntityKind::Label => {
            model.entity = super::model::EntityScreen {
                kind,
                id,
                ..Default::default()
            };
            model.push(Screen::Entity);
            model.clear_status();
            effects.push(Effect::LoadLabelProfile(id));
            effects.push(Effect::LoadRows {
                kind: EntityKind::Label,
                id,
                page: 1,
            });
        }
        EntityKind::Release => match ReleaseId::new(id) {
            Some(release_id) => {
                let request = OpenRequest::FromEntity(release_id);
                model.await_open(request);
                effects.push(Effect::LoadDetail {
                    release: release_id,
                    request,
                });
            }
            None => {
                model.status = Some(Status::Error(format!(
                    "release id {id} is not a Discogs id"
                )));
            }
        },
    }
    effects
}

// ---------------------------------------------------------------------------
// Search screen.
// ---------------------------------------------------------------------------

fn search_key(model: &mut Model, code: KeyCode) -> Vec<Effect> {
    let hits = model.search.hits.len();
    let page = model.search.page;
    let pages = model.search.pages;
    match code {
        KeyCode::Char('w') => {
            let Some(hit) = model.search.hits.get(model.search.selected) else {
                return Vec::new();
            };
            want_something(model, hit.kind, hit.id, hit.title.clone())
        }
        KeyCode::Esc => {
            model.pop();
            Vec::new()
        }
        KeyCode::Up => {
            model.search.selected = model.search.selected.saturating_sub(1);
            hit_enrichments(model)
        }
        KeyCode::Down => {
            model.search.selected = (model.search.selected + 1).min(hits.saturating_sub(1));
            hit_enrichments(model)
        }
        KeyCode::Left if page > 1 => search_page(model, page - 1),
        KeyCode::Right if page < pages => {
            let kind = model.search.filter;
            let query = model.search.query.clone();
            vec![Effect::Search {
                query,
                kind,
                page: page + 1,
            }]
        }
        KeyCode::Enter => {
            let Some(hit) = model.search.hits.get(model.search.selected) else {
                return Vec::new();
            };
            match (hit.kind, ReleaseId::new(hit.id)) {
                (EntityKind::Release, Some(release_id)) => {
                    let request = OpenRequest::FromSearch(release_id, model.search.selected);
                    model.await_open(request);
                    vec![Effect::LoadDetail {
                        release: release_id,
                        request,
                    }]
                }
                (EntityKind::Artist, _) | (EntityKind::Label, _) => {
                    open_entity(model, hit.kind, hit.id)
                }
                _ => Vec::new(),
            }
        }
        // One press, one turn of the order wheel; the row under the
        // selection stays under it. Direct shortcuts: `1` rare, `0` sheet
        // (`2` price doesn't exist for search hits).
        KeyCode::Char('s') => cycle_hit_sort(model),
        KeyCode::Char('1') => {
            model.search.sort = SortMode::Rare;
            sort_hits_in_place(model)
        }
        KeyCode::Char('0') => {
            model.search.sort = SortMode::Sheet;
            search_page(model, model.search.page)
        }
        _ => Vec::new(),
    }
}

/// One page back, refetched with whatever the screen still knows.
fn search_page(model: &mut Model, page: u32) -> Vec<Effect> {
    let kind = model.search.filter;
    let query = model.search.query.clone();
    vec![Effect::Search { query, kind, page }]
}

// ---------------------------------------------------------------------------
// Entity screen (artist or label).
// ---------------------------------------------------------------------------

fn entity_key(model: &mut Model, code: KeyCode) -> Vec<Effect> {
    let rows = model.entity.rows.len();
    let page = model.entity.page;
    let pages = model.entity.pages;
    match code {
        KeyCode::Char('w') => {
            let name = model
                .entity
                .profile
                .as_ref()
                .map(Profile::name)
                .unwrap_or("this one")
                .to_string();
            want_something(model, model.entity.kind, model.entity.id, name)
        }
        KeyCode::Esc => {
            model.pop();
            Vec::new()
        }
        KeyCode::Home => {
            model.entity.selected = 0;
            visible_enrichments(model)
        }
        KeyCode::End => {
            model.entity.selected = rows.saturating_sub(1);
            visible_enrichments(model)
        }
        KeyCode::Up => {
            model.entity.selected = model.entity.selected.saturating_sub(1);
            visible_enrichments(model)
        }
        KeyCode::Down => {
            model.entity.selected = (model.entity.selected + 1).min(rows.saturating_sub(1));
            visible_enrichments(model)
        }
        KeyCode::Left if page > 1 => vec![Effect::LoadRows {
            kind: model.entity.kind,
            id: model.entity.id,
            page: page - 1,
        }],
        KeyCode::Right if page < pages => vec![Effect::LoadRows {
            kind: model.entity.kind,
            id: model.entity.id,
            page: page + 1,
        }],
        KeyCode::Enter => {
            let row_id = model
                .entity
                .rows
                .get(model.entity.selected)
                .map(|row| row.id);
            let Some(row_id) = row_id else {
                return Vec::new();
            };
            let request = OpenRequest::FromEntity(row_id);
            model.await_open(request);
            vec![Effect::LoadDetail {
                release: row_id,
                request,
            }]
        }
        // `s` reorders the loaded page only — see the sort section's doc.
        KeyCode::Char('s') => cycle_row_sort(model),
        // Direct shortcuts, no cycling: the sort claims are constant keys.
        // `0` (sheet order) is the honest exception — the wire's order is
        // unrecoverable from a sorted page without refetching, and one
        // user action is one request.
        KeyCode::Char('1') => {
            model.entity.sort = SortMode::Rare;
            sort_rows_in_place(model)
        }
        KeyCode::Char('2') => {
            model.entity.sort = SortMode::Price;
            sort_rows_in_place(model)
        }
        KeyCode::Char('0') => {
            model.entity.sort = SortMode::Sheet;
            vec![Effect::LoadRows {
                kind: model.entity.kind,
                id: model.entity.id,
                page: model.entity.page,
            }]
        }
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Release screen.
// ---------------------------------------------------------------------------

fn release_key(model: &mut Model, code: KeyCode) -> Vec<Effect> {
    let detail_id = model
        .release
        .as_ref()
        .map(|screen| (screen.detail.release.id.get(), screen.detail.describe()));
    let Some(screen) = model.release.as_mut() else {
        return Vec::new();
    };
    match code {
        KeyCode::Esc => {
            model.pop();
            Vec::new()
        }
        KeyCode::Down | KeyCode::PageDown => {
            screen.scroll += 1;
            Vec::new()
        }
        KeyCode::Up | KeyCode::PageUp => {
            screen.scroll = screen.scroll.saturating_sub(1);
            Vec::new()
        }
        KeyCode::Home => {
            screen.scroll = 0;
            Vec::new()
        }
        // Want *before* leaving; the screen stays where the user left it.
        KeyCode::Char('w') => match detail_id {
            Some((id, name)) => want_something(model, EntityKind::Release, id, name),
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Wantlist screen.
// ---------------------------------------------------------------------------

fn wantlist_key(model: &mut Model, code: KeyCode) -> Vec<Effect> {
    match code {
        KeyCode::Esc => {
            model.pop();
            Vec::new()
        }
        KeyCode::Up => {
            model.wantlist.selected = model.wantlist.selected.saturating_sub(1);
            Vec::new()
        }
        KeyCode::Down => {
            model.wantlist.selected =
                (model.wantlist.selected + 1).min(model.wants.len().saturating_sub(1));
            Vec::new()
        }
        KeyCode::Home => {
            model.wantlist.selected = 0;
            Vec::new()
        }
        KeyCode::End => {
            model.wantlist.selected = model.wants.len().saturating_sub(1);
            Vec::new()
        }
        KeyCode::Enter => {
            let Some(want) = model.wants.get(model.wantlist.selected) else {
                return Vec::new();
            };
            match (want.kind, ReleaseId::new(want.id)) {
                (EntityKind::Release, Some(release_id)) => {
                    let request = OpenRequest::FromWantlist(release_id, model.wantlist.selected);
                    model.await_open(request);
                    vec![Effect::LoadDetail {
                        release: release_id,
                        request,
                    }]
                }
                (EntityKind::Artist, _) | (EntityKind::Label, _) => {
                    open_entity(model, want.kind, want.id)
                }
                _ => Vec::new(),
            }
        }
        KeyCode::Char('d') => {
            model.wantlist.confirm = Some(model.wantlist.selected);
            Vec::new()
        }
        KeyCode::Char('y') => {
            let guard = model.wantlist.confirm;
            let Some(_row) = guard else {
                return Vec::new();
            };
            model.wantlist.selected = model
                .wantlist
                .selected
                .min(model.wants.len().saturating_sub(1));
            let Some(want) = model.wants.get(model.wantlist.selected) else {
                return Vec::new();
            };
            let kind = want.kind;
            let id = want.id;
            model.wantlist.confirm = None;
            vec![Effect::RemoveWant { kind, id }]
        }
        KeyCode::Char('n') => {
            model.wantlist.confirm = None;
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// One `w`, one want — unless it is already there, which is not silence but
/// a status line that says so.
fn want_something(model: &mut Model, kind: EntityKind, id: u64, name: String) -> Vec<Effect> {
    if id == 0 {
        return Vec::new();
    }
    if model.wants_contains(kind, id) {
        model.status = Some(Status::Info(format!("already wanted: {name}")));
        return Vec::new();
    }
    vec![Effect::AddWant { kind, id, name }]
}

// ---------------------------------------------------------------------------
// Tests: keys against a fresh model, assertions on state and effects only.
// ---------------------------------------------------------------------------

#[cfg(test)]
// The helpers build a `Model::default()` then assign the stack and the
// screen under test; that *is* the fixture, stated once per line.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use crate::domain::browse::{CatalogPage, CatalogRow, CommunityStats, SearchResult};
    use crate::domain::identity::ReleaseId;

    fn a_key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::from(code))
    }

    fn a_row(id: u64, master: bool, stats: Option<CommunityStats>, title: &str) -> CatalogRow {
        CatalogRow {
            id: ReleaseId::new(id).unwrap(),
            is_master: master,
            title: title.to_string(),
            artist: "artist".into(),
            year: Some(1994),
            format: "Vinyl".into(),
            label: "label".into(),
            catno: "CAT-1".into(),
            stats,
            lowest_price: None,
            enriched: false,
        }
    }

    fn a_detail(id: ReleaseId) -> ReleaseDetail {
        ReleaseDetail {
            release: crate::domain::release::Release {
                id,
                title: "The Now-Tales".into(),
                year: Some(1994),
                artists: vec![],
                formats: vec![],
                labels: vec![],
                thumb: String::new(),
                resource_url: String::new(),
                country: None,
                master_id: None,
            },
            released: None,
            genres: vec![],
            styles: vec![],
            tracklist: vec![],
            identifiers: vec![],
            notes: None,
            community: Some(CommunityStats { want: 8, have: 2 }),
            community_rating: None,
            lowest_price: Some(crate::domain::market::Money {
                minor_units: 1_005,
                currency: crate::domain::market::Currency::Usd,
            }),
            num_for_sale: Some(3),
            videos: vec![],
        }
    }

    /// `EntityScreen` has no `Default` style constructor; `open_entity` builds
    /// it. This helper states that shape once for the tests.
    fn entity_with(rows: Vec<CatalogRow>) -> Model {
        let mut model = Model::default();
        model.stack = vec![Screen::Entity];
        model.entity = super::super::model::EntityScreen {
            kind: EntityKind::Artist,
            id: 108713,
            profile: None,
            rows,
            page: 1,
            pages: 1,
            selected: 0,
            profile_loading: false,
            inflight: HashSet::new(),
            sort: super::super::model::SortMode::Sheet,
        };
        model.viewport = (100, 40);
        model
    }

    #[test]
    fn the_visible_window_enriches_at_most_max_in_flight_rows_at_once() {
        // Twenty unfetched rows around the selection, an empty channel: the
        // cap, not the viewport, decides. One tick claims eight — no more —
        // and each claim is unique, so a second tick before any reply does
        // not double-book anyone.
        let mut model = entity_with((1..=20).map(|n| a_row(n, false, None, "row")).collect());
        let effects = update(&mut model, Msg::Tick);
        assert_eq!(
            effects.len(),
            MAX_IN_FLIGHT,
            "the cap, counted before returning"
        );
        assert_eq!(model.busy, MAX_IN_FLIGHT as u32);
        assert_eq!(model.entity.inflight.len(), MAX_IN_FLIGHT);
        let again = update(&mut model, Msg::Tick);
        assert!(again.is_empty(), "busy already holds the cap");
        // Work already in the air spends the same budget: seven out leaves
        // exactly one more claim this tick.
        model.busy = 7;
        model.entity.inflight.clear();
        let effects = update(&mut model, Msg::Tick);
        assert_eq!(effects.len(), 1, "busy 7 + one claim = the cap");
    }

    #[test]
    fn quit_is_ctrl_c_on_any_screen() {
        for stack in [
            vec![Screen::Home],
            vec![Screen::Search],
            vec![Screen::Entity],
            vec![Screen::Wantlist],
        ] {
            let mut model = Model::default();
            model.stack = stack;
            let effects = update(
                &mut model,
                Msg::Key(KeyEvent::new_with_kind(
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL,
                    crossterm::event::KeyEventKind::Press,
                )),
            );
            assert_eq!(effects, vec![Effect::Quit]);
        }
    }
    #[test]
    fn f1_and_f2_navigate() {
        let mut model = Model::default();
        update(&mut model, a_key(KeyCode::F(1)));
        assert_eq!(model.screen(), Some(&Screen::Home));
        update(&mut model, a_key(KeyCode::F(2)));
        assert_eq!(model.screen(), Some(&Screen::Wantlist));
        update(&mut model, a_key(KeyCode::F(1)));
        assert_eq!(model.screen(), Some(&Screen::Home));
    }

    #[test]
    fn home_types_and_enter_searches_by_the_toggle() {
        let mut model = Model::default();
        model.push(Screen::Home);
        // The toggle starts on Artist after one Right.
        update(&mut model, a_key(KeyCode::Right));
        assert_eq!(model.home.filter, Some(EntityKind::Artist));
        for c in "Mobb Deep".chars() {
            update(&mut model, a_key(KeyCode::Char(c)));
        }
        assert_eq!(model.home.input, "Mobb Deep");
        update(&mut model, a_key(KeyCode::Left));
        assert_eq!(model.home.filter, None);

        let effects = update(&mut model, a_key(KeyCode::Enter));
        assert!(
            matches!(effects.first(), Some(Effect::Search { query, .. }) if query == "Mobb Deep")
        );
        assert_eq!(model.screen(), Some(&Screen::Search));
        assert_eq!(model.search.filter, None);
        assert!(model.home.input.is_empty(), "the prompt clears after go");
    }

    #[test]
    fn a_numeric_prompt_browses_the_toggled_kind() {
        let mut model = Model::default();
        model.push(Screen::Home);
        update(&mut model, a_key(KeyCode::Right));
        update(&mut model, a_key(KeyCode::Right));
        assert_eq!(model.home.filter, Some(EntityKind::Release));
        for c in "249504".chars() {
            update(&mut model, a_key(KeyCode::Char(c)));
        }
        let effects = update(&mut model, a_key(KeyCode::Enter));
        assert!(matches!(
            effects.first(),
            Some(Effect::LoadDetail { release, request: OpenRequest::FromEntity(_), .. })
                if release.get() == 249_504
        ));
        // Nothing is pushed until the detail arrives.
        assert_eq!(model.screen(), Some(&Screen::Home));
        assert!(model.pending_open.is_some());
    }

    #[test]
    fn a_prefixed_prompt_overrides_the_toggle() {
        let mut model = Model::default();
        model.push(Screen::Home);
        update(&mut model, a_key(KeyCode::Right));
        for c in "a:108713".chars() {
            update(&mut model, a_key(KeyCode::Char(c)));
        }
        let effects = update(&mut model, a_key(KeyCode::Enter));
        assert!(effects.contains(&Effect::LoadArtistProfile(108_713)));
        assert!(effects.contains(&Effect::LoadRows {
            kind: EntityKind::Artist,
            id: 108_713,
            page: 1,
        }));
        assert_eq!(model.screen(), Some(&Screen::Entity));
        assert_eq!(model.entity.kind, EntityKind::Artist);
    }

    #[test]
    fn rows_fill_the_entity_screen_and_kick_off_enrichment() {
        let mut model = Model::default();
        model.stack = vec![Screen::Entity];
        model.entity.kind = EntityKind::Artist;
        model.entity.id = 108_713;

        let msg = Msg::RowsLoaded {
            kind: EntityKind::Artist,
            id: 108_713,
            page: 1,
            data: Box::new(CatalogPage {
                page: 1,
                pages: 2,
                rows: vec![a_row(1, false, None, "one"), a_row(2, false, None, "two")],
            }),
        };
        let effects = update(&mut model, msg);

        assert_eq!(model.entity.rows.len(), 2);
        assert_eq!(model.entity.pages, 2);
        assert!(matches!(effects.first(), Some(Effect::Enrich(id)) if id.get() == 1));
        assert!(effects.len() == 2, "both unloaded rows were requested");
        assert!(
            model.entity.inflight.contains(&ReleaseId::new(1).unwrap()),
            "claimed inflight so a second tick cannot re-request"
        );
    }

    #[test]
    fn fetched_rows_are_never_enriched_again() {
        // Wire stats alone do not claim an enrichment — only a completed
        // fetch does (it also brings the price). So the stats-only row is
        // fetched once and never again.
        let mut model = entity_with(vec![a_row(3, false, None, "fetchable")]);
        let effects = update(&mut model, Msg::Tick);
        assert_eq!(effects.len(), 1, "the row wants its one detail fetch");

        update(
            &mut model,
            Msg::DetailLoaded(
                OpenRequest::Enrich(ReleaseId::new(3).unwrap()),
                Box::new(a_detail(ReleaseId::new(3).unwrap())),
            ),
        );
        let effects = update(&mut model, Msg::Tick);
        assert_eq!(effects, Vec::new(), "the enrichment claim holds");
    }

    #[test]
    fn a_stale_reply_for_another_entity_is_dropped() {
        let mut model = Model::default();
        model.stack = vec![Screen::Entity];
        model.entity.kind = EntityKind::Artist;
        model.entity.id = 108_713;

        let effects = update(
            &mut model,
            Msg::RowsLoaded {
                kind: EntityKind::Label,
                id: 108_713,
                page: 1,
                data: Box::new(CatalogPage {
                    page: 1,
                    pages: 1,
                    rows: vec![a_row(9, false, None, "wrong")],
                }),
            },
        );
        assert_eq!(effects, Vec::new());
        assert!(model.entity.rows.is_empty(), "the wrong entity never fills");
    }

    #[test]
    fn an_enrich_reply_fills_its_row_in_place() {
        let mut model = entity_with(vec![a_row(1, false, None, "one")]);
        let id = ReleaseId::new(1).unwrap();
        model.entity.inflight.insert(id);

        let effects = update(
            &mut model,
            Msg::DetailLoaded(OpenRequest::Enrich(id), Box::new(a_detail(id))),
        );
        assert!(effects.is_empty());
        assert_eq!(
            model.entity.rows[0].rarity_display(),
            Some(80),
            "want 8, have 2 → 0.8 → 80"
        );
        assert_eq!(
            model.entity.rows[0]
                .lowest_price
                .as_ref()
                .map(|m| m.minor_units),
            Some(1_005)
        );
        assert!(!model.entity.inflight.contains(&id), "guard cleared");
    }

    #[test]
    fn an_enrich_miss_clears_its_guard_and_says_nothing() {
        let mut model = entity_with(vec![a_row(1, false, None, "one")]);
        let id = ReleaseId::new(1).unwrap();
        model.entity.inflight.insert(id);

        let before = model.status.clone();
        let effects = update(&mut model, Msg::EnrichFailed(id));
        assert!(effects.is_empty() && before.is_none(), "quiet by design");
        assert!(
            !model.entity.inflight.contains(&id),
            "a later tick may retry"
        );
    }

    #[test]
    fn a_stale_detail_reply_is_dropped_and_the_live_token_survives() {
        // Open release 1 from the entity screen.
        let mut model = entity_with(vec![a_row(1, false, None, "one")]);
        let one = ReleaseId::new(1).unwrap();
        let two = ReleaseId::new(2).unwrap();
        model.await_open(OpenRequest::FromEntity(one));

        // While that fetch is out, the user opens release 2 from home —
        // which replaces the single pending token.
        model.stack = vec![Screen::Home];
        model.await_open(OpenRequest::FromEntity(two));

        // Release 1's detail arrives late. It answers a token nobody holds
        // any more, so it is dropped and no screen is pushed.
        let effects = update(
            &mut model,
            Msg::DetailLoaded(OpenRequest::FromEntity(one), Box::new(a_detail(one))),
        );
        assert!(effects.is_empty());
        assert!(model.release.is_none(), "no screen for a stale reply");

        // The live token survived the stale reply.
        assert_eq!(model.pending_open, Some(OpenRequest::FromEntity(two)));

        // Release 2 arrives: this one is wanted, so it lands.
        update(
            &mut model,
            Msg::DetailLoaded(OpenRequest::FromEntity(two), Box::new(a_detail(two))),
        );
        assert_eq!(model.screen(), Some(&Screen::Release));
        assert!(model.pending_open.is_none(), "the token is spent");
    }

    #[test]
    fn visible_range_stays_inside_the_rows() {
        assert_eq!(visible_range(0, 30, 40), 0..30);
        assert_eq!(visible_range(29, 30, 40), 29..30);
        assert_eq!(visible_range(0, 0, 40), 0..0);
        assert_eq!(visible_range(0, 5, 4), 0..2, "at least two rows, even here");
        assert_eq!(visible_range(10_000, 3, 40), 2..3);
    }

    #[test]
    fn entity_paging_uses_the_server_page_bound() {
        let mut model = entity_with(vec![a_row(1, false, None, "one")]);
        model.entity.page = 1;
        model.entity.pages = 3;
        let effects = update(&mut model, a_key(KeyCode::Right));
        assert!(matches!(
            effects.first(),
            Some(Effect::LoadRows { page: 2, .. })
        ));
        update(&mut model, a_key(KeyCode::Left));
        assert_eq!(model.entity.page, 1);
        // No page 0: the guard drops the keys, not the rows.
        let before = model.entity.page;
        update(&mut model, a_key(KeyCode::Left));
        assert_eq!(model.entity.page, before);
    }

    #[test]
    fn wantlist_confirm_delete_requires_y() {
        let mut model = Model::default();
        model.stack = vec![Screen::Wantlist];
        model.wants = vec![crate::domain::browse::WantEntry {
            kind: EntityKind::Artist,
            id: 7,
            name: "Seven".into(),
            added_at: 0,
        }];

        let effects = update(&mut model, a_key(KeyCode::Char('d')));
        assert!(effects.is_empty());
        assert_eq!(model.wantlist.confirm, Some(0));
        // `n` cancels without an effect; then `d` again; `y` proceeds.
        update(&mut model, a_key(KeyCode::Char('n')));
        assert_eq!(model.wantlist.confirm, None);
        let _ = effects;
        update(&mut model, a_key(KeyCode::Char('d')));
        let effects = update(&mut model, a_key(KeyCode::Char('y')));
        assert_eq!(
            effects,
            vec![Effect::RemoveWant {
                kind: EntityKind::Artist,
                id: 7
            }]
        );
        assert_eq!(model.wantlist.confirm, None);
    }

    #[test]
    fn search_results_page_and_open_a_release_hit() {
        let mut model = Model::default();
        model.stack = vec![Screen::Search];
        model.search = super::super::model::SearchScreen {
            query: "nomad".into(),
            filter: None,
            page: 1,
            pages: 2,
            hits: vec![
                SearchResult {
                    id: 108_713,
                    kind: EntityKind::Artist,
                    title: "An artist".into(),
                    year: None,
                    formats: vec![],
                    labels: vec![],
                    catno: None,
                    stats: None,
                    enriched: false,
                },
                SearchResult {
                    id: 249_504,
                    kind: EntityKind::Release,
                    title: "A release".into(),
                    year: Some("1994".into()),
                    formats: vec![],
                    labels: vec![],
                    catno: None,
                    stats: None,
                    enriched: false,
                },
            ],
            selected: 0,
            inflight: HashSet::new(),
            sort: super::super::model::SortMode::Sheet,
        };

        // Down to the release, then Enter: a detail request, a pending token.
        update(&mut model, a_key(KeyCode::Down));
        let effects = update(&mut model, a_key(KeyCode::Enter));
        assert!(matches!(
            effects.first(),
            Some(Effect::LoadDetail { release, request: OpenRequest::FromSearch(_, 1), .. })
                if release.get() == 249_504
        ));

        // PageRight asks for page 2.
        update(&mut model, a_key(KeyCode::Right));
        let effects = update(&mut model, a_key(KeyCode::Right));
        assert!(matches!(
            effects.first(),
            Some(Effect::Search { page: 2, .. })
        ));
    }

    #[test]
    fn the_release_screen_wants_with_one_key() {
        let mut model = entity_with(vec![a_row(1, false, None, "one")]);
        let id = ReleaseId::new(1).unwrap();
        let detail = a_detail(id);
        // Open from the entity row: the token matches, so the screen lands.
        model.await_open(OpenRequest::FromEntity(id));
        update(
            &mut model,
            Msg::DetailLoaded(OpenRequest::FromEntity(id), Box::new(detail.clone())),
        );
        assert_eq!(model.screen().cloned(), Some(Screen::Release));

        let effects = update(&mut model, a_key(KeyCode::Char('w')));
        assert!(matches!(
            effects.first(),
            Some(Effect::AddWant {
                kind: EntityKind::Release,
                id: 1,
                ..
            })
        ));
        // The store's reply lands; `w` again is idempotent: no effect, only
        // a status line.
        update(
            &mut model,
            Msg::WantAdded {
                kind: EntityKind::Release,
                id: 1,
                name: "The Now-Tales".into(),
                added_at: 123,
                added: true,
            },
        );
        let effects = update(&mut model, a_key(KeyCode::Char('w')));
        assert!(effects.is_empty());
    }

    #[test]
    fn esc_twice_on_an_empty_home_quits() {
        let mut model = Model::default();
        model.push(Screen::Home);
        assert_eq!(update(&mut model, a_key(KeyCode::Esc)), vec![Effect::Quit]);
    }

    #[test]
    fn esc_backs_out_of_screens() {
        let mut model = Model::default();
        model.stack = vec![Screen::Home, Screen::Search];
        update(&mut model, a_key(KeyCode::Esc));
        assert_eq!(model.screen(), Some(&Screen::Home));
    }

    fn a_hit(id: u64, kind: EntityKind) -> SearchResult {
        SearchResult {
            id,
            kind,
            title: "A hit".into(),
            year: None,
            formats: vec![],
            labels: vec![],
            catno: None,
            stats: None,
            enriched: false,
        }
    }

    fn release_id(id: u64) -> ReleaseId {
        ReleaseId::new(id).unwrap()
    }

    /// A detail whose wire formats say one carrier, plainly.
    fn a_format(name: &str) -> crate::domain::release::Format {
        crate::domain::release::Format {
            name: name.to_string(),
            qty: "1".into(),
            descriptions: vec![],
        }
    }

    #[test]
    fn a_hit_admitted_unknown_is_convicted_when_its_detail_names_cd() {
        let mut model = Model::default();
        model.stack = vec![Screen::Search];
        model.search.page = 1;
        model.search.hits = vec![a_hit(1, EntityKind::Release), a_hit(2, EntityKind::Release)];
        // Hit 2 already says vinyl; hit 1 arrives with no format claim at
        // all — the boundary cannot convict it, unknown is not "CD".
        model.search.hits[1].formats = vec!["Vinyl".into(), "12\"".into()];
        model.search.selected = 1;

        // The detail reply for hit 1 says CD: the better claim convicts.
        let mut cd = a_detail(release_id(1));
        cd.release.formats = vec![a_format("CD")];
        let _ = update(
            &mut model,
            Msg::DetailLoaded(OpenRequest::EnrichHit(release_id(1)), Box::new(cd)),
        );

        let ids: Vec<u64> = model.search.hits.iter().map(|hit| hit.id).collect();
        assert_eq!(ids, vec![2], "the CD hit left the table");
        assert_eq!(model.search.selected, 0, "removal above shifts the pin");
    }

    #[test]
    fn a_vinyl_detail_never_convicts_and_fill_only_marks_the_fetch() {
        let mut model = Model::default();
        model.stack = vec![Screen::Search];
        model.search.hits = vec![a_hit(3, EntityKind::Release)];

        let mut vinyl = a_detail(release_id(3));
        vinyl.release.formats = vec![a_format("Vinyl"), a_format("12\"")];
        let _ = update(
            &mut model,
            Msg::DetailLoaded(OpenRequest::EnrichHit(release_id(3)), Box::new(vinyl)),
        );

        assert_eq!(model.search.hits.len(), 1, "vinyl stays");
        assert_eq!(
            model.search.hits[0].formats,
            vec!["Vinyl".to_string(), "12\"".to_string()],
        );
    }

    #[test]
    fn an_entity_master_row_is_convicted_the_same_way() {
        // Real master rows arrive with an empty format cell — the listing
        // endpoint publishes nothing for them; unknown awaits conviction.
        let mut model = entity_with(vec![
            a_row(1, true, None, "unknown pressing"),
            a_row(2, false, Some(CommunityStats { want: 5, have: 5 }), "vinyl"),
        ]);
        model.entity.rows[0].format = String::new();
        model.entity.selected = 1;

        let mut cd = a_detail(release_id(1));
        cd.release.formats = vec![a_format("CD")];
        let _ = update(
            &mut model,
            Msg::DetailLoaded(OpenRequest::Enrich(release_id(1)), Box::new(cd)),
        );

        let ids: Vec<u64> = model.entity.rows.iter().map(|row| row.id.get()).collect();
        assert_eq!(ids, vec![2]);
        assert_eq!(model.entity.selected, 0);
    }

    #[test]
    fn unknown_formats_never_convict_without_a_saying() {
        // A detail with no formats at all is *unknown*, the same claim the
        // boundary used: the row stays, still honest about not knowing.
        let mut model = entity_with(vec![a_row(7, true, None, "quiet row")]);
        let _ = update(
            &mut model,
            Msg::DetailLoaded(
                OpenRequest::Enrich(release_id(7)),
                Box::new(a_detail(release_id(7))),
            ),
        );
        assert_eq!(model.entity.rows.len(), 1);
    }

    #[test]
    fn a_failed_job_closes_its_count_and_the_spinner_stops() {
        let mut model = Model::default();
        // A network request is counted when its effect leaves `update`;
        // count one as "in the air" the way `update` itself would.
        model.busy = 1;

        let failed = Msg::Failed {
            job: true,
            context: Some("search \"positive future music\"".into()),
            message: "some wire failure".into(),
        };
        update(&mut model, failed);

        assert_eq!(
            model.busy, 0,
            "the failed job is closed, spinner gets to stop"
        );
        assert!(model.status.is_some(), "the operator sees what failed");
    }

    #[test]
    fn a_failed_file_talk_never_touches_the_count() {
        let mut model = Model::default();
        // Three other jobs in the air; a want-add failure was never counted,
        // so it must not un-count someone else's job either.
        model.busy = 3;
        let failed = Msg::Failed {
            job: false,
            context: Some("adding a record".into()),
            message: "disk failure".into(),
        };
        update(&mut model, failed);

        assert_eq!(model.busy, 3);
        assert!(model.status.is_some());
    }

    #[test]
    fn the_boot_wantlist_reply_closes_the_boot_count() {
        // `runtime::run` counts the boot's wantlist load by hand before the
        // first key press; its reply must close it or the spinner never
        // stops for the whole session.
        let mut model = Model::default();
        model.busy = 1;
        update(&mut model, Msg::WantlistLoaded(vec![]));
        assert_eq!(model.busy, 0);
    }

    #[test]
    fn release_hits_enrich_and_fill_in_place() {
        let mut model = Model::default();
        model.stack = vec![Screen::Search];
        model.search.hits = vec![a_hit(249_504, EntityKind::Release)];

        // A tick asks for the visible release hit's detail, claiming it.
        assert!(matches!(
            update(&mut model, Msg::Tick).first(),
            Some(Effect::LoadDetail {
                release: _,
                request: OpenRequest::EnrichHit(ReleaseId { .. })
            })
        ));
        assert!(model.search.inflight.contains(&release_id(249_504)));

        // The reply fills the hit; the claim clears; the next tick is quiet.
        let _ = update(
            &mut model,
            Msg::DetailLoaded(
                OpenRequest::EnrichHit(release_id(249_504)),
                Box::new(a_detail(release_id(249_504))),
            ),
        );
        assert_eq!(model.search.hits[0].rarity_display(), Some(80));
        assert!(model.search.hits[0].enriched);
        assert!(!model.search.inflight.contains(&release_id(249_504)));
        assert_eq!(update(&mut model, Msg::Tick), Vec::new());

        // The reply was remembered: a *new* page carrying the same release
        // hits arrives dressed, no refetch.
        assert!(model.detail_fills.contains_key(&release_id(249_504)));
    }

    #[test]
    fn a_page_reload_is_dressed_from_the_session_fills() {
        let mut model = entity_with(Vec::new());
        let row = ReleaseId::new(7).unwrap();
        model.detail_fills.insert(
            row,
            super::super::model::DetailFill {
                stats: Some(CommunityStats { want: 8, have: 2 }),
                lowest_price: Some(crate::domain::market::Money {
                    minor_units: 1_000,
                    currency: crate::domain::market::Currency::Usd,
                }),
                format: "Vinyl, LP, Album".into(),
                label: "Acid Test".into(),
                catno: "AT-09".into(),
            },
        );

        // A page load whose wire rows carry no stats: the session knows
        // this release, so the row arrives dressed and no enrichment is
        // kicked off for it.
        let effects = update(
            &mut model,
            Msg::RowsLoaded {
                kind: EntityKind::Artist,
                id: 108_713,
                page: 1,
                data: Box::new(CatalogPage {
                    page: 1,
                    pages: 1,
                    rows: vec![a_row(7, false, None, "seven")],
                }),
            },
        );

        let dressed = &model.entity.rows[0];
        assert_eq!(dressed.rarity_display(), Some(80));
        assert_eq!(dressed.lowest_price.map(|m| m.minor_units), Some(1_000));
        assert!(dressed.enriched, "the fetch happened; the row must say so");
        assert!(effects.is_empty(), "nothing refetches for a known row");
    }

    #[test]
    fn artist_and_label_hits_never_fetch_a_detail() {
        let mut model = Model::default();
        model.stack = vec![Screen::Search];
        model.search.hits = vec![
            a_hit(108_713, EntityKind::Artist),
            a_hit(1, EntityKind::Label),
        ];
        assert_eq!(update(&mut model, Msg::Tick), Vec::new());

        // Moving onto an artist hit changes nothing: there are no counts.
        update(&mut model, a_key(KeyCode::Down));
        assert_eq!(update(&mut model, Msg::Tick), Vec::new());
    }

    #[test]
    fn a_failed_hit_enrich_clears_its_guard_and_says_nothing() {
        let mut model = Model::default();
        model.stack = vec![Screen::Search];
        model.search.hits = vec![a_hit(249_504, EntityKind::Release)];
        model.search.inflight.insert(release_id(249_504));

        let effects = update(&mut model, Msg::EnrichFailed(release_id(249_504)));
        assert!(effects.is_empty());
        assert!(model.status.is_none(), "enrichment failures stay quiet");
        assert!(!model.search.inflight.contains(&release_id(249_504)));

        // The cleared guard means a later tick may retry the fetch.
        assert!(matches!(
            update(&mut model, Msg::Tick).first(),
            Some(Effect::LoadDetail {
                request: OpenRequest::EnrichHit(_),
                ..
            })
        ));
    }

    #[test]
    fn a_master_row_enriches_through_its_main_release_and_closes_gaps() {
        // A master row: the compact wire never said format, label or catno,
        // and its id is the main release's id, so the one detail fetch both
        // counts the crowd and says what the pressing is.
        let mut row = a_row(249_504, true, None, "a master");
        row.format = String::new();
        row.label = String::new();
        row.catno = String::new();
        let mut model = entity_with(vec![row]);

        let effects = update(&mut model, Msg::Tick);
        assert!(matches!(
            effects.first(),
            Some(Effect::Enrich(id)) if id.get() == 249_504
        ));

        let mut detail = a_detail(release_id(249_504));
        detail.release.formats = vec![crate::domain::release::Format {
            name: "Vinyl".into(),
            qty: "1".into(),
            descriptions: vec!["LP".into()],
        }];
        detail.release.labels = vec![crate::domain::release::Label {
            id: 1,
            name: "Acid Test".into(),
            catno: "AT-09".into(),
        }];
        update(
            &mut model,
            Msg::DetailLoaded(OpenRequest::Enrich(release_id(249_504)), Box::new(detail)),
        );

        let row = &model.entity.rows[0];
        assert_eq!(row.format, "Vinyl, LP");
        assert_eq!(row.label, "Acid Test");
        assert_eq!(row.catno, "AT-09");
        assert_eq!(row.rarity_display(), Some(80));
        assert!(row.enriched, "one fetch, never more");
    }

    #[test]
    fn s_sorts_the_page_rarest_first_with_unknowns_sinking() {
        let mut model = entity_with(vec![
            a_row(1, false, None, "not yet asked"),
            a_row(
                2,
                false,
                Some(CommunityStats { want: 2, have: 8 }),
                "common",
            ),
            a_row(
                3,
                false,
                Some(CommunityStats { want: 9, have: 1 }),
                "critical",
            ),
        ]);

        // `s`, one press, one turn: rare first. The selection rides its
        // row, not its index.
        let effects = update(&mut model, a_key(KeyCode::Char('s')));
        assert_eq!(model.entity.sort, super::super::model::SortMode::Rare);
        assert!(effects.is_empty(), "sorting never touches the wire");
        let ids: Vec<u64> = model.entity.rows.iter().map(|r| r.id.get()).collect();
        assert_eq!(ids, vec![3, 2, 1], "rarest first, unknown last");
        assert_eq!(model.entity.selected, 2, "the row stays under the finger");

        // The wheel keeps turning between rare and price; unknowns keep
        // their sheet order at the bottom for both.
        update(&mut model, a_key(KeyCode::Char('s')));
        assert_eq!(model.entity.sort, super::super::model::SortMode::Price);
        let ids: Vec<u64> = model.entity.rows.iter().map(|r| r.id.get()).collect();
        assert_eq!(ids, vec![3, 2, 1], "no input rows carry a price yet");
    }

    #[test]
    fn s_sorts_by_price_once_details_landed() {
        let mut model = entity_with(vec![
            a_row(1, false, Some(CommunityStats { want: 5, have: 5 }), "cheap"),
            a_row(2, false, Some(CommunityStats { want: 5, have: 5 }), "dear"),
        ]);
        let usd = |minor: i64| crate::domain::market::Money {
            minor_units: minor,
            currency: crate::domain::market::Currency::Usd,
        };
        model.entity.rows[0].lowest_price = Some(usd(1_000));
        model.entity.rows[1].lowest_price = Some(usd(45_000));
        model.entity.selected = 0;

        update(&mut model, a_key(KeyCode::Char('s')));
        update(&mut model, a_key(KeyCode::Char('s'))); // rare → price

        let ids: Vec<u64> = model.entity.rows.iter().map(|r| r.id.get()).collect();
        assert_eq!(ids, vec![2, 1], "priciest first");
        assert_eq!(model.entity.selected, 1, "selection follows the row");
    }

    #[test]
    fn a_fresh_page_sorts_on_arrival_so_the_order_survives_page_turns() {
        let mut model = entity_with(Vec::new());
        model.entity.sort = super::super::model::SortMode::Rare;
        let effects = update(
            &mut model,
            Msg::RowsLoaded {
                kind: EntityKind::Artist,
                id: 108_713,
                page: 2,
                data: Box::new(CatalogPage {
                    page: 2,
                    pages: 9,
                    rows: vec![
                        a_row(11, false, None, "unknown"),
                        a_row(12, false, Some(CommunityStats { want: 9, have: 1 }), "rare"),
                    ],
                }),
            },
        );

        let ids: Vec<u64> = model.entity.rows.iter().map(|r| r.id.get()).collect();
        assert_eq!(ids, vec![12, 11], "page two arrives already sorted");
        // The unknown row sits inside the visible window once sorted, so
        // the ticker asks for its detail the same as it always does. (The
        // stats row was built unfetched too, so it asks as well — first.)
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::Enrich(id) if id.get() == 11))
        );
    }

    #[test]
    fn one_and_two_sort_directly_and_zero_refetches_the_wire_order() {
        let mut model = entity_with(vec![
            a_row(1, false, Some(CommunityStats { want: 5, have: 5 }), "mid"),
            a_row(2, false, Some(CommunityStats { want: 9, have: 1 }), "rare"),
        ]);
        model.entity.rows[0].lowest_price = Some(crate::domain::market::Money {
            minor_units: 5_000,
            currency: crate::domain::market::Currency::Usd,
        });
        model.entity.rows[1].lowest_price = Some(crate::domain::market::Money {
            minor_units: 500,
            currency: crate::domain::market::Currency::Usd,
        });

        // `1` goes straight to rarity, no cycling.
        update(&mut model, a_key(KeyCode::Char('1')));
        assert_eq!(model.entity.sort, SortMode::Rare);
        let ids: Vec<u64> = model.entity.rows.iter().map(|r| r.id.get()).collect();
        assert_eq!(ids, vec![2, 1], "rarest first");

        // `2` goes straight to price.
        update(&mut model, a_key(KeyCode::Char('2')));
        assert_eq!(model.entity.sort, SortMode::Price);
        let ids: Vec<u64> = model.entity.rows.iter().map(|r| r.id.get()).collect();
        assert_eq!(ids, vec![1, 2], "priciest first");

        // `0` refuses to fake the wire order: it refetches the current
        // page — one user action, one request — and sorts it as sheet.
        let effects = update(&mut model, a_key(KeyCode::Char('0')));
        assert_eq!(model.entity.sort, SortMode::Sheet);
        assert!(matches!(
            effects.first(),
            Some(Effect::LoadRows { page: 1, .. })
        ));
    }

    #[test]
    fn the_search_screen_sorts_rare_directly_too() {
        let mut model = Model::default();
        model.stack = vec![Screen::Search];
        model.search.hits = vec![a_hit(1, EntityKind::Artist), a_hit(2, EntityKind::Release)];
        model.search.hits[1].stats = Some(CommunityStats { want: 9, have: 1 });
        model.search.pages = 1;
        model.search.page = 1;

        let effects = update(&mut model, a_key(KeyCode::Char('1')));
        assert_eq!(model.search.sort, SortMode::Rare);
        assert!(effects.is_empty(), "sorting never touches the wire");
        assert_eq!(
            model.search.hits.iter().map(|h| h.id).collect::<Vec<_>>(),
            vec![2, 1]
        );

        // `0` on search refetches the current page as well.
        let effects = update(&mut model, a_key(KeyCode::Char('0')));
        assert_eq!(model.search.sort, SortMode::Sheet);
        assert!(matches!(
            effects.first(),
            Some(Effect::Search { page: 1, .. })
        ));
    }

    #[test]
    fn search_sort_cycles_sheet_to_rare_and_stays_there() {
        let mut model = Model::default();
        model.stack = vec![Screen::Search];
        model.search.hits = vec![a_hit(1, EntityKind::Artist), a_hit(2, EntityKind::Release)];
        model.search.hits[1].stats = Some(CommunityStats { want: 9, have: 1 });

        update(&mut model, a_key(KeyCode::Char('s')));
        assert_eq!(model.search.sort, super::super::model::SortMode::Rare);
        assert_eq!(
            model.search.hits.iter().map(|h| h.id).collect::<Vec<_>>(),
            vec![2, 1],
            "the rare release hit rides to the top"
        );

        // Once sorted, the wheel has nowhere honest to go but rare.
        update(&mut model, a_key(KeyCode::Char('s')));
        assert_eq!(model.search.sort, super::super::model::SortMode::Rare);
    }
}
