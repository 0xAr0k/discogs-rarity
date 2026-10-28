//! The digger's renderer: `model → pixels`, nothing else.
//!
//! # What the view is allowed to know
//!
//! Everything about *presentation* — layout, truncation, which color a
//! rarity band wears — and nothing about *behavior*. The only arithmetic is
//! presentation arithmetic (`cut`, `date_of`, `rarity_style`), pure and
//! tested at the bottom; `update` never calls any of it.
//!
//! # Truncation over scrolling
//!
//! Tables cut text at the column edge rather than wrapping rows; a table
//! that shoves columns sideways is worse than a title that ends early.
//! [`cut`] is the one truncation rule: take characters, never pad.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};

use crate::domain::browse::{CatalogRow, EntityKind, SearchResult, WantEntry};
use crate::domain::market::Money;
use crate::tui::model::{Model, Profile, SalesState, Screen, Status};

// Palette — Kanagawa (k/github.com/rebelot/kanagawa). The app paints *no*
// background of its own: every style only sets a foreground so a
// transparent-capable terminal shows through. The single exception is the
// selected row — Kanagawa's selection band (`waveBlue2`), because a
// selection you cannot point at fails the sheet.
const ACCENT: Color = Color::Rgb(127, 180, 202); // springBlue #7fb4ca
const DIM: Color = Color::Rgb(114, 113, 105); // sumiInk3 #727169
const CRITICAL: Color = Color::Rgb(232, 36, 36); // samuraiRed #e82424
const COMMON: Color = Color::Rgb(152, 187, 108); // springGreen #98bb6c
const NOTICEABLE: Color = Color::Rgb(230, 195, 132); // carpYellow #e6c384
/// The selected row: Kanagawa's selection ground.
const SELECTED_BG: Color = Color::Rgb(45, 74, 103); // waveBlue2 #2d4a67
/// Body text — Kanagawa's oldWhite; whatever white the terminal would
/// inherit is warmed to the theme.
const INK: Color = Color::Rgb(200, 192, 147); // oldWhite #c8c093

fn dim() -> Style {
    Style::new().fg(DIM)
}

fn ink() -> Style {
    Style::new().fg(INK)
}

/// Any cell that must *carry the band* on the selected row: rarity wears
/// its own temperature, so the row's selection style cannot simply paint
/// over it — the cell style is patched onto the row's, and bg None would
/// leave a hole. On the band the score keeps its color.
fn rarity_on_selection(score: Option<u16>) -> Style {
    rarity_style(score).bg(SELECTED_BG)
}

/// Braille spinner, one glyph per tick.
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Cut `s` to `width` characters (not bytes).
fn cut(s: &str, width: usize) -> String {
    s.chars().take(width).collect()
}

