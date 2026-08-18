# watchpost

Self-hosted tracking for your own GitHub repositories: one page per repo that puts the metrics
GitHub only keeps for 14 days — views, clones, referrers, popular paths — next to a timeline of
what you did to earn them. Post a release on Hacker News, add it as an event, and the spike lands
under a marker on the chart instead of being a spike you no longer remember the cause of.

It collects hourly into a local SQLite file, serves a small server-rendered dashboard, and talks to
nothing but the GitHub API.

![The repo page: stars, views, clones and downloads over 90 days, with event markers on every
chart, the top referrers and paths, and the event list underneath.](assets/screenshot.png)

## What it records

- Stars, forks, watchers, open issues and open PRs, sampled daily
- Traffic views and clones (count and uniques), referrers, popular paths
- Release asset download counts per tag
- Your own events — a post, a talk, a release announcement — with a date, title, URL, kind and
  notes, rendered as markers on every chart for that repo

## Quickstart

You need a GitHub personal access token. A fine-grained token is the better choice; under
**Repository permissions** grant:

- **Metadata: read** — the repository list and the basic counts; selected for you, cannot be removed
- **Administration: read** — the traffic endpoints (views, clones, referrers, paths)
- **Contents: read** — releases and asset download counts
- **Pull requests: read** — the open pull request count

A missing permission costs that one part of a collection pass, not the pass: without
*Administration: read* the token authenticates fine, every traffic call returns 403 and those
charts stay empty while the rest of the data still lands. A classic token with the `repo` scope
also works.

Traffic is only served for repositories you own or administer, whatever the token says.

Then:

```sh
git clone https://github.com/YOUR/watchpost.git
cd watchpost
cp .env.example .env       # paste the token into WATCHPOST_GITHUB_TOKEN
mkdir -p data              # bind-mounted at /app/data
docker compose up -d
```

Open <http://127.0.0.1:8080>. The first collection runs at startup, so the repo list appears within
a minute; traffic data follows on the same pass. Pick which repos to track on the settings page —
nothing is tracked until you say so.

The container runs as uid 1000. If your host user has a different uid, `chown -R <uid> data` after
creating the directory, or the database cannot be created.

## Configuration

All settings are environment variables; the image reads them from `.env` via compose.

| Variable | Default | Meaning |
| --- | --- | --- |
| `WATCHPOST_GITHUB_TOKEN` | *(required)* | PAT used for every API call |
| `WATCHPOST_CRON` | `0 5 * * * *` | Collection schedule, six fields (seconds first), UTC. An unparseable value falls back to the default |
| `WATCHPOST_DB_PATH` | `./data/watchpost.db` | SQLite file; `/app/data/watchpost.db` in the image |
| `WATCHPOST_HOST` | `127.0.0.1` | Bind address; the image sets `0.0.0.0` |
| `WATCHPOST_PORT` | `8080` | Bind port |
| `WATCHPOST_LOG` | `info` | `tracing` filter, e.g. `watchpost=debug` |
| `WATCHPOST_GITHUB_API_BASE` | `https://api.github.com` | Override for GitHub Enterprise or tests; must be `http`/`https`, and a missing trailing slash is added (`…/api/v3` → `…/api/v3/`) |

## Security and operations

**There is no authentication.** No login, no users, no API key: anyone who can reach the port gets
the whole app — every metric, plus write access to events, to the tracked-repo list and to the
sync button. Syncing spends the token's GitHub rate budget, so an open instance is also a way for
a stranger to exhaust it. The token itself is never rendered (`--doctor` prints its last 4
characters and length, nothing else), but everything it can read is on the page.

So both defaults keep the port private. Outside Docker `WATCHPOST_HOST` is `127.0.0.1`, and
`compose.yml` publishes to `127.0.0.1:8080`. The container itself listens on `0.0.0.0` — that is
what makes publishing work at all — but the host offers the port to the loopback and nowhere else,
so as shipped nothing on your network can reach it.

Widening that is one line, and worth a moment's thought first:

```yaml
ports:
  - "8080:8080"      # every interface on the host
```

Do that only behind a reverse proxy that does the authenticating. Otherwise leave it on the
loopback and reach it through an SSH tunnel. Do not expose it directly.

**Behind a TLS-terminating proxy, forward `X-Forwarded-Proto: https`.** The CSRF cookie is marked
`Secure` only when that header says the browser spoke HTTPS; setting it unconditionally would make
the cookie invisible to a plain-HTTP deployment and 403 every POST there. Only the first hop's
value is read.

**Backups are taken on migration, not on a schedule.** Before a schema upgrade the database is
copied to `data/watchpost.v{schema}.{timestamp}.bak` and the newest three are kept. That copy goes
through SQLite's backup API, so it is consistent even if a write is in flight.

For backups of your own, copying `data/watchpost.db` is not enough. WAL mode means committed data
can still be sitting in the `watchpost.db-wal` sidecar, so a copy of the main file alone is stale at
best and torn at worst. Either stop the container first, or let SQLite take the snapshot while it
runs:

```sh
sqlite3 data/watchpost.db ".backup data/snapshot.db"
```

## Reading the data

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

**Schedules are UTC.** `WATCHPOST_CRON` and every date in the database are UTC, including the day
an event is filed under. A collection also runs at startup, so restarting is a way to force a
refresh.

## Diagnostics

```sh
docker compose exec watchpost watchpost --doctor
```

`--doctor` prints the effective configuration (the token as last-4 and length only, never the value),
the database path, schema version and per-table row counts, the current GitHub rate limit budget,
and a per-repo table of last sync time, error streak, backoff and last error. It exits non-zero if
the database is unwritable or the API is unreachable, which makes it usable as a post-deploy check.

## Building from source

```sh
cargo build --release
./target/release/watchpost
```

Rust 1.88 or newer, no system dependencies beyond a C toolchain (SQLite is compiled in). `make ci`
runs the main gate — `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`,
`cargo test --locked` — which CI runs too, alongside a 1.88 build, a Docker build and an advisory
audit.

The Docker image is single-arch — it builds for the host architecture. Cross-building a multi-arch
image (cargo-zigbuild or buildx with a musl cross toolchain) is a follow-up, not a v1 concern.

## Architecture

An axum server rendering [maud](https://maud.lambda.xyz) templates, with htmx for the interactive
bits (event editing, filters) and Chart.js for the charts. Everything the browser loads —
Chart.js, htmx, Pico CSS, the app's own CSS and JS — is vendored and embedded in the binary with
`include_bytes!`, so there is no CDN, no asset directory and no network fetch at page load. Storage
is SQLite through rusqlite in WAL mode, accessed from async code via a blocking pool so collection
never stalls request serving. The result is one static binary and one data directory: the SQLite
file, the WAL sidecars beside it, and whatever pre-migration backups it has taken.

## License

AGPL-3.0-or-later — see [LICENSE](LICENSE).

Copyright © 2026 0xzerolight.
