# Semantic Rust: notes for this codebase

A tour of the Rust ideas this crate actually uses, written as explanations of
*why they appear here* rather than a language reference. Each section says what
the feature is, where it is in `src/`, and what it would look like if you got
it wrong.

---

## 1. Make illegal states unrepresentable

**The idea.** If a value cannot exist, do not let a type hold it. Then the
compiler carries the invariant and you stop defending it at every call site.

**Here.** `src/domain/identity.rs` defines four newtypes over integers:

```rust
pub struct ReleaseId(u64);
pub struct FolderId(u32);
pub struct PageSize(u32);
```

Without them, this compiles and silently queries the wrong thing:

```rust
fn fetch(release_id: u64, folder: u64, per_page: u64)
client.collection(&user, folder, release_id); // swapped; no error
```

With them, `rustc` rejects it. That is the whole value: the mistake becomes
unrepresentable rather than caught by a test you might forget to write.

**The deeper version.** The numbers inside are still `u64`, so `ReleaseId(0)`
still exists inside the module. The public surface does not offer it:

- the tuple field is private (no `pub struct ReleaseId(u64)`),
- construction goes through `const fn new(id) -> Option<Self>`,
- and the `TryFrom<u64>` impl is the ergonomic door in.

`const fn` matters more than it looks. `ReleaseId::new` can be evaluated at
compile time, so a bound like `MAX` cannot drift away from the validator — there
is one rule, written once, in one place.

**The lesson.** The question to ask when writing a function is not "what does
it take?" but "what *is* a valid input?" The second question has a type
attached to it.

---

## 2. Errors as values, not strings

**The idea.** `anyhow::Error` erases a cause into a string. That makes
`match`-ing on the reason impossible and forces callers to print.

**Here.** Two error types with different jobs:

- `DomainError` (`src/error.rs`) — a **closed enum**. It cannot grow a variant
  by accident, and `match` over it is checked for exhaustiveness.
- `Error` — a message plus an `Option<DomainError>`, so the reason survives
  being wrapped with human context.

```rust
pub fn context(mut self, context: impl Display) -> Self {
    self.message = format!("{context}: {}", self.message);
    self   // domain: Option<DomainError> still set
}
```

That is why `src/adapters/discogs.rs` can say *where* a failure happened while
the domain still owns *what* was wrong.

**The small Rust detail.** `DomainError` derives `PartialEq` but **not** `Eq`,
because one variant carries an `f64` and `f64` is not `Eq` — `NaN != NaN`.
Deriving `Eq` would require a wrapper that lies. Omitting the bound is the
honest option: `f64` really is not totally ordered.

**`?` and `From`.** `?` does three things: unwraps, converts via `From`, and
returns early. The `From<DomainError> for Error` impl is what lets a domain
function's error flow out through a UI function unchanged.

---

## 3. `Option` for absence, never a sentinel

**The idea.** A sentinel that means "unknown" (`0`, `""`, `-1`) is one `if`
away from being summed, compared, or averaged as if it were real.

**Here.**

```rust
pub struct PriceEstimate {
    pub median: Option<Money>,   // None = we cannot say; Some(0) = worthless
    pub sample_size: usize,
    pub confidence: f64,
}
```

Those are different claims and the type keeps them apart all the way to the
renderer. `src/ui/text.rs` and `src/ui/json.rs` both have a test asserting a
zero never appears where an unknown should be. That is the payoff.

Same pattern in `RaritySignals`, where every field is `Option<f64>`: filling in
`0.0` would make "nobody owns any" indistinguishable from "we have no data",
and those argue in opposite directions about what to buy next.

**The related Rust lesson:** `Option<Money>` on a struct makes
`Option<PriceEstimate>` and `Option<Rarity>` *not* free. `None` is contagious
by design — it forces every consumer to handle the absence, which is exactly
what you want when the absence is common.

---

## 4. Ports and adapters

**The idea.** Keep the interesting logic free of the messy outside world by
describing that world as traits written in *your* vocabulary.

```text
   ui  ──▶  app  ──▶  ports  ◀──  adapters
                 └──▶  domain
```

**The payoff is test speed.** `src/adapters/discogs.rs` originally called
`thread::sleep` in the middle of business logic. Any test touching rate-limit
handling waited up to five real minutes. Now:

- `Sleeper` (`src/ports.rs`) is the ability to wait,
- `RecordingSleeper` (`src/adapters/testing.rs`) records durations and returns
  instantly,
- and the *policy* — how long to wait — lives in `Pacing::backoff_for`, which
  is a pure function you can test exhaustively.

```rust
assert_eq!(
    pacing.backoff_for(Some(Duration::from_millis(10))),
    pacing.interval,
    "a too-short Retry-After must be raised to the interval"
);
```

That test would have taken a minute before. Now it takes microseconds, and it
is asserting on behaviour rather than on elapsed wall-clock time.

**`&self`, not `&mut self`.** No port mutates. That is not style: it means the
app can hold several sources at once, share them across calls, and hand in a
fake built in two lines.

