---
name: Rust DDD style
description: Keeping this crate's ports-and-adapters shape — use when writing or reviewing any code in src/domain, src/app, src/ports, src/adapters
---

# The shape to defend

Dependency arrows point one way: `ui/tui → app → ports ← adapters`, and `app`
depends on `domain`. Full rationale lives in
[docs/ARCHITECTURE.md](../../../docs/ARCHITECTURE.md); this skill is the
working checklist.

## Before you write a type in `domain`

- It must survive with no I/O, no clock, no RNG. If it needs the world, that
  need is a port — put a trait in `src/ports.rs` and let the adapter choose.
- A struct the wire might omit a field on gets `Option`, never a default
  zero. "Unknown" is not zero (see `CommunityStats`).
- Money is integer minor units, converted once at the boundary. No floats.
- Errors are values: `DomainError` closed enum in domain, `Error` top-level,
  no `anyhow`. `unwrap`/`expect` on external input is forbidden — see
  `scripts/boundary-guard.sh` rule 2.

## Before you write an adapter

- Adapter = translation, not decision. Wire types stay private, hand-built
  domain values come out, and `serde` derives never touch domain types.
- Translate every oddity the wire produces into a decision the domain
  already knows (`docs/DISCOGS.md` lists the known ones). A new oddity gets
  a decision + a `docs/DECISIONS.md` entry, not a bare `.unwrap_or_default`.
- Pacing is policy: one request per user action plus a gap. Never fire
  parallel requests from an adapter.

## Before you change `app` or `tui`

Keep the sweet spot. When adding a feature, use this table:

| Request | That lands in |
|---|---|
| fetch from somewhere other than Discogs | a new `adapters::` module |
| new way to slice catalog data | `domain::browse` vocabulary, then ports |
| new screen or key in digger | `tui` — model, then update tests, then view |

## Verification

```sh
./scripts/boundary-guard.sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo test   # expect exactly the 29 exercise failures (docs/EXERCISES.md)
```

Do not "fix" the 29 failing exercises — see
[docs/EXERCISES.md](../../../docs/EXERCISES.md).
