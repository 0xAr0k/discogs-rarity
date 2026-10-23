# Decisions and open questions

One id per decision: **D-nn** decided, **Q-nn** open. These are the calls
that shaped the digger, in the order a newcomer meets them, each with the
alternative that lost.

## Decided

### D-01 — A second binary, not a mode
`digger` lives in `src/bin/` and shares the library with the collection CLI.
A `--tui` flag would fork the CLI's attention (batch report vs conversation)
while fusing its failure modes. Two binaries also mean the TUI deps do not
weigh on the `diggers-rarity` default binary link. *Lost alternative:*
subcommands (`diggers dig`) — same fusion, fewer wins.

### D-02 — The Elm loop, states and effects
`model` (pure state) ← `Msg` ← runtime; `update(model, msg) → Vec<Effect>`
pure; `view(model)` reads only. All I/O — HTTP, file, clock — happens in a
worker thread behind `Effect`s. *Lost alternative:* a component framework on
top of ratatui. That buys scale the loop is nowhere near needing, and costs a
second vocabulary for behavior a single `match` already covers.

### D-03 — Two threads, no async runtime
A raw `std::thread` owns the ports and runs effects strictly in order. The
sequence is the rate limiter. async/await would bring tokio into the deps
tree for a job that is one request at a time. `ratatui` itself stays
sync-friendly (`crossterm` blocking `poll`). *Lost alternative:* async —
worth it only when the app itself parallelizes.

### D-04 — Search results are their own type
`SearchPage { page, pages, hits: Vec<SearchResult> }`, not a `CatalogPage`.
Search mixes artists, labels and releases; coercing that into release rows
would lie about half the hits. This is the second port signature
(`CatalogSource::search`) — a trait that returns the type the screen needs,
not the type that already existed.

### D-05 — The want-pressure score lives in `domain::browse`
`want_pressure = want / (want + have)`, a 0..=1 f64, displayed ×100 rounded.
It shares no code with the exercise's `rarity::classify` and makes no claim
to be a full signal — it is the one *objective* pairing the wire already
carries and answers this digger's question: "how many other people want
this compared to owning it".

### D-06 — Wantlist persists to a JSON file, not sqlite
Dozens of lines, written on a `w`, read at boot. A temp-file + rename is
atomic on every filesystem the tool touches; the file opens in `less` for
reading and in an editor of choice. No new dependency. The store re-reads per call and rewrites whole — no
interior state, cache expiry impossible to get wrong. *Lost alternative:*
sqlite — indexes nobody asked for.

### D-07 — Vinyl inclusion rule
Rows display when the format string contains `vinyl` (case-insensitive).
The collection exercise's `Format::is_physical` is a *different* rule for a
different question ("is this a physical copy at all") and is left alone —
the exercise stays an exercise. The digger's rule is a product decision and
lives in the domain layer as `is_vinyl_format`, documented, tested, and easy
to broaden (labels often file "Vinyl, LP" plus "Cassette" — those show; a
CD-only pressing never does).

### D-08 — The session cache is in the adapter
`DiscogsCatalogSource` holds `Mutex<HashMap<ReleaseId, Arc<ReleaseDetail>>>`.
The port hands one owned clone across the boundary per hit — interior
`Arc` sharing would leak a shared-mutable temptation into `update`. Disk
caching waits until there is a *session boundary* worth persisting across.

### D-09 — Anonymous search refuses up front
The adapter checks the token before any HTTP, and its message names the
variable (`DISCOGS_TOKEN`). Refusing at the door with the fix in the
message beats a 401 dressed as an empty result.

### D-10 — Enrichment is visible-window paced
Enrich only rows near the selection (a `visible_range` around it), max 4 in
flight, each row claimed by an `inflight` set before its effect leaves.
Masters skipped (Q-01). *Lost alternative:* enrich the whole page on load —
twenty wasted requests each time someone peaks at page 12 and leaves.

### D-11 — Staleness is one token
Exactly one pending release-open (`Model::pending_open`); a `DetailLoaded`
reply for anything else is dropped. The worker is sequential, but the user
is not: open row 3, Esc, open row 7 — the row-3 reply has no claim on the
UI any more. Enrichment needs no token (it fills the row whose id it
fetched; a stale enrich finds no row and is a no-op).

## Open questions (deliberately not decided)

### Q-01 — Can a master become enrichable?
Masters have no detail endpoint today. Options: fetch
`GET /masters/{id}` and synthesize a detail (new wire type, new cache
class, real data); or keep them inert markers (current). Inertness is
honest; `GET /masters` would make them second-class releases. Decide when
the empty cells annoy someone real.

### Q-02 — Should unresolvable master search hits ever be shown?
A `master` search hit without a `main_release` is dropped today. Alternatives:
show it as text (present but un-openable — odd) or call `/masters/{id}` per
hit (one request per row — the exact luxury D-10 refuses). Stays dropped
until real hit-rate data says otherwise.

