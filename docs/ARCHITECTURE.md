# Architecture

## The rule

Dependencies point one way. A change that makes an arrow point the other way
is a bug, even if it compiles.

```text
  ui  ──▶  app  ──▶  ports  ◀──  adapters
                └──▶  domain
```

| Layer | Owns | May not |
|---|---|---|
| `domain` | What a pressing is; what makes a collection coherent; what something is worth | touch I/O, mutate arguments, `unwrap` external input |
| `ports` | The traits: `CollectionSource`, `SaleSource`, `Sleeper`, `Clock` | know any concrete implementation |
| `adapters` | HTTP, JSON, real clocks, real sleeping | decide what data *means* |
| `app` | Which call to make, in what order | hold a rule about music |
| `ui` | Text and JSON rendering | contain business logic |

`main.rs` parses arguments, wires adapters, prints. That is all.

## Checking the rule

Three ways, weakest to strongest:

1. **Reading the imports.** `grep -rn "use crate::adapters" src/domain/` should
   return nothing.
2. **Unit tests.** Domain tests build domain values. If they needed a JSON
   fixture, the layer would be leaking.
3. **`tests/integration.rs`.** The file defines a `CannedSource` implementing
   `CollectionSource` from outside the crate. If a trait signature ever picks up
   a private type, that file stops compiling.

That last one is why architecture survived the refactor: it is enforced by the
build, not by discipline.

## Why this shape for *this* app

A crate organizer looks like an API client with a bit of math. It is not — the
hard parts are all decisions, and the API is incidental:

- **Is this collection one coherent snapshot?** Discogs has no snapshot
  endpoint. Collections change mid-read. Every rule in `PageSet::assemble`
  exists to refuse plausible-but-inconsistent data, because a partial export
  presented as complete is worse than a failure.
- **What is this pressing worth?** Not one number. A market estimate from
  filtered sale history, adjusted for *this* copy's condition, reported with a
  confidence figure and a coverage ratio.
- **How rare is it?** Genuinely a judgement call. So `rarity` names its inputs
  (`RaritySignal`) and refuses to pretend the combination is objective —
  `classify` is left for you to write, with tests that pin the properties
  regardless of the arithmetic you choose.

Only the first bullet needs the network. Two of three do not, which is exactly
why they belong in a pure layer you can test in microseconds.

## Request flow

```text
cli::Args ──▶ app::Request          validation happens once, at the edge
                                     everything below works with domain types only
               │
               ├─▶ CollectionSource::fetch_pages       (adapter: HTTP, pacing)
               │      └──▶ domain::PageSet::assemble  (pure: is it coherent?)
               │             └──▶ domain::Collection
               │
               ├─▶ SaleSource::sales_for  × per pressing (errors degrade to unknown)
               │      └──▶ domain::market::snapshots_for  (pure)
               │
               └─▶ domain::valuation                    (pure)
                      └──▶ ui::Format::render
```

Two deliberate choices in that diagram:

**Transport failure and coherence failure stay separate.** `load_collection`
returns `Error` for both, but only the second carries a
`Option<DomainError>`. They need different responses: one is retryable, the
other needs a rerun.

**A price failure degrades, a collection failure aborts.** Losing sale history
for one pressing should not sink a 4,000-record report. `gather_sales` swallows
per-pressing errors on purpose; §"availability trade-offs" in
`src/app.rs` says so, because the opposite choice would be defensible for a shop.

## Where the seams are, and what they cost

| Seam | Bought | Cost |
|---|---|---|
| `CollectionSource` / `SaleSource` | test without network; a cache is a wrapper | two impls for one client |
| `Sleeper` | rate-limit tests run instantly | one extra trait |
| `Clock` | pricing windows are deterministic functions | callers pass `now` |
| No `Deserialize` on domain types | Discogs schema churn stays in one file | hand-written conversion |
| No `anyhow` in `domain` | failures are matchable, testable | domain returns `DomainError` |

With three ports the trade is clearly worth it. At thirty it would not be —
a large port surface is its own kind of coupling. Worth knowing before adding
more.

## Test layout

| Location | Scope | Uses |
|---|---|---|
| `#[cfg(test)]` in a module | the rule in that module | hand-built domain values |
| `tests/integration.rs` | the public API, wiring, output contract | fakes from `adapters::testing` |
| *(missing)* | HTTP behaviour of `DiscogsCollectionSource` | local server + `RecordingSleeper` |

Pure rules are tested where they live; wiring and output contracts are tested
from outside. See `tests/README.md` for why the JSON fixture is gone.

29 tests currently fail with `not yet implemented`. That is the exercise list —
`docs/EXERCISES.md` maps them one-to-one.

## Where to add things

| Want to… | Add it in |
|---|---|
| change how a price is estimated | `domain::market` |
| weight rarity differently | `domain::rarity::classify` |
| fetch from somewhere other than Discogs | a new `adapters::` module; no other layer changes |
| add a `--sort` flag | `ui`, plus an `Ord` on a view type |
| cache collection data | a `CollectionSource` that wraps another one |
| add a currency | `market::Currency` — its exponent and code live there together |
| add a field to the output | `ui::json` view types, not the domain structs |