Test fixtures
=============

This directory previously held `fixtures/collection.json`, a Discogs API
response used by the old `src/api/tests.rs`.

The new design does not need it. Two reasons:

1. **Wire-format tests belong to the adapter.** A file that mirrors Discogs'
   JSON is a snapshot that drifts: when Discogs adds a field, the fixture stops
   being representative and the tests keep passing while telling you less. The
   adapter tests now build `serde_json::Value` or wire structs inline, so a
   schema change shows up as a test that needs attention rather than a stale
   blob nobody remembers updating.

2. **Domain tests should not touch the wire at all.** Every rule in
   `src/domain/` is tested with hand-built domain values — `Release`,
   `Page`, `Sale`. That is only possible because the domain has no
   `Deserialize` derives tying it to Discogs' JSON. A fixture that exercised
   the domain would undo that separation.

`integration.rs` lives alongside this file and drives the crate through its
public API only.