/// `2023-11-14` from a unix timestamp, no date library. Hinnant's
/// `civil_from_days` — the same explanation the adapter gives for its
/// forward conversion; the two arithmetics are one rule read both ways.
fn date_of(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month + 2) / 5 + 1;
    let month = if month < 10 { month + 3 } else { month - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

/// The slice of table rows the viewport shows: centered on the selection,
/// clamped to the edges, stateless — every frame derives it fresh, so a
/// page swap or a resize re-anchors itself. `rows_fit` is the exact number
/// of body rows the framed area can show; when fewer fit than exist, moving
/// past the visible edge slides the sheet instead of hiding the cursor.
fn scroll_window(selected: usize, total: usize, rows_fit: usize) -> std::ops::Range<usize> {
    if rows_fit == 0 || total == 0 {
        return 0..0;
    }
    let fit = rows_fit.min(total);
    let max_start = total - fit;
    let start = selected.saturating_sub(fit / 2).min(max_start);
    start..start + fit
}

/// Rarity band colors, a temperature: cold blue for common, warm amber
/// past the halfway feel, hot blur-red when the want-pressure burns.
/// Thresholds mirror `domain::rarity::band_of`'s first cut, scaled to
/// walks: <50, <75, else here.
fn rarity_style(score: Option<u16>) -> Style {
    match score {
        None | Some(0) => Style::new().fg(DIM),
        Some(s) if s < 50 => Style::new().fg(COMMON),
        Some(s) if s < 75 => Style::new().fg(NOTICEABLE),
        Some(_) => Style::new().fg(CRITICAL).add_modifier(Modifier::BOLD),
    }
}

/// Money with the currency glyph front: `$29.99`. `Money::Display` renders
/// `USD 29.99` for the report; the digger wants the glyph.
fn price(money: Option<&Money>) -> String {
    match money {
        None => "—".to_string(),
        Some(m) => match m.currency {
            crate::domain::market::Currency::Usd => format!("${:.2}", m.minor_units as f64 / 100.0),
            crate::domain::market::Currency::Eur => format!("€{:.2}", m.minor_units as f64 / 100.0),
            crate::domain::market::Currency::Gbp => format!("£{:.2}", m.minor_units as f64 / 100.0),
            crate::domain::market::Currency::Jpy => format!("¥{}", m.minor_units),
        },
    }
}

// ---------------------------------------------------------------------------
// One draw call, three fixed bands, per-screen bodies.
// ---------------------------------------------------------------------------

/// Render one frame; the runtime calls this after every key and every tick.
///
/// # Pure, but not stateless
///
/// The model is read top-down: layout is fixed (breadcrumb, body, status
/// band), and the focused screen owns the body's layout. Nothing here
/// mutates; `frame` is ratatui's drawing target, not the model.
pub fn draw(frame: &mut Frame, model: &Model) {
    let band = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());

    // Transparent by design: the frame paints no ground of its own, so the
    // terminal's background — solid or alpha — shows through everywhere.

    breadcrumb_band(frame, band[0], model);
    match model.screen() {
        Some(Screen::Home) => home_body(frame, band[1], model),
        Some(Screen::Search) => search_body(frame, band[1], model),
        Some(Screen::Entity) => entity_body(frame, band[1], model),
        Some(Screen::Release) => release_body(frame, band[1], model),
        Some(Screen::Wantlist) => wantlist_body(frame, band[1], model),
        None => home_body(frame, band[1], model),
    }
    status_band(frame, band[2], model);
}

// ---------------------------------------------------------------------------
// Loading claims: an empty body must say *why* it is empty.
// ---------------------------------------------------------------------------

/// A centered, animated badge for a body waiting on its first reply.
///
/// `what` names the job ("searching Discogs…", "fetching releases…"), so a
/// blank region is never something the user has to interpret — the fetch is
/// the claim, and the spinner is its proof of life.
fn loader_badge(frame: &mut Frame, area: Rect, model: &Model, what: &str) {
    let pockets = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);
    let spinner = SPINNER[(model.ticks % SPINNER.len() as u64) as usize];
    let line = Line::from(vec![
        Span::styled(
            format!("{spinner} {what}"),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "   you press, it requests — the pacing is the rate limit",
            dim(),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line).alignment(Alignment::Center),
        pockets[1],
    );
}

/// What the breadcrumb's spinner is doing right now — the coarse, truthful
/// version. Fine-grained claims live on the bodies themselves.
fn busy_label(model: &Model) -> &'static str {
    if model.pending_open.is_some() {
        "opening…"
    } else {
        match model.screen() {
            Some(Screen::Home) | None => "loading wantlist…",
            Some(Screen::Search) => "searching…",
            Some(Screen::Entity) => "fetching…",
            Some(Screen::Release) => "fetching detail…",
            Some(Screen::Wantlist) => "loading…",
        }
    }
}

