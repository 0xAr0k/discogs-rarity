# The digger's UX

`digger` is a terminal UI for digging through the Discogs database — artists,
labels, releases — and keeping a wantlist. It is a second binary, not a flag on
the collection CLI, because the two jobs share adapters but not attention: a
report is a batch job, a dig is a conversation.

Run it:

```sh
DISCOGS_TOKEN=... cargo run --bin digger
```

## The loop shape

Everything is `state → message → state + effects`: keys and network replies
arrive as messages, `update` computes new state and returns effects (fetch
this, write that), a worker thread runs them in order, and the screen redraws
once per frame. No async, no locks you can see. The trim below is strictly
that.

## Screens

Stack, home at the floor:

| Screen | Shows | Reach |
|---|---|---|
| **Home** | one prompt, two help columns | the entry point |
| **Search** | hits for a name query, as a bordered table (`title year format label cat# rare`) | `home → type a name → ⏎` |
| **Entity** | artist or label profile + a release table | `home → an id`, or `⏎` on a search hit — masters open via their main release |
| **Release** | full detail of one pressing, with its sale rows (`condition price shop`) | `⏎` on a table row or a release hit |
| **Wantlist** | everything you promised to dig | `F2` from anywhere |

## Keys

Every screen: `F1` home, `F2` wantlist, `Ctrl+C` quit.

| Screen | Keys |
|---|---|
| home | type; `←`/`→` cycle the kind filter; `⏎` go; `Esc` clears the prompt — over an **empty** prompt it quits |
| search | `↑`/`↓` move; `←`/`→` page; `1` sort rare; `0` sheet order (refetches); `s` cycle the sort wheel; `⏎` open the hit; `w` want it; `Esc` back |
| entity | `↑`/`↓` move; `←`/`→` page; `Home`/`End` jump; `1` sort rare; `2` sort price; `0` sheet order (refetches); `s` cycle the sort wheel; `⏎` open the row; `w` want the artist/label; `Esc` back |
| release | `↑`/`↓`/`PgUp`/`PgDn`/`Home` scroll; `w` want the release; `Esc` back |
| wantlist | `↑`/`↓` move; `⏎` open; `d` then `y`/`n` confirm a removal; `Esc` back |

## Loading feels like loading

The screen always answers three questions about fetches — *is anything in
the air, what is it, and how much*:

- the breadcrumb runs a spinner plus a word when work is out there
  (`⠴ searching…`, `⠴ fetching releases…`, `⠴ opening…` on a release open);
- a body that is *empty because it is waiting* shows a centered
  `spinner + what is loading` badge (search, entity, wantlist) — never a
  blank region;
- on the entity screen, rows near the selection fetch their detail in
  place; while that happens the status band counts it
  (`⟳ 4 row(s) fetching their detail…`) and the row's `…` cells fill in
  waves of eight. The same fetch is never repeated — `docs/DECISIONS.md`,
  D-10 and D-13.
- only the focused screen schedules enrichment, and the worker answers
  user actions first: an open (artist, label, release) never waits behind
  a hit-stat backlog (`docs/DECISIONS.md`, D-14). Detail fetches run on
  short-lived threads behind one token-bucket gate: a burst up to eight
  at once, then refills at the wire's documented per-minute rate, so the
  budget holds (`docs/DECISIONS.md`, D-15 and D-18).

## The prompt grammar

One prompt, three shapes:

```text
108713      an id, interpreted by the current filter (default artist)
a:108713    an artist by id — the prefix overrides the filter
r:249504    a release by id
l:1         a label by id
mobb deep   anything else is a database search
```

## Wantlist

`w` on a search hit, an entity, or a release detail — or the row already
being on the list, which the status line says out loud. On the wantlist
screen, `d` arms a delete and `y` confirms: no destructive step happens on
one key press.

The list persists to a JSON file
(`$XDG_DATA_HOME/diggers-rarity/wantlist.json`, else
`$HOME/.local/share/diggers-rarity/wantlist.json`). It is readable with
`less` by design — see `docs/DECISIONS.md`, D-06.

## What the table means

