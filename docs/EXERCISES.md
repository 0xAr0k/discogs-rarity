# Exercises

Six exercises, ordered so each one is usable before the next. Every one is
gated by a test that already exists and currently fails with
`not yet implemented`, so you always know when you are done:

```sh
cargo test --lib domain::rarity     # exercise 1
cargo test --lib domain::market     # exercises 2
cargo test --lib domain::valuation  # exercise 3
```

If you want the whole suite green right now, that is the point — the 29
failing tests *are* the to-do list. Nothing else in the crate depends on them.

## How to approach each one

The doc comment above each `todo!` explains the constraints. What follows is the
part the doc comment cannot know: the questions worth sitting with, and the
mistakes that feel productive right up until they are not.

Write the tests first if you can. Every exercise already has tests, so start by
reading them and asking *what would make them fail for the wrong reason*. Then
implement. If you want a harder version, delete a test, write your own, and see
whether it catches a bug.

---

## Exercise 1 — Rarity (`src/domain/rarity.rs`)

**1a. `edition_signal`** — derive an edition-signal magnitude from format
descriptions. Pure, local, no network.

The interesting part is *not* the keyword table. It is deciding what to do
when the evidence disagrees. A release described as both `Limited Edition` and
`Reissue` is a reissue of something limited — not a limited reissue. Take the
max? The min? Does the order of the format list matter? Whatever you choose,
the choice encodes a belief about collectors, and it deserves a comment.

Two traps:

- **Substring false positives.** `"Not First Pressing"` contains `"First
  Pressing"`. Splitting into tokens and comparing case-insensitively is more
  honest than `contains`. Discogs' descriptions are free text written by
  volunteers, so assume the grammar is not consistent.
- **The empty case.** `[]` and `["LP, Album"]` are the same thing: no edition
  information. Both must be `None`, and conflating them with `Some(0.0)` is
  how a common record ends up looking like a rarity signal of zero.

*Why `max` and not `sum`:* a limited vinyl bonus alongside a plain CD is one
pressing with an interesting format. Summing can exceed 1.0 and every clamping
mechanism you add afterwards is a bug waiting to be found.

**1b. `classify`** — combine signals into one defensible score. This is the
judgement call the whole app rests on.

Start with a weighted mean over the *measured* signals only, and then consider
whether a mean is the right shape at all:

- A **geometric mean** punishes imbalance: 1.0 scarcity with 0.0 demand scores
  lower than 0.7/0.7. For "is this worth hunting", that may be right.
- A **minimum** (bottleneck) rule says a pressing is only as rare as its least
  rare attribute. Very conservative, hard to argue with.
- A **hand-tuned table** is not cheating if you write down why each cell holds.

The test `classify_does_not_treat_missing_signals_as_zero` is the important
one. If your score for "scarcity 1.0, everything unknown" equals your score for
"scarcity 1.0, demand 0.0, edition 0.0, discontinued 0.0", then unknown signals
are silently acting as evidence of commonness. Weight by what you measured.

*Harder:* add a `RarityBand::Unknown` and make `band()` return it when
`measured_count() < 2`. Then make the renderer say so.

**1c. `signals_for`** (`src/app.rs`) — wire in real scarcity data from
Discogs' `community.have` / `community.want`.

This is the one that needs the most thought, because it is where honest
uncertainty meets a tempting shortcut. `have` counts are wildly skewed: most
records have a few hundred owners, a handful have tens of thousands. Linear
normalization makes almost everything score near zero.

Consider a **log scale**: `scarcity = 1 - log(1+have) / log(1+have_max)`. Now
compare three choices for `have_max`:

- a fixed constant — simple, but goes stale and has no domain meaning;
- the max across the user's own collection — adaptive, but the same record
  scores differently for different users, which quietly breaks comparison;
- a documented Discogs-wide percentile — meaningful, but needs a source and a
  date, and the date is data you now have to keep.

Write down which you chose and why. That sentence is the deliverable; the
arithmetic is easy.

---

## Exercise 2 — Market (`src/domain/market.rs`)