/// Where we are: `digger · search "nbhd"`, `digger · artist SomeName`, plus
/// a spinner while the worker is not quiet.
fn breadcrumb_band(frame: &mut Frame, area: Rect, model: &Model) {
    let far = match model.screen() {
        Some(Screen::Search) => {
            format!("· search “…{}”", cut(&model.search.query, 36))
        }
        Some(Screen::Entity) => match model.entity.profile.as_ref().map(Profile::name) {
            Some(name) => format!("· {} {name}", entity_word(model.entity.kind)),
            None => format!("· {} {}", entity_word(model.entity.kind), model.entity.id),
        },
        Some(Screen::Release) => model
            .release
            .as_ref()
            .map(|screen| format!("· {}", cut(&screen.detail.release.title, 48)))
            .unwrap_or_else(|| "· release".to_string()),
        Some(Screen::Wantlist) => "· wantlist".to_string(),
        Some(Screen::Home) | None => "· home".to_string(),
    };
    let mut spans = vec![
        Span::styled("digger ", Style::new().fg(ACCENT)),
        // The busy label rides on the same line; reserve room for it so the
        // breadcrumb and the spinner never fight for the last column.
        Span::styled(cut(&far, area.width.saturating_sub(20) as usize), dim()),
    ];
    if model.busy > 0 {
        let spinner = SPINNER[(model.ticks % SPINNER.len() as u64) as usize];
        spans.push(Span::styled(
            format!(" {spinner} {}", busy_label(model)),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(Line::from(spans), area);
}

fn entity_word(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Artist => "artist",
        EntityKind::Release => "release",
        EntityKind::Label => "label",
    }
}

/// Status line when present, hint line always.
fn status_band(frame: &mut Frame, area: Rect, model: &Model) {
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    if let Some(status) = &model.status {
        let span = match status {
            Status::Error(text) => Span::styled(
                cut(text, split[0].width as usize),
                Style::new().fg(CRITICAL).add_modifier(Modifier::BOLD),
            ),
            Status::Info(text) => {
                Span::styled(cut(text, split[0].width as usize), Style::new().fg(ACCENT))
            }
        };
        frame.render_widget(Line::from(span), split[0]);
    } else {
        // With no status to clear, the band can report its quiet progress:
        // rows around the selection fetch their detail in place, one claim
        // each, and the count says how many are in the air right now.
        let in_flight = match model.screen() {
            Some(Screen::Entity) => model.entity.inflight.len(),
            Some(Screen::Search) => model.search.inflight.len(),
            _ => 0,
        };
        if in_flight > 0 {
            frame.render_widget(
                Line::from(Span::styled(
                    format!("⟳ {in_flight} row(s) fetching their detail…"),
                    Style::new().fg(ACCENT),
                )),
                split[0],
            );
        }
    }

    let hints = match model.screen() {
        Some(Screen::Home) => {
            "type an id (a:23 r:59 l:1) or a name · ←/→ filter · ⏎ go · esc clear/quit · F2 wantlist · ctrl+c quit"
        }
        Some(Screen::Search) => {
            "↑/↓ move · ←/→ page · 1 rare · 0 sheet · ⏎ open · w want · esc back · F2 wantlist"
        }
        Some(Screen::Entity) => {
            "↑/↓ move · ←/→ page · 1 rare · 2 price · 0 sheet · ⏎ open · w want · esc back · F2 wantlist"
        }
        Some(Screen::Release) => "↑/↓ scroll · w want · esc back · F2 wantlist",
        Some(Screen::Wantlist) => "↑/↓ move · ⏎ open · d then y/n remove · esc back",
        None => "",
    };
    frame.render_widget(Line::from(Span::styled(hints, dim())), split[1]);
}

// ---------------------------------------------------------------------------
// Home.
// ---------------------------------------------------------------------------

fn home_body(frame: &mut Frame, area: Rect, model: &Model) {
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(area);

    let title = Line::from(vec![
        Span::styled(
            "Diggers",
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" — dig the Discogs database", dim()),
    ]);
    frame.render_widget(Paragraph::new(title), parts[0]);

    let filter = match model.home.filter {
        None => "any",
        Some(EntityKind::Artist) => "artist",
        Some(EntityKind::Release) => "release",
        Some(EntityKind::Label) => "label",
    };
    let prompt_line = Line::from(vec![
        Span::styled("❯ ", Style::new().fg(ACCENT)),
        Span::styled(model.home.input.clone(), ink()),
        Span::styled("▌", Style::new().fg(ACCENT)),
        Span::styled(format!("   filter: {filter} (←/→)"), dim()),
    ]);
    frame.render_widget(
        Paragraph::new(prompt_line).block(Block::default().borders(Borders::TOP | Borders::BOTTOM)),
        parts[1],
    );

    let help = vec![
        Line::from(Span::styled(
            "How to dig",
            ink().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "  108713              an artist, by id",
            dim(),
        )),
        Line::from(Span::styled(
            "  r:249504            a pressing (release), by id",
            dim(),
        )),
        Line::from(Span::styled("  l:1                 a label, by id", dim())),
        Line::from(Span::styled(
            "  mobb deep           search the database by name",
            dim(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "search needs DISCOGS_TOKEN in the environment (make one at \
             discogs.com/settings/developers); browsing by id works without it (slower)",
            dim(),
        )),
    ];
    frame.render_widget(Paragraph::new(help), parts[2]);
}

// ---------------------------------------------------------------------------
// Search.
// ---------------------------------------------------------------------------

fn search_body(frame: &mut Frame, area: Rect, model: &Model) {
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let header = Line::from(Span::styled(
        format!(
            "“{}” — page {} of {} — {}",
            model.search.query,
            model.search.page,
            model.search.pages,
            model.search.hits.len()
        ),
        dim(),
    ));

    // Same windowing as the entity table: the selection slides the sheet
    // instead of running off its bottom edge.
    let rows_fit = parts[1].height.saturating_sub(3) as usize; // borders + header
    let window = scroll_window(
        model.search.selected,
        model.search.hits.len(),
        rows_fit.max(1),
    );
    let rows: Vec<Row> = model.search.hits[window.start..window.end]
        .iter()
        .enumerate()
        .map(|(in_window, hit)| {
            hit_to_row(
                window.start + in_window,
                hit,
                model.search.selected == window.start + in_window,
            )
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Min(28),
            Constraint::Length(6),
            Constraint::Length(12),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Length(5),
        ],
    )
    .column_spacing(2)
    .header(search_header())
    .block(sheet_block(
        model.search.page,
        model.search.pages,
        "results",
        model.search.sort.label(),
    ));

    if model.search.hits.is_empty() {
        // Same rule as everywhere else: waiting is visible, empty is named.
        if model.busy > 0 {
            loader_badge(frame, parts[1], model, "searching Discogs…");
        } else {
            frame.render_widget(header, parts[0]);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!("no results for “{}”", model.search.query),
                    dim(),
                )),
                parts[1],
            );
        }
        return;
    }
    frame.render_widget(header, parts[0]);
    frame.render_widget(table, parts[1]);
}

