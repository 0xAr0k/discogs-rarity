# Discogs API notes — the digger half

The collection CLI uses three endpoints and is documented in `src/app.rs`
and `src/adapters/discogs.rs`. The digger adds the *catalog* half of the API.
This file records what it learned, so the next person does not rediscover it
from a 403 page.

## Endpoints the digger uses

| Purpose | Endpoint | Auth | Notes |
|---|---|---|---|
| artist profile | `GET /artists/{id}` | optional | `name`, `realname`, `aliases`, `members`, `namevariations`, `urls` |
| label profile | `GET /labels/{id}` | optional | `sublabels`, `parentLabel`, `urls` (camelCase on the wire) |
| artist releases | `GET /artists/{id}/releases?page=&per_page=&sort=year&sort_order=desc` | optional | rows carry `type: release\|master`; `release` rows have `format` (string), `label`, `catno`; `master` rows have only `main_release` + `stats.community.{in_wantlist, in_collection}` — **no format, no label, no catno**, and their `id` is a master id |
| label releases | `GET /labels/{id}/releases?page=&per_page=` | optional | rows have `format` + `catno`, no `type` (all releases), no stats |
| release sales | `GET /releases/{id}` | optional | the digger reads the payload's `history` array: `price` (float, minor-unit-converted, USD/EUR/GBP/JPY), `sold_at` (ISO-8601), `condition` (free text, defaults to VG+ when unreadable), `title` (signed/media-only flags are scraped from this), and `seller` — seen as a bare username string **or** an object with `username`; unreadable rows are dropped, not a failure |
| release detail | `GET /releases/{id}?curr_abbr=USD` | optional | the only endpoint with community stats, prices, tracklist |
| database search | `GET /database/search?q=&type=&page=&per_page=` | **token required** | 401 anonymously; refused up front with a message that names `DISCOGS_TOKEN` |

## The facts worth pinning

- **Auth.** Header `Authorization: Discogs token=< TOKEN >`. Anonymous calls
  work for everything except search, at 25 req/min instead of 60.
- **Pagination.** Every catalog endpoint answers `pagination`
  `{ page, pages, items }`. The digger trusts `pages` for turning only — a
  server that lies about `items` does not create a loop here (unlike the
  collection contract, the digger shows what arrived instead of assembling a
  snapshot).
- **Master rows dress down.** On `/artists/{id}/releases` a `master` row
  publishes no `format`/`label`/`catno`; the openable id is its
  `main_release`. The adapter keeps the row (unknown ≠ non-vinyl), shows
  `…` in its unknown cells, and uses `main_release` as the row's id.
- **`format` is a string in listings** (`"Vinyl, LP, Album"`), an array of
  objects in release details. Two shapes, two wire types, both private.
- **`type: master` rows are abstract albums.** No release detail exists for
  them: no community block, no price, no tracklist. The digger keeps them in
  the table (they are part of an artist's story) but marks them and refuses
  to open them (Q-01).
- **Search results mix kinds.** `type` may be `artist | label | release |
  master | ...12 others...`. The digger keeps the three it can act on.
  `master` results carry `main_release` (sometimes only `master_id`); the
  digger resolves them to the main release id or drops the hit (Q-02).
- **Vinyl inclusion.** A row is shown when its format string contains
  `vinyl`, case-insensitive. This is the operator's rule ("show all the
  vinyls"), deliberately simpler than the exercise's `Format::is_physical`,
  and documented as a deviation in `docs/DECISIONS.md`, D-07. The search
  applies the same rule to release hits: CD, cassette, and file formats
  are dropped at the boundary, hits with no `format` array at all stay
  (unknown is not "CD").
- **Money.** `curr_abbr=USD` on release detail; `lowest_price` arrives as a
  float that is rounded *once* into integer minor units at the adapter
  boundary, never float-compared afterwards.
- **Rate limits.** 60 req/min with a token, 25 without — the adapter spends
  that budget as a token bucket (`Pacing::cap`): bursts up to eight
  requests fire at once, then refills at cap/minute, so a page load with
  its detail wave never out-paces the window the server actually
  enforces. `Retry-After` on a 429 still wins over any local arithmetic.
- **No snapshots.** Unlike the collection contract, digger pages are displayed
  as fetched; coherence duties apply only within one page.

## What is deliberately not used

- **OAuth request flow** — one token is enough for a single-operator tool.
- **The `search` POST** — the GET form needs no CSRF-style round trip.
- **`GET /masters/{id}`** — would let a master row show *something*, but its
  data is derived from pressings this digger would rather fetch on demand.
  Revisit if the empty `…` cells next to masters annoy someone in practice.