**The test that enforces it.** `tests/integration.rs` defines a `CannedSource`
implementing `CollectionSource` from outside the crate. If a trait signature
ever picks up a private type, that file stops compiling. Architecture that is
checked by a test is architecture that survives.

---

## 5. Functional core, imperative shell

**The idea.** Push side effects to the edges; keep the middle pure.

**Here.** The signature of the central domain rule:

```rust
pub fn assemble(self) -> Result<Collection, DomainError>
```

It takes pages, returns a collection or a named reason. No I/O, no mutation of
its argument, no hidden state. You can read it top to bottom and know exactly
what it does.

Contrast with the imperative version this replaced:

```rust
// The original, in src/utils/dedupe.rs
for release in releases {
    let next = unique.len();
    let index = *positions.entry(release.id).or_insert(next);
    if index == next {
        unique.push(release);
    } else {
        unique[index] = release;   // <- the rule hides in here
    }
}
```

The `HashMap`+`Vec` dance encodes two rules — "last copy's details win" and
"first-seen order survives" — in four lines with no name attached. Now they are
in `Collection::dedupe`'s doc comment, and `OwnedRelease` makes the copy count
a *field* instead of something you have to re-derive by counting.

**Functions take references, return results.** If you need a changed version,
return a new value. That is what makes each rule independently testable: three
literal structs and a `?` away.

**Let-else for early exits.** Used throughout the adapters:

```rust
let mut headers = HeaderMap::new();
let Some(token) = token else { /* anonymous */ };
```

The happy path stays at one indent level. Compare with `if let Some(x) = ... {
   ... 8 more lines ... }` where the real code is eight spaces in.

---

## 6. `?`, `map`, `and_then` — composition

**The idea.** These are the monadic bits, and they are worth learning because
they make chains read like pipelines instead of nesting.

```rust
// Reading: build a URL, then send it, then parse it.
let value = self
    .client
    .get(page_url.clone())
    .send()
    .map_err(|e| Error::msg(format!("{label}: request failed: {e}")))?
    .json()
    .map_err(|e| Error::msg(format!("{label}: malformed JSON: {e}")))?;
```

`map_err` is the one to reach for. `?` gives up the original error type; adding
context means converting, and `map_err` says exactly where the context goes.

**Where composition shines — iterators.** `src/domain/collection.rs`:

```rust
releases: pages.into_iter().flat_map(|page| page.releases).collect()
```

No index, no `push`, no off-by-one. And the enumeration idiom, which reads as
"for each of these, return these" rather than "loop, assign, return":

```rust
let (total_pages, total_items) =
    claimed.ok_or(DomainError::ShortCollection { got: 0, expected: 0 })?;
```

**`Option::is_some_and`** is worth knowing: `map_or(false, |x| …)` with a
closure that takes the value *by value* rather than by reference. It reads
better than the older form and clippy nudges you toward it.

**The version-agnostic alternative:** if you get tired of `Option` combinators,
`let … else` and an early `return` are equally idiomatic. Consistency within a
file matters more than picking the fanciest tool.

---

## 7. Trait objects for flexibility, generics for static dispatch

**The idea.** `Box<dyn Error>` and `&dyn Trait` are one pointer plus a vtable
and let you choose an implementation at runtime. Generics monomorphize into
separate code per type.

**Here.** `app::run` takes `&dyn CollectionSource` — one source per process,
chosen at startup, so the indirection is free and the testability is worth it.

```rust
pub fn run(
    request: Request,
    collection_source: &dyn CollectionSource,
    sale_source: &dyn SaleSource,
    clock: &dyn Clock,
    sleeper: &dyn Sleeper,
    pacing: Duration,
) -> Result<Report>
```

That is five dependencies as arguments. It looks verbose. What it buys: the
whole pipeline is testable with fakes, and nothing reads the environment or
sleeps for real during a test run.

**Generics would be wrong here.** `fn run<S: CollectionSource>(source: &S)`
generates a copy of `run` — and everything it calls — per source type, and
cannot accept "whatever source this is" without a generic function anyway.
Where generics genuinely win is `impl Trait` in return position, for iterator
chains, where monomorphization lets the compiler see the whole chain.

**`impl Iterator` in arguments.** `src/domain/market.rs`:

```rust
pub fn snapshots_for<'a>(
    releases: impl Iterator<Item = &'a Release>,
    sales_by_release: &'a HashMap<ReleaseId, Vec<Sale>>,
    ...
```

The caller passes anything iterable. One monomorphized copy, zero boxing, and
the function can still be called with a `vec.iter()`.

---

## 8. Ownership: the actual reason this is memory-safe

**The idea.** Every value has one owner. When the owner goes out of scope, the
value is freed. Moves are explicit; references are permissions that end.

**Where it shows up here.**

*Slices instead of `&Vec`.* `median(samples: &[i64], …)` accepts a slice, a
`Vec`, or an array. `&Vec<T>` would force callers to have a `Vec` specifically.

*`&str` instead of `String`.* `Username::new(raw: &str) -> Result<Username>`
borrows during parsing and allocates once, on success.

*Borrowing to avoid cloning.* `PageSet::assemble` moves `self.pages` and
`flat_map`s over it rather than cloning each `Page`.