**2a. `Sale::is_outlier`** — decide which sales are not evidence about the
plain pressing.

Signed copies are the hard case and there are two defensible answers:

- **Discard them.** Correct if you are valuing an unsigned copy.
- **Keep them separately.** Also correct — a signed sale is strong evidence
  about *demand*, just not about this copy.

If you choose the second, the type wants to change: instead of `bool`, maybe
`enum SaleClass { Plain, Signed, Promo }`, and the estimator can report both a
plain median and a signed premium. That is more code and a better answer.

For IQR-based price outliers, watch the small-sample case. Quartiles of four
data points are noise. Only apply the rule above some minimum count, and say
what that count is.

**2b. `confidence`** — how much to trust an estimate.

The tempting answer, `min(1.0, n / 20)`, is fine and worth starting from. The
better answer asks what confidence *means* here. If it means "how likely a user
is to complain", dispersion matters more than count: twenty sales spanning €5 to
€200 tell you less than five sales all within €20. Multiplying a size term by
an agreement term captures both.

*Careful:* the test asserts confidence never falls as evidence grows. Any
dispersion-based rule can violate that. Decide whether to relax the test (with
a comment explaining why) or to make confidence monotonic by construction.

**2c. `median`** — the mechanical one. Sort a copy, index the middle.

Three things beyond the mechanics:

- **`Option` for empty input.** A function that panics on a valid input is a
  bug, not a precondition. `Vec::new()` is a perfectly normal thing to hand a
  median.
- **Do not mutate the argument.** `samples.sort()` on a `&[i64]` needs
  `to_vec()` first. The test `median_does_not_mutate_its_input` checks this,
  and it is worth knowing *why*: a function that reorders its input changes the
  caller's data, and the next caller will be surprised.
- **Rounding on even lengths.** Average the two central values. Decide the
  rounding direction and write it in a comment — `div_euclid` and `/` disagree
  for negatives, and a median function with a sign-dependent half-cent bias is a
  genuinely nasty thing to debug.

*Harder:* implement a trimmed mean and compare against the median on the same
data. For five samples they differ meaningfully. Which would you rather show a
user, and why?

**2d. `estimate_from_sales`** — the pipeline: filter, age-cut, median,
confidence.

Order matters. Filter before the age cut, or age-cut before filtering — the
results differ when an outlier is old. Either is defensible; be consistent and
say which you chose.

`recent_window_days` is a parameter, not a constant. That is deliberate: a shop
wanting to price inventory wants a month, an archivist wants a decade. If you
hard-code it, the function stops being reusable and stops being testable.

---

## Exercise 3 — Valuation (`src/domain/valuation.rs`)

**3a. `apply_condition`** — adjust a market median for *your* copy's grade.

The table in the doc comment is a starting point, not an answer. Where do those
multipliers come from? For a real project you would want observed
condition-vs-price data, which means the sales you are already fetching — you
have `condition: u8` on every `Sale`. That is a much better source than
anyone's intuition, and it is sitting unused.

The subtler design point is **how confidence should modulate the adjustment**:

```text
adjusted = median * (1 + confidence * (multiplier - 1))
```

With one sale, `confidence` is near zero, so the adjustment nearly vanishes —
which is right, because a condition correction off a single data point is
mostly fiction. With twenty consistent sales, the correction applies fully.

*Trap:* never adjust a value you do not have. `None` in, `None` out. And never
let a multiplier drive a price below zero; a VG pressing of a €0.80 single is
still worth something.

**3b. `CollectionValuation::total`** — the sum, and the honesty problem.

The test `total_ignores_unvalued_pressings_rather_than_counting_them_as_zero`
is the point of this exercise. "Your collection is worth €40" when €40 is the
sum of the only two pressings you could price and 400 others are unknown is a
lie by aggregation. The total and the coverage figure must always appear
together.

**The currency problem.** You cannot sum EUR and USD. Two honest options:

- convert with an explicit rate table passed as an argument — useful, but then
  the rate's source and *date* belong in the type, because a stale rate is a
  wrong number that looks right;
