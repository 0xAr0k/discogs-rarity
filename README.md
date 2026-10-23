# diggers-rarity

A Rust CLI that reads a Discogs collection and reports, for each pressing you
own, **how rare it looks** and **what it is worth right now** — plus the
`digger` TUI for digging through the catalog interactively.

Fetching, deduplication, and data validation are done and tested. Rarity
scoring and price estimation are deliberately *not* invented: a rule that
confidently mis-prices a pressing is worse than an honest "unknown", and the
honest unknown is what the report prints today.

## Status

| Working | Not invented yet |
|---|---|
| paging, rate limiting, token handling | rarity scoring |
| collection coherence validation | edition signal detection |
| deduplication by pressing, with copy counts | median and price estimation |
| date/ISO-8601 parsing, currency conversion | condition adjustment |
| text and JSON output | collection totals |
| `Sleeper`/`Clock` ports + fakes | HTTP-level adapter tests |

Every absent number renders as an absence (`unknown`, `—`), never as a zero.

## Usage

Two binaries now ship from this one crate:

- **`diggers-rarity`** — the collection report CLI
- **`digger`** — the Discogs-digging TUI (see `docs/UX.md`)

Requires a current stable Rust toolchain with edition 2024.

```sh
# Public collection, all folders, text summary
cargo run --locked --bin diggers-rarity -- your_username

# JSON, for piping elsewhere
cargo run --locked --bin diggers-rarity -- your_username --format json

# One folder, tighter pricing window
cargo run --locked --bin diggers-rarity -- your_username --folder 1 --market-window-days 90

cargo run --locked --bin diggers-rarity -- --help

# The digger TUI
cargo run --locked --bin digger
```

With only one binary the `--bin` suffix is optional; with two, cargo refuses
to guess, so the examples always name one.

Set `DISCOGS_TOKEN` for private collections or higher rate limits. The env var
is preferred over `--token`: shell history and `ps` output are readable by
other users. A token never grants access to someone else's private collection.

`--market-window-days` controls how recent a sale must be to inform a price. The
default is 365.

## What it prints

Text mode prints a row per pressing: ID, copy count, value, rarity band. Text
mode deliberately prints `unknown` rather than `0` for a value it could not
establish — those are different claims, and a collector will act on the
difference.

JSON mode writes a normalized subset, not a Discogs response backup:

- `username`, `folder_id`, `priced_at`
- `collection_items` (fetched, duplicates included), `unique_pressings`, `total_units`
- `valuations[]` — per pressing: copies, condition, `rarity`, `value` (low/high
  in minor units plus a display string), `confidence`, and the raw `sales[]`
- `total_value` (omitted when nothing could be priced) and `coverage` — the
  fraction of pressings that received a value

Money crosses the wire as integer minor units. Unknowns are `null` or omitted,
never zero. A sum that added zero for unpriced entries would report your
collection as worth the price of the one item it managed to price, so
`total_value` and `coverage` always appear together.

## Behaviour worth knowing

- **Pressing is not album.** The unit of ownership is the *release*: year,
  label, catalogue number, country, format. A Japanese first press and a US
  reissue are not duplicates and are valued separately. Deduplication is keyed
  on release ID alone.
- **A collection must be one consistent snapshot.** Discogs offers no snapshot
  endpoint, so a collection can change mid-read. Every coherence rule in
  `PageSet::assemble` exists to refuse plausible-but-inconsistent data — a lost
  page, changed totals, mismatched IDs — rather than export partial results
  labelled complete.
- **Rates are respected.** 60 req/min authenticated, 25 anonymous. `Retry-After`
  is honoured but never below the pacing interval; a wait beyond five minutes is
  refused rather than slept through.
- **A price outage is not a collection outage.** Sale-history failures degrade
  per pressing to "unknown value". A collection fetch failure aborts, because a
  truncated collection is worse than no report.
- **Tokens are marked sensitive**, so they do not appear in debug output or
  error dumps. `describe_token_source` never echoes the value.

## The digger (second binary)

`cargo run --locked --bin digger` opens a terminal UI for browsing the Discogs
database — artists by id or search, labels, releases — with a persistent
wantlist on `w`. Everything above about architecture holds; the TUI is just a
second `app` in front of the same ports:

```text
  tui::update  ──▶  ports  ◀──  adapters
       │  pure                    DiscogsCatalogSource · JsonWantlist
       └─▶ tui::runtime (one worker thread owns the ports, strictly
            sequential: the sequence is the rate limiter)
```

- `docs/UX.md` — screens, keys, what each column claims
- `docs/DECISIONS.md` — D-01…D-11 decided, Q-01…Q-06 open
- `docs/DISCOGS.md` — the catalog endpoints and their quirks

No collection required; anonymous browsing works, search wants `DISCOGS_TOKEN`.

## Architecture

```text
  ui  ──▶  app  ──▶  ports  ◀──  adapters
                └──▶  domain
```

`domain` is pure — no I/O, no clock, no `unwrap`. `ports` are traits written in
domain vocabulary. `adapters` are the dirty implementations. `app` orchestrates.
`ui` renders. `main.rs` wires and exits.

The property is enforced by a test: `tests/integration.rs` implements
`CollectionSource` from outside the crate, so a trait signature that picks up a
private type breaks the build.

Full explanation, including what each seam costs: **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)**

## Learning notes

[docs/RUST.md](docs/RUST.md) — twelve sections on the Rust this crate actually
uses, written as "why it appears here" rather than as a language reference:
newtypes over integers, two error types instead of `anyhow`, `Option` versus
sentinel values, ports and adapters, functional core, iterator composition,
`?`/`map_err`, trait objects versus generics, borrow lifetimes, and when to
override clippy with a written reason.

## Development

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Tests need no Discogs account, token, or network: adapters are tested through
`adapters::testing` fakes. The one gap is HTTP-level adapter testing, which
`RecordingSleeper` was built to make feasible.

`cargo test` should exit zero. The scoring and pricing stories are absent code,
not failing tests — a `todo!()` on a production path (as the old estimator
scaffolding had) is a panic waiting for real data, so it is gone.