| Column | Claim |
|---|---|
| `year` | the pressing year the listing reports; `—` is unknown-to-Discogs, not empty |
| `format`/`label`/`cat#` | what the listing row publishes; `…` means the wire does not say — and enrichment closes those gaps (master rows say nothing compact, but their detail does, `docs/DECISIONS.md` D-16) |
| `want` | raw community want count |
| `have` | raw community have (in-collection) count; `…` means *not fetched yet*, not zero |
| `rare` | want-pressure on a 0–100 scale; `…` means *not fetched yet*, not *unknown* |
| `price` | lowest current ask; `—` when there is none |

On the search screen the columns are `title year format label cat# rare`,
and the hits are **vinyl only** — a release hit whose formats say CD,
cassette, or files is dropped at the boundary, exactly like the entity
tables (`docs/DECISIONS.md`, D-18). A hit that publishes *no* formats
stays: unknown is not "CD". That unknown is the rule's open edge, and
the detail fetch closes it: the same fill that writes the `…` cells
**convicts on arrival** — a row whose detail names no vinyl leaves the
table where it stood, and the selection stays pinned to its row.
and `rare` claims what its position demands: release hits say `…` while
their detail is in the air — the same enrichment policy as the entity
table — and `—` once it came back community-less. Artist and label hits
say `—` for good: no endpoint publishes want/have counts for them.

`…` and `—` are different claims on purpose, the same lesson the collection
report teaches in text mode: unknown must never masquerade as zero.

## Sorting, honestly (`1`, `2`, `0`, and the `s` wheel)

`1` on an entity or search table sorts **rare** right away, `2` sorts
**price** (entity only — search hits publish no price), and the `s` wheel
cycles the same sorts sticky so repeated presses stay put. Sorting
reorders the **loaded page** — the wire hands the digger one page at a
time, so "rarest overall" would be a lie unless every page had been
fetched first (the block title carries the order, e.g. `releases · rare ·
1/3`):

- `0` restores the **sheet order** honestly: sorting cannot un-sort —
  the table keeps no record of the wire's order — so `0` refetches the
  current page, one request, and the fresh rows start over at `sheet`.
- unknown values (`…`) are not zero and do not sort as one: they keep
  their sheet order at the bottom until their detail lands.
- a fresh page also sorts on arrival (`←`/`→` keeps the order), and the
  selection stays pinned to the row it was on.

## The release sheet, and its sales

Opening a release fetches its sale rows alongside the sheet (one request,
session-cached in the adapter, so a reopen is free). They replace the
wire's video titles as the tail of the sheet: a bold `sales · N` header,
then one row per sale — condition (Discogs' label), price (ASCII glyph,
the same shape the `ask` line uses), shop. Unknown is not zero: a seller
the wire did not name prints `…`, an unreadable row is dropped (never a
zero-price line), an empty history says `none on record`, and a failed
fetch says `could not fetch` with the reason on the status line — the
section never spins forever.

## The look — Kanagawa, and a transparent ground

The palette is [Kanagawa](https://github.com/rebelot/kanagawa):
`springBlue` accents, `samuraiRed` errors, quiet text in `sumiInk3` grey,
body ink in `oldWhite` — no page-white anywhere. The ground is **the
terminal's own**: the digger paints no background except the selected
row's `waveBlue2` band, so a transparent-capable terminal shows through
exactly where the sheet does. Rarity stays a temperature the reader
learns once: green (common) → amber (noticeable) → red + bold (critical).
What the app does **not** control is the font size — that is the
terminal's choice; the sheet keeps its legibility by spacing and weight.

## Enrichment, briefly

Listing endpoints and search carry no community counts. The moment rows
or hits land, the digger fetches details for the ones on screen a few at
Listing endpoints and search carry no community counts. The moment rows
or hits land, the digger fetches details for the ones on screen a few at
a time, capped at eight in flight, filling each row in place — on the
entity screen and the search screen alike. Masters are never enriched —
they have no release detail at all (`docs/DECISIONS.md`, Q-01).

## What you will NOT find

- No mouse. A digger's hands are on chords, not the rodent.
- No async runtime (`docs/DECISIONS.md`, D-03).
- No background prefetch of pages you have not turned to — quota is for the
  pages you asked for (`docs/DECISIONS.md`, Q-04).
- No editing of anything outside your own wantlist file.
