# digger — implementation plan

Everything here is done except where a box is empty. Not committed; all
changes sit in the working tree next to the `init` commit.

## Core

- [x] `src/domain/browse.rs` — the dig-side vocabulary: `EntityKind`,
      `CommunityStats`, `want_pressure`, `is_vinyl_format`, `CatalogRow`,
      `CatalogPage`, `SearchPage`, `ArtistProfile`, `LabelProfile`,
      `ReleaseDetail`, `SearchResult`, `WantEntry`, `Intent`/`parse_intent`
- [x] `domain::identity` gains `ArtistId`/`LabelId` newtypes;
      `rarity::band_of` extracted (`Rarity::band` delegates; exercises
      untouched — still `todo!()`, still 29 failures)
- [x] `ports`: `CatalogSource` (artist/label profiles, release pages, detail
      with cache cooperation, search) and `WantlistStore`
      (add/remove/list/contains on `&self`)
- [x] `adapters/discogs.rs` reworked: shared `DiscogsHttp` transport,
      `DiscogsCatalogSource` with session cache, paced-on-every-request,
      original tests restored plus digger fixture tests, master resolution,
      dedupe, anonymous-search refusal
- [x] `adapters/wantlist.rs` — `JsonWantlist`: `&self`, read-all-per-call,
      write-through temp file + rename, newest-first ordering
- [x] `adapters/testing.rs` — `FakeCatalogSource` (records
      `CatalogCall`s, canned pages, fault injection) and `MemoryWantlist`
- [x] `Cargo.toml` — ratatui 0.29 + crossterm 0.28 pinned as a pair

## TUI

- [x] `src/tui/model.rs` — `Model`, `Screen` stack, `Msg`, `Effect`,
      `OpenRequest` staleness tokens; large payloads boxed
- [x] `src/tui/update.rs` — pure reducer + ~18 unit tests (keys, replies,
      staleness, enrichment guards, wantlist confirm flow, paging bounds)
- [x] `src/tui/view.rs` — breadcrumb/spinner/body/status bands; search,
      entity, release, wantlist bodies; date + price helpers with tests
- [x] `src/tui/runtime.rs` — worker thread owning ports strictly in order,
      terminal guard with Drop restore, 50 ms loop, key-batch draining
- [x] `src/bin/digger.rs` — env (DISCOGS_TOKEN, data dir), wiring, exit codes

## Verification

- [x] `cargo fmt` clean
- [x] `cargo clippy --all-targets -- -D warnings` clean
- [x] `cargo test` — 137 pass; exactly the 29 exercise failures remain
- [x] smoke: draws the home screen on a real pty
- [x] smoke: boots on Home (first key press was dropped — found and fixed:
      the model had an empty stack; now `run()` boots with `[Home]`)
- [x] smoke: `a:108713` navigates, spinner runs, failed anonymous fetch
      degrades to a status line, no panic
- [x] smoke: `Esc` over the empty (or cleared) home quits with code 0 and
      restores the terminal (`?1049l` seen)

## Docs & tooling

- [x] `docs/UX.md`
- [x] `docs/DISCOGS.md`
- [x] `docs/DECISIONS.md` (D-01…D-11, Q-01…Q-06)
- [x] `docs/ARCHITECTURE.md` appendix (digger shape)
- [x] `README.md` — two-binary usage + digger section
- [x] `AGENTS.md`
- [x] `.opencode/skills/{rust-ddd-style, discogs-api, ratatui-tui,
      boundary-guard}/SKILL.md`
- [x] `.opencode/agents/{digger-reviewer, discogs-researcher}.md`
- [x] `opencode.jsonc` — instructions + never-push/reset permissions
- [x] `scripts/boundary-guard.sh` — domain purity greps that allow the
      intentional `todo!()` exercises; **proven to fail** on all three rules
      (injected `use crate::ports` / `unwrap()` / `todo!()`, watched it
      flag exactly those, reverted to byte-clean)

## Deliberately not done

- not committing anything (task: leave the tree uncommitted)
- no sqlite caching, no async runtime, no prefetching, no mouse
- the exercise modules stay unwritten (`docs/EXERCISES.md` owns them)
