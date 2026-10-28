---
name: Boundary guard
description: Running and interpreting scripts/boundary-guard.sh — mechanical enforcement of the layer rules — before handing any change back
---

# The guard

```sh
./scripts/boundary-guard.sh
```

It checks three rules (why each exists: header comments in the script):

1. **Import table** — every layer may only `use crate::…` the names its
   arrows allow (`domain` → `error` only; `tui` → domain/ports/tui; wiring
   in `src/bin`, `main.rs`, `lib.rs` is exempt). A multi-line `use crate::{…}`
   tree is lexed brace-aware, and doc comments that *mention* a forbidden
   import cannot fake a violation.
2. **Domain purity** — no `unwrap`/`expect`/`panic!` outside a
   `#[cfg(test)]` module in `src/domain`. Tests sit at the file bottom; the
   scan cuts the tail at `#[cfg(test)]`.
3. **`todo!` only in domain** — the exercise markers are documented in
   `docs/EXERCISES.md`; a `todo!` anywhere else would silently ship.

## Reading a failure

- `… imports crate::X — allowed here: …` — an arrow flipped. Move the code or
  move the dependency; do not edit the allow-list to make it pass, unless the
  architecture itself changed (then update `docs/ARCHITECTURE.md` **first**,
  and the table in the script).
- `domain must not call … outside tests` — external input handling leaked
  into domain. Errors are values: return them.
- `todo! outside src/domain would silently ship` — either delete it or, if it
  is a new deliberate exercise, it belongs in the documented set, not here.

Run it together with the rest:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
./scripts/boundary-guard.sh
cargo test   # exactly the 29 exercise failures, nothing else
```
