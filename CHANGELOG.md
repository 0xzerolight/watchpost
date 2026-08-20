# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Recent changes on the dashboard.** A feed above the repo cards lists what moved, one row per
  repo per UTC day: stars, forks, watchers, open issues, open PRs, release downloads and container
  pulls, each as a signed delta. The cards show levels, so noticing that three stars arrived
  yesterday used to mean having memorised the old number — the difference was already in the
  database and nothing surfaced it. Four rules decide what counts as a change. The predecessor is
  the last *observed* value rather than the previous calendar day, so a sync gap produces one change
  on the day the next observation landed instead of a phantom pair; a first observation is dropped,
  because the first sync of a 400-star repo is a reading and "+400 stars" would be a fiction; a zero
  delta produces no row at all; and views and clones are excluded, being per-day rates where the
  day's own value already is the change. Day resolution is what `repo_stats` stores, so the feed
  renders the entire existing history the moment it ships, backfilled star history included. No
  schema migration, no new configuration and no new query per repo — one pass over three tables.
- **CSV and JSON export per repo** (`Export: CSV · JSON` in the repo page header). Both span the
  whole history and take no period, because the history is the point. The CSV is the chart data
  flattened — one row per UTC day, built from the same dense readers the charts plot, so a cell and
  the same day on a chart are the same number by construction rather than by agreement. The JSON is
  the raw record: observed rows only, no carry-forward, plus the release assets, container pulls,
  referrers, paths and events that have no place in a daily grid, stamped with the schema version
  the file was written at. An unobserved counter is an empty field in the CSV and `null` in the
  JSON, never a `0` — a file that filled gaps with zeroes would re-introduce on the way out the lie
  watchpost refuses to tell on the way in. Sync errors, backoff state and the saved token are
  operational rather than history and appear in neither. Both are plain read-only GETs, and like
  every other page they are unauthenticated. The repo-name half of the filename is sanitised, since
  it is upstream-owned and would otherwise be able to write the response's own headers.

## [1.1.0] - 2026-08-20

### Added

- **GHCR container pull counts, auto-detected.** Every sync also fetches each tracked repo's
  public package page (`github.com/{owner}/{repo}/pkgs/container/{name}`) and charts the
  cumulative pull count as a "Container pulls" card on the repo page. Scraped, because no GitHub
  API exposes the number; unauthenticated, so it costs no token scope and no rate budget. A repo
  without a package named after it 404s and is skipped — zero configuration. A failed scrape is a
  partial sync like any failing endpoint, and deliberately does not count toward the total-failure
  verdict that backs a repo off. Schema migration v4 adds the `container_pulls` table (day-keyed,
  monotonic MAX, same rules as release assets).

### Changed

- **The repo charts are redrawn on a validated palette.** Each series is a gradient-filled line on
  a palette checked for colourblind separation and 3:1 contrast against the card surface. The axis
  borders and vertical gridlines are gone, dates read `Aug 19` rather than `2026-08-19`, counts past
  five digits read `12.3K`, and the tooltip takes the card's own surface and ink instead of the
  library default. Event markers drop to a half-strength dashed line under a ringed dot, so a
  marker sits behind the data it annotates rather than across it.

- **Event-kind chips carry their colour on a dot, not in the text.** The label is body ink at every
  size, which frees the palette from having to be readable as small text and lets its two lightest
  slots be used as marks.

- **Chart cards with no observed data are hidden.** A repo that ships only docker images no longer
  shows a blank Downloads pane, and repos without container packages don't get a blank pulls pane;
  the same rule covers views/clones cards where the token lacks traffic permissions. A repo with
  nothing observed at all keeps its existing empty state.

## [1.0.0] - 2026-08-18

First release. Everything below is relative to the pre-release state of the repository, which was
never tagged.

### Added

- One-line install: `curl … | bash` on Linux and macOS, `irm … | iex` on Windows. Both pull a
  published image, write `PUID`/`PGID` so the bind-mounted `data/` is writable whatever the host
  uid is, wait for `/health`, and open the setup page. `scripts/update.sh` updates in place.
- A first-run setup page. An install with no `WATCHPOST_GITHUB_TOKEN` now boots and redirects every
  page to `/setup`, where a pasted token is checked against GitHub before it is saved and
  collection starts immediately. The token can be replaced from the settings page afterwards; only
  its last four characters are ever rendered. An environment token still wins and hides the form.
- Published multi-arch images at `ghcr.io/0xzerolight/watchpost`, `linux/amd64` and `linux/arm64`,
  built on a tag push.
