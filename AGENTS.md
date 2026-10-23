# AGENTS.md — working in this repository

A Rust crate for Discogs *diggers*: a collection-report CLI and a browse/search
digger TUI, built as a ports-and-adapters teaching codebase. Read this before
changing code; the rules are short because the architecture doc is long.

## The one rule

Dependencies point one way: `ui/tui → app → ports ← adapters`, and `app`
depends on `domain`. Domain is pure: no I/O, no clock, no `unwrap`/`expect`
on external input, no panic on bad input (errors are values). Full text and
the reasons: **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)**.

Enforcement: `tests/integration.rs` implements the ports from outside the
crate, and `scripts/boundary-guard.sh` greps the domain layer's imports and
panic calls. Run it:

```sh
./scripts/boundary-guard.sh   # passes; break a rule once and watch it fail
```

## The two binaries

| Binary | Job | Entry |
|---|---|---|
| `diggers-rarity` | collection report CLI (batch) | `src/main.rs` → `app.rs` |
| `digger` | dig/search TUI (conversation) | `src/bin/digger.rs` → `tui::runtime` |

Cargo cannot guess between two binaries: use `cargo run --bin <name>` or
`cargo build --bin <name>`.

## Deliberate learning exercises (retired)

The 29 deliberately failing exercise tests were removed on 2026-10-23 at the
operator's request, together with the `todo!()` functions they gated (some sat
under production paths — see `docs/DECISIONS.md`, D-20). A clean test run here
is fully green. The scoring/pricing decisions those exercises held open live as
plain comments beside the code that will need them.

## Conventions

- **Adapter = translation, not decision.** Wire types are private per
  adapter; domain gets hand-built values, never `serde` derives.
- **Errors are values.** `DomainError` closed enum in domain; `Error`
  top-level; no `anyhow` below `main`.
- **Unknown is not zero.** `Option` for anything the wire may omit; display
  `…`/`—` vs `unknown` are different claims — do not collapse them.
- **Money is integer minor units**, rounded once, at the boundary.
- **Rate limits are policy.** One request per user action plus a gap; the
  digger's sequential worker is the limiter. Never fire parallel requests.
- **Comments explain why**, never what; module docs teach the decision the
  module embodies (see `src/app.rs` for the house voice).
- **Simplicity over cleverness:** three plain lines beat one premature
  abstraction; no defensive clones, parse once then move.

## Verification before handing back

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                  # clean run: all tests pass
cargo build --bin digger    # TUI compiles
```

Do not claim the digger's UI "works" from a green build alone — the TUI's
runtime behavior is only observable with a real TTY; at minimum run
`timeout 2 script -qec "cargo run --bin digger" /dev/null` and check it
boots and renders.

## Docs that must stay true

| Doc | Holds |
|---|---|
| `docs/ARCHITECTURE.md` | layers, seams, where to add things |
| `docs/UX.md` | digger screens/keys — every key listed must exist in `tui::update` |
| `docs/DECISIONS.md` | D-… decided, Q-… open — append, do not rewrite history |
| `docs/DISCOGS.md` | endpoint quirks — new endpoint usage goes here |
| `docs/EXERCISES.md` | the 29 failing tests and their intent |
