# Exercises — retired

The seven learning exercises this file used to describe were removed on
2026-10-23 at the operator's request (see `docs/DECISIONS.md`, D-20).

What was removed, and why it mattered beyond test hygiene:

- The 29 deliberately failing tests (`domain::rarity` 8, `domain::market` 13,
  `domain::valuation` 7, `app::signals` 1) are gone.
- The `todo!()` functions they gated (`rarity::edition_signal`,
  `rarity::classify`, `market::median`, `market::estimate_from_sales`,
  `valuation::apply_condition`, `CollectionValuation::total`,
  `app::signals_for`) are gone too — several sat under **production** call
  paths (`snapshots_for` called the estimator stub), so live data would have
  panicked the CLI report.
- `snapshots_for` now reports `PriceEstimate::UNKNOWN` while carrying the raw
  sales, and `CollectionValuation.total` is absent — the renderers already
  print these as honest absences, not zeros.

The judgement calls the exercises used to hold open — signed sales, want
pressure, confidence, currency totals — are still open questions, living as
plain comments next to the code that will need the answers. A clean `cargo
test` run is all green.