fn search_header() -> Row<'static> {
    Row::new(vec![
        Cell::new("title"),
        Cell::new("year"),
        Cell::new("format"),
        Cell::new("label"),
        Cell::new("cat#"),
        Cell::from(right("rare".into())),
    ])
    .style(Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
}

/// The spreadsheet frame both release tables sit in: a full border with the
/// order and paging claims in the corner. Breathing room over naked rows —
/// the table reads like a sheet, not a spill of text. `order` names how the
/// rows on screen are actually ordered (the `s` wheel), so the title never
/// claims an order the rows do not keep.
fn sheet_block(page: u32, pages: u32, what: &str, order: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(DIM))
        .title(Span::styled(
            format!(" {what} · {order} · {page}/{pages} "),
            dim(),
        ))
}

/// A search-hit line. The `kind` column is gone — the rarity column says
/// more about a dig than a type word does — and selection is Kanagawa's
/// blue band, carried by every cell that has its own style.
///
/// Rarity claims per kind: a release hit says `…` until its detail lands
/// and `—` if that detail carried no community block; an artist or label
/// hit says `—` forever, because no endpoint publishes their want/have.
fn hit_to_row(_index: usize, hit: &SearchResult, selected: bool) -> Row<'static> {
    let selection = |base: Style| {
        if selected {
            Style::new().fg(INK).bg(SELECTED_BG)
        } else {
            base
        }
    };
    let style = selection(dim());
    let rare = match hit.kind {
        EntityKind::Release => match hit.rarity_display() {
            Some(score) => score.to_string(),
            None if hit.enriched => "—".to_string(),
            None => "…".to_string(),
        },
        _ => "—".to_string(),
    };
    let mut rare_cell = Cell::from(right(rare));
    rare_cell = rare_cell.style(if selected {
        rarity_on_selection(hit.rarity_display())
    } else {
        rarity_style(hit.rarity_display())
    });
    Row::new(vec![
        Cell::from(cut(&hit.title, 36)),
        Cell::from(hit.year.clone().unwrap_or_else(|| "—".into())),
        Cell::from(or_unknown(&hit.formats.join(", "), 12)),
        Cell::from(cut(&hit.labels.join(", "), 14)),
        Cell::from(cut(hit.catno.as_deref().unwrap_or("—"), 10)),
        rare_cell,
    ])
    .style(style)
}