### Q-03 — What should `w` mean on a *label*?
Today it wants the label in its entirety — the artifact is a label-sized
promise, which the wantlist row displays as such. The alternative: `w`
wants all `n` vinyl rows (n requests, an n-keys action for the user).
"Wanting a label" is honest about being a shopping criterion.

### Q-04 — Should paging prefetch the next page?
No today (quota is spent on what was asked). With a prefetch, the seam is
the workers' effect queue, but the risk is a user who reads four rows of
page 1 and pays for page 2 without reading a single row of it. Decide with
that user in hand.

### Q-05 — Configurable rarity-band cutoffs
Band thresholds (<50, <75 green/yellow/red) are hardcoded in the view
(style) and semantics in `domain::rarity::band_of`. Should `digger` grow a
config file with the cutoffs? Not yet — one operator, one set of eyes on
colors that are opinions rather than measurements. The extraction point
(`band_of`) already exists, which is the entire cost of changing track.

### Q-06 — Cancel-stale-enrichment on page turn?
Effects already in flight are let to complete even after rows change. The
workers' queue is LIFO; a cancel channel adds a second ordering concept for
a cap of four requests. Leave until measured waste exceeds noise.

### D-12 — Master rows resolve to their `main_release` (revises part of Q-01)
The live inspect of `a:3057` proved the artist page would be blank: the
compact artist rows for `master` entries publish **no** `format`, so the
vinyl filter judged "unknown" and dropped every master. Rather than special-
case the math, a master's openable id is now its `main_release` — the same
translation the search page already applies (Q-02's rule, entity pages
included). A master that names no main release is dropped silently, row
count is the truth, and `is_master` stays on the row as a display claim
(`…` in its format/label/catno cells: the wire does not say). Q-01's
"masters are inert" stops being true; what remains open there is only
whether `GET /masters/{id}` (or one main-release detail) is worth an extra
request — until then the user opens the main release like any other row.

### D-13 — Row community stats come from the wire, and enrichment is a claim
The compact rows carry `stats.community.{in_wantlist, in_collection}`; the
adapter maps them to `CommunityStats`, so a fresh page shows want/rarity on
paint — no fetch. The fetch still happens (it brings the price), but *"this
row has been fetched"* is now a field (`CatalogRow::enriched`), because a
row whose detail has neither stats nor price otherwise looks unfilled
forever and re-enters the enrich channel every tick. Along the way the
"four in flight" cap was fixed to count claims inside one tick as well as
outside (`busy` is only settled when `update` returns), and `busy` itself
now rises per network effect — before, only the boot fetch ever
incremented it, so the cap saw a calm channel that was actually running.

### Q-07 — Distinguish 'adders' from 'fetched' in the want column?
The wire's `in_wantlist` and the detail fetch's `community.want` are not
the same number forever; rows refetch and can swap an older count for a
newer one silently. Probably fine (both are community counts of the same
release), but if a row ever looks like it *changed* under idle reading,
this Q decides whether enrichment marks its rows as "verified".

### D-14 — Worker classes: user work preempts enrichment
The worker stays the rate limiter — one HTTP call at a time, order kept
within each class — but the queue splits by class. User-driven effects
(opens, profiles, rows, searches) drain before enrichment effects (row and
hit stats), so a search screen's hit backlog can never strand an artist
open behind four 2.5 s detail fetches it quietly queued while the user was
still looking elsewhere. Two boundaries move together with this: only the
*focused* screen schedules enrichment on tick (a background search's hits
stop arming themselves behind an entity screen), and the worker classifies
by `Effect::class`. A call already in flight cannot be preempted — worst
case, the user waits for one request, not a backlog. Lost alternative:
cancelling in-flight fetches — that is another channel and another
ordering concept for a queue that is at most four deep.

### D-15 — Gap pacing, concurrent enrichment, session fills
Three changes, one goal: pages paint fast, the wire stays honest.
**Pacing became a gap, not a charge** — no request starts until `interval`
has passed since the previous request *started* (shared across all
callers, mutex held across the wait), so the first request after daylight
goes out instantly instead of every call paying a fixed 1–2.5 s tax.
**Enrichment fetches run on detached threads** (capped by the scheduler's
four-in-the-air rule at the model, ordered only by the shared pacing gate
at the wire), which supersedes the strict-sequential element of D-14 for
the enrichment class: user work still runs strictly in order on the
worker. Difference perceived, not parallelism raced: replies fill the row
whose id they carry, so out-of-order completion is safe by construction.
**The model remembers fetched details for the session** (`DetailFill`,
keyed by release id), and dresses rows/hits as pages (re)load, so a page
switch no longer greys out prices it already knows — the adapter's HTTP
cache made the refetch free, the dress makes it invisible. Session-scoped
like that cache: prices age across runs, so nothing survives them.

