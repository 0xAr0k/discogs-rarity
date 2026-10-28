---
name: Ratatui TUI
description: Working on the digger TUI (src/tui, src/bin/digger.rs) — the Elm loop rules, staleness tokens, and what must stay testable without a terminal
---

# The digger's rules

The TUI is a functional core / impure shell. Full decisions live in
[docs/DECISIONS.md](../../../docs/DECISIONS.md) (D-01…D-11); screens and keys
in [docs/UX.md](../../../docs/UX.md).

## Where code goes

| Kind of change | File |
|---|---|
| any behavior at all | `src/tui/update.rs` — the whole app is one pure `match` |
| how something looks | `src/tui/view.rs` — rendering only, no decisions |
| new state, message, effect | `src/tui/model.rs` first, then update, then view |
| terminal setup, threads, teardown | `src/tui/runtime.rs` — no decisions allowed in here |

## Invariants you can break without noticing

- `update` is pure and synchronous. Any I/O becomes an `Effect`; replies come
  back as `Msg`. Tests drive keys and assert state — with zero terminals.
- One staleness token: `Model::pending_open`. Every open reply carries the
  token it answers; a mismatched reply is dropped (`OpenRequest`,
  D-11). Enrichment replies always apply (D-10 tokens live in
  `EntityScreen::inflight`).
- The worker owns the ports and runs effects **strictly in order** — that
  sequence *is* the rate limiter. Adding concurrency here breaks policy.
- No unwired layer imports: the runtime takes the clock as a
  `Box<dyn Clock + Send>` port, exactly like the other ports (D-01).
- The wantlist view is a claim, not a prediction: `WantAdded`/`WantRemoved`
  confirms flip state; optimistic rows render as "adding…", not as owned.

## Before claiming the UI works

A green build proves compilation, not behavior. Run the pty smoke, at
minimum:

```sh
timeout 2 script -qec "cargo run --bin digger" /dev/null   # boots and renders?
```

Then update `docs/UX.md` if a key was added or renamed — every key listed
there must exist in `tui::update`.
