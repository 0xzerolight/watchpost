# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

### Changed

- `WATCHPOST_GITHUB_TOKEN` is optional. Missing is no longer a startup error; it is the state the
  setup page exists to resolve. Schema v3 adds a `settings` table to hold a token saved there.
- `--doctor` reports which token is in use and where it came from, and fails an install that has
  none with a pointer to the setup page rather than a request that was never made.

### Fixed

- The Add event form pre-filled the UTC day, so between local midnight and the UTC rollover it
  defaulted to the wrong date.
- A line chart with only one or two observed days drew nothing at all — the Downloads card on a
  repo whose releases were first read this week was an empty plot area under a correctly scaled
  axis. Such points now get a marker.

## [1.0.0] - 2026-08-18

First release. Everything below is relative to the pre-release state of the repository, which was
never tagged.

### Added

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

[1.0.0]: https://github.com/0xzerolight/watchpost/releases/tag/v1.0.0