### D-16 — Masters enrich now, sheet sorting is page-local and sticky
Two follow-ons from the last pass. **Master rows were un-skipped from
enrichment**: their `id` became the *main release's* id when the adapter
translated opens (the Q-01 resolution), so the "a master has no release
detail" premise of the old skip is simply stale — the same one detail
fetch that counts the crowd now also closes the format/label/catno gaps
the compact rows (and especially masters) leave blank. The session fill
cache carries those fields so a page reload stays dressed.
**`s` sorts the loaded page** — entity: sheet → rare → price; search:
sheet → rare. Page-local on purpose: the wire hands one page at a time,
and "rarest overall" without fetching everything is a claim the UI cannot
keep. Unknown values sort *last in sheet order* — unknown is not zero, so
it must not be treated as the least rare or cheapest. Sorting is sticky
(the wire order is unrecoverable without refetching, so the wheel never
pretends to restore it), a fresh page sorts on arrival, and the selection
rides its row through the reorder.
Alongside: the digger's look moved from greyscale to a Dune palette —
baked-earth ground, ash-orange quiet text (never grey), orange-ground/
black-glyph selection — presentation only, all of it in `view`.

### D-17 — the look reverted: Kanagawa, transparent ground
The Dune experiment and the blur.io token lift are both rescinded. The
digger now speaks [Kanagawa](https://github.com/rebelot/kanagawa):
`springBlue` accent, `samuraiRed` errors, `sumiInk3` quiet text,
`oldWhite` body ink, rarity as green→amber→red. The ground rule worth
keeping: **the app paints no background** — every style sets a foreground
only, so a transparent-capable terminal shows through; the one exception
is the selected row's `waveBlue2` band, which exists so the selection is
point-at-able. Presentation only, all of it in `view`.

### D-18 — bursts not gaps: token-bucket pacing, eight in the air, vinyl search, direct sort keys
Three operator requests from live testing, one decision each:

- **Pacing is a token bucket, not a minimum gap.** D-15's gap rule made
  every enrichment wait its turn one-by-one; the wire's rule is a
  *per-minute budget* (60/min with a token, 25/min without — now
  `Pacing::cap`). The adapter starts the bucket full (`BURST = 8`),
  spends one token per request, refills at cap/minute, so opening a
  screen fires a whole wave at once and the sustained rate still never
  exceeds the documented window. `interval` keeps its job as the floor
  on any server-sent `Retry-After`. The enrichment scheduler's
  in-flight cap matches the burst (4 → 8, `MAX_IN_FLIGHT`), so the
  pacing gate is the only limiter that feeds back.
- **Search says vinyl only.** The entity tables already filter at the
  boundary by D-07's rule (format string contains `vinyl`); the search
  path now does the same to release hits — CD, cassette, files go.
  Hits that publish no `format` array stay: unknown is not "CD". This
  makes page sizes vary from the wire's counts; that variance is the
  honest price of the filter.
  The rule's second half is **conviction on arrival**: the same detail
  fetch that fills a hit's `…` cells can convict it. When the fill names
  the formats and none of them says `Vinyl`, the row leaves the table
  where it stood — selection pinned to its row — instead of staying as a
  CD listed under a vinyl-only banner. A detail that says nothing again
  does not convict; unknown is still not "CD".
- **Sort has direct keys.** `1` sorts rare, `2` sorts price (entity
  only), `0` returns to sheet order — by refetching the current page,
  because the table keeps no record of the wire's order and faking the
  claim would be a lie. The `s` sticky wheel stays for repeated presses.
  Page-local rules, unknown-sort-last, and selection pinning (D-16) are
  unchanged.

### D-19 — every counted job must close, failure included
An operator's search looked like "load forever". The mechanics: `busy`
(the spinner's truth) was incremented for every network effect that left
`update`, but only *successes* closed the count. A failed request sent
[`Msg::Failed`], which was not terminal — the job was counted out and
never back in, so one failed request left the spinner spinning for the
rest of the session. The boot's wantlist load leaked the same way: the
runtime counts it by hand, and its success reply (never `terminal`)
credited nothing.

The fix is a contract, not a patch: **every counted job closes exactly
once, success or failure**. `Msg::Failed` carries a `job` flag — the
mirror of the effect that failed, so network failures close their count
and want-file failures (never counted) never un-count someone else's —
and `WantlistLoaded` closes the boot's hand-counted load. The spinner
can now only spin while something is actually in the air.

## D-20 — Exercises retired, absent code over failing gates (2026-10-23)

The operator asked to "remove failed tests … make it clean". The seven
learning exercises (29 failing tests in `domain::rarity`, `domain::market`,
`domain::valuation`, `app::signals`) and the `todo!()` functions they gated
are removed. This was not only test hygiene: `snapshots_for` — production
code on the CLI report path — called the estimator stub, so a collection
with real sales data would have panicked the report. The same for
`app::signals_for` reaching into `rarity::edition_signal`.

The rule the replacement obeys: *absent code over fake code*. Where an
estimator is missing, the missing pieces do not exist and the types the
renderers already speak (`PriceEstimate`, `Valuation::unknown`, absent
totals) carry honest unknowns downstream. Judgement calls the exercises
held open (signed sales, confidence, currency totals, edition scoring)
remain open as plain comments beside the code that will need the answers.
`docs/EXERCISES.md` records the inventory.