- total only when every valuation shares a currency, else report per-currency
  subtotals — less convenient, needs no external data, cannot be wrong.

The second is the better default for a tool with no rate feed. Pick one and
justify it.

**Multiplicity.** Three copies of one pressing is not three times the value of
one: buyers discount lots, and the marginal copy of a duplicate is worth less
than the first. Consider a decay (first copy full value, second 80%, third 60%)
and say where you got the numbers.

**3c. `value_release`** (`src/app.rs`) — assemble a real `Valuation` from the
pieces above. The open question: **how does confidence combine across stages?**

Rarity is one input to price, or is it purely informational? If a rare pressing
gets a price *bump*, then `Rarity` needs to reach `apply_condition`, and the
signature changes. If rarity is only displayed, it does not. Both are coherent
designs; pick one deliberately.

---

## Exercise 4 — `Format::is_physical`

Currently a substring check against six words, in `src/domain/release.rs`.

The real question: **should digital entries be in a physical collection at
all?** If a user has listed a digital download, is it a data error, or are they
mixing media on purpose? If it is an error, `is_physical` is not enough — you
want a way to *report* the problem, not just a filter. If it is intentional,
they should not be valued like vinyl.

Also worth considering: `"Flexi"` and `"Unofficial Release"` are not the same
kind of thing. One is a real physical format; the other is a caveat about the
pressing. A single `bool` conflates them.

## Exercise 5 — Adapter tests against a local HTTP server

`src/adapters/discogs.rs` is currently tested at the level of its private
helpers (ISO-8601 parsing, currency conversion, condition mapping). The
`CollectionSource` and `SaleSource` *impls* have no tests.

The old code had a local test server; this is worth restoring, because HTTP
behaviour is where the real bugs live:

- a `Retry-After: 120` response actually waits 120 seconds (use
  `RecordingSleeper` and assert on the recorded duration — no real waiting);
- a `Retry-After: 9999` is refused rather than slept through;
- a 401 says something about tokens, not "HTTP error";
- a response with `items: 100` but 40 releases is caught by the domain, not
  silently exported.

Write this with a `RecordingSleeper` and a hand-rolled TCP listener (the crate
has no HTTP-server dependency, and adding one is probably not worth it for four
tests). The point is that `Sleeper` exists precisely so this test can be instant.

## Exercise 6 — `Format::render` should return `Result`

Both renderers currently return `String` and paper over serialization failure
in `ui/json.rs`. Change the signature to `Result<String>`, drop the
`unwrap_or_else` that fabricates an error-shaped JSON document, and let `main`
print a real message.

Then consider the memory trade-off: returning a `String` buffers the whole
report. For a 10,000-record collection that is a few megabytes — fine. If you
want streaming, `ui::json::write_to` already exists; wire the JSON path to it.

## Exercise 7 — Make the token an explicit input

`app::run_discogs` reads `std::env::var("DISCOGS_TOKEN")` itself, which means
the environment is an invisible input and the authenticated/anonymous pacing
split cannot be tested. Thread the token through explicitly:

```rust
pub fn run_discogs(token: Option<&str>, request: Request) -> Result<Report>
```

`cli::token_from` already returns the right thing. This is a small change with
a disproportionate payoff: after it, every code path in the binary is reachable
from a test.

---

## Non-exercises worth doing anyway

- **A `--sort` flag.** Sort by value, rarity, artist, year. Each is a different
  question about the same collection, and each is a different `Ord` on a view
  type. Note that sorting by value means handling unknowns — where do they go?
- **A local cache.** Collections change rarely; prices change constantly. Cache
  `Release` data and refresh `Sale` data on different schedules. This is where
  the port seam pays off: a `CachingCollectionSource` wrapping the real one is
  a few dozen lines and needs no changes anywhere else.
- **Condition tracking.** `Condition` exists and nothing sets it. Collector's
  copy grades vary per copy, so `OwnedRelease` may need to hold one grade per
  copy rather than a single `copies: u32`. That changes the valuation shape
  significantly — which is a good demonstration of why the domain is isolated.