**The rule that catches people:** the iterator you pass must not outlive the
data it reads.

```rust
pub fn snapshots_for<'a>(
    releases: impl Iterator<Item = &'a Release>,   // borrows for 'a
    sales_by_release: &'a HashMap<…>,               // same 'a
```

One lifetime, written once, tying both to the same borrow. Elision would not
have done this: with two input lifetimes, `&mut` output would be rejected until
you said which input it came from.

**`clone` as a deliberate cost.** `Collection::dedupe` clones each `Release`.
That is intentional and localized — it returns an owned `Vec<OwnedRelease>` that
outlives the borrow. The alternative is threading lifetimes through every
caller, which is worse. Measure before optimizing; `Release` is a few `String`s,
not a megabyte.

---

## 9. Trait-based design over inheritance

**The idea.** No classes, no inheritance. Reuse comes from traits with default
methods, and composition comes from types containing types.

**Here.** `Format::is_physical` is a method on `Format` because physicality is
a property *of a format*. `Collection::dedupe` is a method on `Collection`
because deduplication is a property *of a collection*. The decision to keep
them as methods rather than free functions in a `utils` module is the
organizing principle of the whole crate: put a function next to the type it
knows about.

**A trait with a purpose.** `Clock` is three lines. It exists so
`src/domain/market.rs` can be a pure function of `(sales, now, window)` instead
of secretly reading the system clock. `FixedClock(1_700_000_000)` makes every
"is this sale recent?" test deterministic.

**The Rust-specific twist — sealed-ish traits.** Nothing here is sealed yet,
but if you later find that `SaleSource` is only implemented by `Discogs…`, add
a private supertrait or a sealed marker so users cannot implement it
incorrectly. Note the deliberate exception in `adapters::testing`: fakes are
*meant* to be implemented outside, so they are not sealed.

---

## 10. Documentation that carries the reasoning

**The idea.** `///` comments are not restating the signature. They explain why
the function is shaped the way it is, what invariant it maintains, and what the
caller must not do.

**Here.** A few that earn their keep:

```rust
/// # Why median and not mean
///
/// One typo — a €400 listing for a €4 record — drags a mean into fiction.
/// The median ignores up to half the sample, so a handful of bad values cannot
/// move it. Mean also has no natural place to *land*: the median of a set of
/// prices is itself a price somebody actually paid.
```

That is not documentation — it is the argument for the design, preserved next
to the code. When someone later "simplifies" median to mean, they will read
this first.

Also present: the invariant each test protects (`copies must be conserved`,
`first-seen order must survive`), the trade-off behind a choice
(`PartialInfo is more useful but honest…`), and the deliberate exceptions
(why `Eq` is omitted).

**Doc tests.** `/// ```ignore ` blocks are examples that should compile but are
not run. `/// ```` ` blocks (no `ignore`) are run by `cargo test`. Both appear
here; the non-ignored ones are a free correctness check on the examples.

---

## 11. `clippy` is a reviewer, not a nag

**The lint that mattered here:** `manual_is_multiple_of`, on

```rust
let rounded = magnitude / 2 + u64::from(magnitude % 2 != 0);
```

Clippy pointed at `is_multiple_of`. That is a better name for the same
operation, and adopting it makes the intent legible.

**When to ignore clippy, and how.** Justify it in the code:

```rust
#[allow(
    dead_code,
    reason = "parsed so unknown fields are consumed; see the note above"
)]
id: Option<u64>,
```

The `reason` field exists so an allow is a comment. A bare `#[allow(dead_code)]`
tells the next reader nothing.

**`#[must_use]`** would be a good addition to a few pure functions here
(`RaritySignal::new`, `Release::unit_count`) — the compiler can then warn about
a computed value that was thrown away. Clippy's `must_use_candidate` lint finds
these; it is part of the pedantic group and worth turning on for a library.

---

## 12. Idioms specific to this codebase

**`let … else`** — early exit without nesting. Used for token handling, error
mapping, and returning early from validation.

**`?` on `Option`** — `claimed.ok_or(DomainError::ShortCollection { … })?`
converts absence into a named failure. Cleaner than `match` when there is no
recovery to do.

**`split_once` / `is_some_and`** — parse and check without index arithmetic:

```rust
let (date, time) = raw.split_once('T')?;
label.split('(').next().unwrap_or(label)
title.to_lowercase().contains("signed")
```

**`div_euclid` / `rem_euclid`** — rounding and bucketing without the
negative-value trap that `/` and `%` have.

**`u64::from(bool)`** — clippy's preferred replacement for `if b { 1 } else { 0 }`.

**`#[derive(Debug)]` everywhere** — including on error types, so a failure can
be printed with structure rather than parsed out of a sentence.

---

## Where to go next

Read in this order:

1. `src/lib.rs` — the module map and the dependency-direction rule.
2. `src/domain/collection.rs` — the best pure function in the crate, with the
   reasoning for each refusal.
3. `src/error.rs` — why two error types.
4. `src/adapters/discogs.rs` — translation without decisions.

Then implement `docs/EXERCISES.md` in order. Each one is gated by a test that
already exists, so you always know when it is done.