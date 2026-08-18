# Architecture

## Stack

An axum server rendering [maud](https://maud.lambda.xyz) templates, with htmx for the interactive
bits (event editing, filters) and Chart.js for the charts. Everything the browser loads —
Chart.js, htmx, Pico CSS, the app's own CSS and JS — is vendored and embedded in the binary with
`include_bytes!`, so there is no CDN, no asset directory and no network fetch at page load. Storage
is SQLite through rusqlite in WAL mode, accessed from async code via a blocking pool so collection
never stalls request serving. The result is one static binary and one data directory: the SQLite
file, the WAL sidecars beside it, and whatever pre-migration backups it has taken.

| Layer | Choice |
| --- | --- |
| HTTP | axum 0.8 |
| Templates | maud 0.27 |
| Storage | rusqlite 0.40, bundled SQLite, WAL |
| GitHub client | reqwest 0.13, rustls |
| Scheduler | tokio-cron-scheduler 0.15 |
| CSS | Pico CSS 2.0.6 |
| Fragments | htmx 2.0.4 |
| Charts | Chart.js 4.4.7 |

`assets/vendor/MANIFEST.txt` records each vendored file's version, sha256, source URL and license.

## Collection

- The collector runs on `WATCHPOST_CRON` (default `0 5 * * * *`, so hourly at five past), plus once
  at startup, plus whenever you press **Sync now**.
- Per tracked repo it calls `repos/{name}`, `/pulls`, `/traffic/views`, `/traffic/clones`,
  `/traffic/popular/referrers`, `/traffic/popular/paths`, `/releases` and — once, on first sync —
  `/stargazers` to backfill star history.
- A per-repo failure records `last_error` and an exponential `backoff_until` and the loop moves on;
  one broken repo does not cost the others their pass. Partial success still writes what it got.
- A rate-limit response is different: it closes the rate gate and aborts the whole cycle, because
  the budget is global and the remaining repos would only fail too.
- Writes are day-keyed, NULL-safe, monotonic upserts. A repeated pass on the same UTC day overwrites
  rather than double-counts, so a manual sync is always safe.

## Storage rules

The schema is deliberately opinionated about what a missing number means:

- `NULL` is "not observed". `0` is "observed zero". They are not the same and the charts do not
  treat them the same.
- A daily row holds the last observation of that day, not a sum of the day's polls.
- Cumulative columns (`repo_stats.stars`, `release_assets.download_count`) carry forward across
  gaps at render time. Rate columns (views, clones) do not.
- Before any schema upgrade the database is copied through SQLite's backup API to
  `data/watchpost.v{schema}.{timestamp}.bak`, newest three kept. A database written by a newer
  build is refused rather than opened.

## Reading the numbers

A few things about GitHub's numbers are worth knowing before you draw conclusions from them.

**The 14-day window.** GitHub's traffic API only returns the last 14 days, which is the reason this
app exists: it samples hourly and keeps every day it has seen, so history accumulates past the
window. Days before your first run are gone for good — GitHub will not backfill them.

**Uniques are never summed.** A unique visitor on Monday may be the same person on Tuesday, so
adding daily uniques would overcount. Wherever a range covers more than one day — a chart zoomed
out to weeks or months, or the all-time referrer and path tables — the uniques figure shown is the
peak daily value in that range, not a total. Counts (non-unique views and clones) are summed,
because those are events.

**The period selector is a zoom, not a query.** A repo page opens on its whole history and ships
every day of it to the browser, so switching to 7, 30, 90 days or a year is instant and offline;
the `?days=` in the address bar is only a starting zoom, and the referrer and path tables ignore it
entirely (they are always all-time).

**Gaps mean "not observed".** If the app was down for a day, that day has no row, and rate metrics
(views, clones) render as a gap rather than as zero — an honest hole beats an invented zero.
Cumulative series (stars, download counts) carry the last known value forward across the gap,
because a total does not stop existing when nobody is watching.

**Stars are backfilled once.** On first sync of a repo, watchpost walks the stargazers API to
reconstruct the star history from before it was installed. GitHub stops paginating that endpoint at
40,000 stars, so for larger repos only the first 40,000 stargazers — the oldest ones — can be
reconstructed. The history between there and today is missing: the curve covers the early growth,
then jumps to the current total, and daily sampling takes over from the first sync onwards.

**Times display in `WATCHPOST_TZ`; days are UTC.** Timestamps you read — "last synced", the
`--doctor` rate-limit reset, and the day a new event defaults to — render in the zone you
configure, with that zone's abbreviation. Everything that groups by day does not: `WATCHPOST_CRON`,
the dates stored in the database, and the chart columns are all UTC, because GitHub returns traffic
already summed per UTC day and those buckets cannot be re-cut. A collection also runs at startup,
so restarting is a way to force a refresh.

## Diagnostics

```sh
docker compose exec watchpost watchpost --doctor
```

`--doctor` prints the effective configuration (the token as last-4 and length only, never the value)
and where that token came from, the database path, schema version and per-table row counts, the
current GitHub rate limit budget, and a per-repo table of last sync time, error streak, backoff and
last error. It exits non-zero if the database is unwritable, the API is unreachable, or no token has
been configured yet — which makes it usable as a post-deploy check.
