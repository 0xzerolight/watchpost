# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
- The CSRF cookie is checked for the shape this server mints, and carries `Secure` when the request
  arrived over HTTPS plus a 30-day `Max-Age` so it outlives the browser session.
- Repo discovery is a CSRF-gated POST; it used to be a GET that spent API calls as a side effect.
- Error responses carry no internal detail — no paths, no SQL, no upstream error strings.

[1.0.0]: https://github.com/0xzerolight/watchpost/releases/tag/v1.0.0