- `PUID`/`PGID`: the container entrypoint aligns the data directory with the host user and drops
  privileges, replacing the manual `chown -R <uid> data` the README used to ask for.
- `WATCHPOST_TZ`: displayed times — "last synced", the `--doctor` rate-limit reset, and the day a
  new event defaults to — render in the configured IANA zone instead of always UTC. Stored dates,
  chart day buckets and `WATCHPOST_CRON` stay UTC.
- Global error toast: a failed request says so instead of failing silently.
- Loading indicators and disabled states on every request that can be waited on.
- Delete confirmation in a real dialog rather than a bare button.
- Visible sort-direction indicators on the sortable tables.
- Skip link, current-page state in the nav, and focus that survives a fragment swap.
- Response compression (gzip).
- Content-hashed asset URLs, served `immutable` for a year and revalidated with a 304.
- Schema migration v2, adding the indexes the page queries actually use.
- Pre-migration backups: the database is copied to `data/watchpost.v{schema}.{timestamp}.bak`
  before a schema upgrade, and the newest three are kept.
- AGPL-3.0-or-later license.

### Changed

- `WATCHPOST_GITHUB_TOKEN` is optional. Missing is no longer a startup error; it is the state the
  setup page exists to resolve. Schema v3 adds a `settings` table to hold a token saved there.
- `--doctor` reports which token is in use and where it came from, and fails an install that has
  none with a pointer to the setup page rather than a request that was never made.
- The pages are rebuilt on one shared component set: real labels on every form field, consistent
  headings, empty states, and a restructured repo, settings and index page.
- Spacing, type and colour are design tokens from a single source; the event-kind chips meet WCAG
  AA contrast.
- A period change re-scales the existing charts instead of rebuilding them.
- Inline event handlers are gone, replaced by delegated listeners.
- The repo overview is one query rather than one per repo, and the delta recompute is bounded to
  the window that can have changed.
- Minimum supported Rust version is 1.88, declared in `Cargo.toml`.
- Docker base images are pinned to explicit versions and the build caches its dependency layer; CI
  runs a locked build, an MSRV check, a Docker build and an advisory audit.

### Fixed

- The Add event form pre-filled the UTC day, so between local midnight and the UTC rollover it
  defaulted to the wrong date.
- A line chart with only one or two observed days drew nothing at all — the Downloads card on a
  repo whose releases were first read this week was an empty plot area under a correctly scaled
  axis. Such points now get a marker.
- Rate-limit classification is limited to 403 and 429; a transient 5xx is retried instead of being
  counted as a rate limit.
- Daily stats record the last observation of the day rather than the intraday maximum.
- Repo writes are transactional, so a failure part-way through no longer leaves a half-written repo.
- A partial sync no longer inflates the error streak and the backoff that follows from it.
- A database written by a newer build is refused at open with the version pair and the fix, instead
  of being served against a schema the binary does not know.
- Backup pruning keys off the embedded timestamp, so a v10 database no longer discards its newest
  backups first.
- The health endpoint verifies the database and queries the live schema.
- Background polls no longer clear the error toast or clobber a pending focus target.
- Enter submits the picker and edit-row forms.
- Sort links carry the client-side zoom instead of resetting it.
- Chart tooltips are clamped to the canvas, reduced-motion preferences are honoured, and the axis
  labels are readable.
- A poisoned mutex is recovered and a panicking handler is caught, rather than taking down the
  worker behind it.
- Configuration is validated at startup: the token, the API base URL and the log filter.

### Security

- Security headers on every response: Content-Security-Policy, X-Content-Type-Options,
  X-Frame-Options, Referrer-Policy and Cross-Origin-Opener-Policy.
- The policy allows no inline script or style at all (`script-src 'self'`, `style-src 'self'`), so
  an injected `<script>` or `onerror=` does not execute even if it survives the escaping.
- The CSRF cookie is checked for the shape this server mints, and carries `Secure` when the request
  arrived over HTTPS plus a 30-day `Max-Age` so it outlives the browser session.
- Repo discovery is a CSRF-gated POST; it used to be a GET that spent API calls as a side effect.
- Error responses carry no internal detail — no paths, no SQL, no upstream error strings.
- There is no authentication, and the README now says so outright: anyone who can reach the port
  has full read and write. Bind it to the loopback or put it behind a proxy that authenticates.
- `compose.yml` publishes to `127.0.0.1:8080` rather than every interface, so the default deployment
  is not reachable from the network.

[1.1.0]: https://github.com/0xzerolight/watchpost/releases/tag/v1.1.0
[1.0.0]: https://github.com/0xzerolight/watchpost/releases/tag/v1.0.0