// ---------------------------------------------------------------------------
// Entity (artist / label): profile paragraph + the release table.
// ---------------------------------------------------------------------------

fn entity_body(frame: &mut Frame, area: Rect, model: &Model) {
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(0)])
        .split(area);

    // Profile: name bold, then whatever identity facts exist.
    let mut head = vec![Line::from(match &model.entity.profile {
        Some(Profile::Artist(profile)) => vec![
            Span::styled(
                profile.name.clone(),
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            match profile.realname.as_deref() {
                Some(realname) if !realname.is_empty() => {
                    Span::styled(format!("  ({realname})"), dim())
                }
                _ => Span::styled("  loading profile…", dim()),
            },
        ],
        Some(Profile::Label(profile)) => vec![
            Span::styled(
                profile.name.clone(),
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                match profile.parent_label.as_deref() {
                    Some(parent) if !parent.is_empty() => format!("  · parent: {parent}"),
                    _ => String::new(),
                },
                dim(),
            ),
        ],
        None => vec![Span::styled(
            match model.entity.kind {
                EntityKind::Artist => "loading artist…",
                EntityKind::Label => "loading label…",
                EntityKind::Release => "opening release…",
            },
            dim(),
        )],
    })];
    let context: Option<String> = match &model.entity.profile {
        Some(Profile::Artist(profile)) => {
            if profile.aliases.is_empty()
                && profile.members.is_empty()
                && profile.namevariations.is_empty()
            {
                None
            } else {
                let mut all = profile.namevariations.clone();
                all.extend(
                    profile
                        .aliases
                        .iter()
                        .map(|alias| format!("a.k.a. {alias}")),
                );
                if !profile.members.is_empty() {
                    all.push(format!("members: {}", profile.members.join(", ")));
                }
                Some(all.join("  ·  "))
            }
        }
        Some(Profile::Label(profile)) => Some(if profile.sublabels.is_empty() {
            profile.urls.join("  ·  ")
        } else {
            format!("sublabels: {}", profile.sublabels.join(", "))
        }),
        None => None,
    };
    if let Some(context) = context {
        head.push(Line::from(Span::styled(cut(&context, 160), dim())));
    }
    frame.render_widget(Paragraph::new(head), parts[0]);

    // The table. An empty table is a claim: either a job is in the air or
    // the wire really returned nothing — never a plain blank region.
    if model.entity.rows.is_empty() {
        if model.busy > 0 {
            loader_badge(
                frame,
                parts[1],
                model,
                match model.entity.profile_loading {
                    true => "fetching profile…",
                    false => "fetching releases…",
                },
            );
        } else {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "no rows: nothing came back for this page",
                    dim(),
                )),
                parts[1],
            );
        }
        return;
    }

    // Show the window around the selection, not the whole page: ↑/↓ at the
    // sheet's bottom edge slides the rows, so every row of a 100-row page
    // is reachable with the selection alone.
    let rows_fit = parts[1].height.saturating_sub(3) as usize; // borders + header
    let window = scroll_window(
        model.entity.selected,
        model.entity.rows.len(),
        rows_fit.max(1),
    );
    let rows: Vec<Row> = model.entity.rows[window.start..window.end]
        .iter()
        .enumerate()
        .map(|(in_window, row)| row_to_row(row, model.entity.selected == window.start + in_window))
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Min(28),
            Constraint::Length(15),
            Constraint::Length(17),
            Constraint::Length(10),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(8),
        ],
    )
    .column_spacing(2)
    .header(release_header())
    .block(sheet_block(
        model.entity.page,
        model.entity.pages,
        "releases",
        model.entity.sort.label(),
    ));

    frame.render_widget(table, parts[1]);
}

fn release_header() -> Row<'static> {
    Row::new(vec![
        Cell::from(right("year".into())),
        Cell::new("title"),
        Cell::new("format"),
        Cell::new("label"),
        Cell::new("cat#"),
        Cell::from(right("want".into())),
        Cell::from(right("have".into())),
        Cell::from(right("rare".into())),
        Cell::from(right("price".into())),
    ])
    .style(Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
}

