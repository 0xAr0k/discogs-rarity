---
name: Discogs API
description: Making every Discogs HTTP request correctly — pacing, pagination, wire-shaped fields — and extending docs/DISCOGS.md when a new endpoint appears
---

# Talking to Discogs

Read [docs/DISCOGS.md](../../../docs/DISCOGS.md) first — it holds the pinned
facts and quirks for the endpoints this crate already uses. Never trust
memory over that doc; if the doc and the wire disagree, the wire wins and the
doc gets the correction.

## Sending a request

- All requests go through the shared transport (`DiscogsHttp`) and the
  pacing policy: **one request per user action**, plus the configured gap.
  The digger's sequential worker is the limiter — never parallelize.
- Requests are per user action, and the gap lives in the pacing config.
  Never fire parallel requests; the digger's sequential worker enforces this.
- `200` is not "answer": a `search` response type you don't recognize or an
  `id == 0` row is *dropped*, not defaulted (Q-02 in `docs/DECISIONS.md`).
- Pages are big on purpose: catalog fetches pull 100 rows so a flip is rare
  (`CATALOG_PER_PAGE`); search 50 (`SEARCH_PER_PAGE`).

## Adding an endpoint

1. Confirm the shape against the live wire (curl with `DISCOGS_TOKEN`).
2. Record the endpoint plus every quirk in `docs/DISCOGS.md`.
3. Private wire types in the adapter; translate by hand into domain values.
4. Tests with a canned fixture that shows one *odd* row, not just the happy
   row.