/// One table line. Unselected rows sit quiet in Kanagawa's ink3 grey; the
/// selected row rides the `waveBlue2` band with warmed-white glyphs, and
/// every styled cell carries the band so it reads as one solid line.
fn row_to_row(row: &CatalogRow, selected: bool) -> Row<'static> {
    let selection = |base: Style| {
        if selected {
            Style::new().fg(INK).bg(SELECTED_BG)
        } else {
            base
        }
    };
    let style = selection(dim());
    let want = match row.stats {
        Some(stats) => stats.want.to_string(),
        None => "…".to_string(),
    };
    // Same claim `want` makes: a number once the wire (or an enrichment)
    // carried it, `…` until then — never `0`, which would read as
    // "nobody owns this".
    let have = match row.stats {
        Some(stats) => stats.have.to_string(),
        None => "…".to_string(),
    };
    let rarity = match row.rarity_display() {
        None => "…".to_string(),
        Some(score) => score.to_string(),
    };
    // Rarity colors the cell, and on the selected row the temperature is
    // re-tilted black-on-orange so the band stays one solid ember.
    let mut rarity_cell = Cell::from(right(rarity));
    rarity_cell = rarity_cell.style(if selected {
        rarity_on_selection(row.rarity_display())
    } else {
        rarity_style(row.rarity_display())
    });
    Row::new(vec![
        Cell::from(right(match row.year {
            Some(year) => year.to_string(),
            None => "—".to_string(),
        })),
        Cell::from(cut(&row.title, 48)),
        Cell::from(or_unknown(&row.format, 16)),
        Cell::from(or_unknown(&row.label, 18)),
        Cell::from(or_unknown(&row.catno, 10)),
        Cell::from(right(want)),
        Cell::from(right(have)),
        rarity_cell,
        Cell::from(price(row.lowest_price.as_ref())),
    ])
    .style(style)
}

/// A number column reads best when its digits share a right edge.
fn right(text: String) -> Text<'static> {
    Text::from(text).alignment(Alignment::Right)
}

/// `""` is not an answer: the endpoints leave `format`, `label` and `catno`
/// out of comp rows, and that gap deserves the unknown glyph, not a hole
/// the user stares through.
fn or_unknown(what: &str, width: usize) -> String {
    if what.is_empty() {
        "…".to_string()
    } else {
        cut(what, width)
    }
}

// ---------------------------------------------------------------------------
// Release details.
// ---------------------------------------------------------------------------

fn release_body(frame: &mut Frame, area: Rect, model: &Model) {
    let Some(screen) = model.release.as_ref() else {
        return;
    };
    let detail = &screen.detail;

    let mut lines: Vec<Line> = Vec::with_capacity(8 + detail.tracklist.len());
    let name = Line::from(vec![
        Span::styled(
            detail.release.title.clone(),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            match detail.release.year {
                Some(year) => format!("  ({year})"),
                None => String::new(),
            },
            dim(),
        ),
    ]);
    let artist_line: String = detail
        .release
        .artists
        .iter()
        .map(|artist| artist.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let label_line: String = detail
        .release
        .labels
        .iter()
        .map(|label| {
            if label.catno.is_empty() {
                label.name.clone()
            } else {
                format!("{} [{}]", label.name, label.catno)
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    lines.push(name);
    lines.push(Line::from(Span::styled(artist_line, ink())));
    lines.push(Line::from(Span::styled(label_line, dim())));

    // The community band: keys quiet, numbers loud, the rarity score
    // wearing its color. Everything the table promised, resolved in full.
    if let Some(community) = detail.community {
        let rarity = detail.rarity_display();
        let band = crate::domain::rarity::band_of(rarity.map_or(0.0, f64::from)).as_str();
        let score = rarity.map_or("—".to_string(), |s| s.to_string());
        let mut summary = vec![
            Span::styled("want ", dim()),
            Span::styled(community.want.to_string(), ink()),
            Span::styled(" · have ", dim()),
            Span::styled(community.have.to_string(), ink()),
            Span::styled(" · rarity ", dim()),
            Span::styled(score, rarity_style(rarity).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {band}"), dim()),
        ];
        if let Some((average, count)) = detail.community_rating {
            summary.push(Span::styled(" · rating ", dim()));
            summary.push(Span::styled(format!("{average:.1} ({count})"), ink()));
        }
        if let Some(for_sale) = detail.num_for_sale {
            summary.push(Span::styled(" · ", dim()));
            summary.push(Span::styled(format!("{for_sale} for sale"), ink()));
        }
        lines.push(Line::from(summary));
    }
    lines.push(ask_line(&detail.lowest_price));

    let mut facts: Vec<(&str, String)> = vec![
        ("genres", detail.genres.join(", ")),
        ("styles", detail.styles.join(", ")),
        ("released", detail.released.clone().unwrap_or_default()),
        (
            "country",
            detail.release.country.clone().unwrap_or_default(),
        ),
    ];
    if !detail.identifiers.is_empty() {
        facts.push(("identifiers", detail.identifiers.join(" · ")));
    }
    for (name, value) in facts {
        if value.is_empty() {
            continue;
        }
        lines.push(Line::from(vec![
            Span::styled(format!("{name}: "), dim()),
            Span::styled(cut(&value, 160), ink()),
        ]));
    }

    if !detail.tracklist.is_empty() {
        lines.push(String::new().into());
        lines.push(
            Span::styled(
                "tracklist",
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            )
            .into(),
        );
        for track in &detail.tracklist {
            let position = if track.position.is_empty() {
                "  ".to_string()
            } else {
                track.position.clone()
            };
            let duration = if track.duration.is_empty() {
                String::new()
            } else {
                track.duration.clone()
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{position:>4}"), dim()),
                Span::styled(format!("  {}", track.title), ink()),
                Span::styled(format!("{duration:>8}"), dim()),
            ]));
        }
    }
    // Sales: condition, price, shop — one row per sale, replacing the wire's
    // videos (whose titles were the sheet's only self-promotional content).
    // Loading is said out loud and failure settles — nothing here spins
    // forever after a closed job (the rule that ended the eternal loader).
    lines.push(String::new().into());
    match &screen.sales {
        SalesState::Loading => lines.push(Line::from(Span::styled("sales: loading…", dim()))),
        SalesState::Failed => lines.push(Line::from(Span::styled(
            "sales: could not fetch (status has the reason)",
            dim(),
        ))),
        SalesState::Rows(rows) if rows.is_empty() => {
            lines.push(Line::from(Span::styled("sales: none on record", dim())));
        }
        SalesState::Rows(rows) => {
            lines.push(
                Span::styled(
                    format!("sales · {}", rows.len()),
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                )
                .into(),
            );
            for sale in rows {
                let condition = crate::domain::valuation::Condition::try_from(sale.condition)
                    .map_or("unknown".to_string(), |c| c.as_str().to_string());
                let shop = match &sale.seller {
                    Some(name) => cut(name, 42),
                    None => "…".to_string(),
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{condition:>13}"), dim()),
                    Span::styled(
                        format!("  {:>9}", price(Some(&sale.price))),
                        Style::new().fg(ACCENT),
                    ),
                    Span::styled(format!("  {shop}"), ink()),
                ]));
            }
        }
    }

    // Scroll the Paragraph (it wraps), then draw. The sheet frame keeps
    // the detail in the same visual grammar as the tables.
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((
                u16::try_from(screen.scroll)
                    .ok()
                    .unwrap_or(u16::MAX)
                    .min(area.height.saturating_sub(1)),
                0,
            ))
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::new().fg(DIM))
                    .title(Span::styled(
                        format!(" release {} ", detail.release.id.get()),
                        dim(),
                    )),
            ),
        area,
    );
}

/// The ask, styled like every other number on the digger: quiet label,
/// accented money.
fn ask_line(ask: &Option<Money>) -> Line<'static> {
    Line::from(vec![
        Span::styled("ask ", dim()),
        Span::styled(
            price(ask.as_ref()),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
    ])
}

// ---------------------------------------------------------------------------
// Wantlist.
// ---------------------------------------------------------------------------

fn wantlist_body(frame: &mut Frame, area: Rect, model: &Model) {
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    let header = Line::from(Span::styled(
        format!("your wantlist · {} entries", model.wants.len()),
        dim(),
    ));
    if model.wants.is_empty() {
        frame.render_widget(header, parts[0]);
        frame.render_widget(
            Paragraph::new(Span::styled(
                "empty: open something and press w to start digging",
                dim(),
            )),
            parts[1],
        );
        return;
    }

    let rows: Vec<Row> = model
        .wants
        .iter()
        .enumerate()
        .map(|(index, want)| {
            want_to_row(
                index,
                want,
                model.wantlist.selected == index,
                model.wantlist.confirm == Some(index),
            )
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Min(24),
            Constraint::Length(8),
            Constraint::Length(10),
        ],
    )
    .column_spacing(2)
    .header(wantlist_header());

    let confirm_note = if let Some(row) = model.wantlist.confirm {
        let want_name = cut(
            model
                .wants
                .get(row)
                .map(|want| want.name.as_str())
                .unwrap_or(""),
            40,
        );
        format!("  · delete “{want_name}”? y / n")
    } else {
        String::new()
    };
    let header = Line::from(vec![
        Span::styled(
            format!("your wantlist · {} entries", model.wants.len()),
            dim(),
        ),
        Span::styled(confirm_note, Style::new().fg(NOTICEABLE)),
    ]);

    frame.render_widget(header, parts[0]);
    frame.render_widget(table, parts[1]);
}

fn wantlist_header() -> Row<'static> {
    Row::new(vec![
        Cell::new("name"),
        Cell::new("kind"),
        Cell::new("added"),
    ])
    .style(dim())
}

fn want_to_row(_index: usize, want: &WantEntry, selected: bool, pending: bool) -> Row<'static> {
    // Pending delete wears yellow; selection is Kanagawa's blue band;
    // everything else is the theme's warmed white.
    let style = if pending {
        Style::new().fg(NOTICEABLE).add_modifier(Modifier::BOLD)
    } else if selected {
        Style::new().fg(INK).bg(SELECTED_BG)
    } else {
        ink()
    };
    Row::new(vec![
        Cell::from(cut(&want.name, 48)),
        Cell::from(want.kind.as_str().to_string()),
        Cell::from(date_of(want.added_at)),
    ])
    .style(style)
}

// ---------------------------------------------------------------------------
// Shared *pure* helpers, tested locally.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::market::Currency;

    #[test]
    fn dates_round_trip_the_epoch_and_a_recent_day() {
        assert_eq!(date_of(0), "1970-01-01");
        assert_eq!(date_of(1_700_000_000), "2023-11-14");
        assert_eq!(date_of(86_399), "1970-01-01", "sub-day stamps do not stray");
        assert_eq!(date_of(-1), "1969-12-31", "negative days are honest too");
    }

    #[test]
    fn cut_truncates_never_pads() {
        assert_eq!(cut("hello", 3), "hel");
        assert_eq!(cut("hi", 10), "hi");
        assert_eq!(cut("héllo", 3), "hél", "characters, not bytes");
    }

    #[test]
    fn scroll_window_centers_and_clamps() {
        assert_eq!(scroll_window(0, 100, 10), 0..10, "top of a long sheet");
        assert_eq!(
            scroll_window(50, 100, 10),
            45..55,
            "mid-sheet stays centered"
        );
        assert_eq!(
            scroll_window(99, 100, 10),
            90..100,
            "the last row is reachable"
        );
        // Never past the edges of the data — pagination or sizes misjudged
        // cannot push the window into a panic.
        assert_eq!(scroll_window(2, 5, 100), 0..5, "window clamps to the data");
        assert_eq!(scroll_window(4, 5, 100), 0..5);
        assert_eq!(scroll_window(0, 0, 10), 0..0, "nothing to show");
        // A tiny viewport still shows the selection itself, not a gap.
        assert_eq!(scroll_window(7, 8, 0), 0..0, "no fit, no window");
        assert_eq!(scroll_window(7, 8, 1), 7..8, "one row, the selected one");
    }

    #[test]
    fn prices_show_glyphs_and_exponents() {
        let usd = Money {
            minor_units: 2999,
            currency: Currency::Usd,
        };
        let jpy = Money {
            minor_units: 1200,
            currency: Currency::Jpy,
        };
        assert_eq!(price(Some(&usd)), "$29.99");
        assert_eq!(price(Some(&jpy)), "¥1200", "JPY has no minor units");
        assert_eq!(price(None), "—");
    }

    #[test]
    fn rarity_styles_climb_the_color_scale() {
        assert_eq!(rarity_style(None), dim());
        assert_ne!(rarity_style(Some(10)), rarity_style(Some(60)));
        assert_ne!(rarity_style(Some(60)), rarity_style(Some(90)));
    }
}
