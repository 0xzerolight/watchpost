# Contributing

Bug reports, feature requests and pull requests are all welcome.

## Setup

Rust 1.88 or newer. No system dependencies beyond a C toolchain — SQLite is compiled in.

```bash
git clone https://github.com/0xzerolight/watchpost.git
cd watchpost
cp .env.example .env    # paste a GitHub token into WATCHPOST_GITHUB_TOKEN
cargo run               # dotenvy loads .env automatically
```

Change the port with `WATCHPOST_PORT=8899 cargo run` if 8080 is taken.

## The gate

`make ci` must pass before a pull request.

```bash
make check   # cargo fmt --check + cargo clippy --all-targets --locked -- -D warnings
make test    # cargo test --locked
make ci      # both
```

CI runs four jobs and all of them must be green: `make ci` on stable, a
`cargo check --locked --all-targets` on 1.88 to hold the MSRV, a Docker build, and
`rustsec/audit-check`. See [`.github/workflows/ci.yml`](.github/workflows/ci.yml).

## Tests

Integration tests live in `tests/`. HTTP is faked with `wiremock` and storage with in-memory
SQLite, so no test touches the live GitHub API — and none should start to.

## Vendored assets

Everything the browser loads is embedded in the binary with `include_bytes!` and there is no CDN.
Adding or bumping a vendored file means updating `assets/vendor/MANIFEST.txt` with its version,
sha256, source URL and license.

## Commits

Conventional Commits — `feat`, `fix`, `docs`, `chore`, `ci`, `refactor`, `test`. Keep the subject
under 72 characters and write a body only when the "why" is not obvious from the diff.

`CHANGELOG.md` follows [Keep a Changelog](https://keepachangelog.com). User-visible changes go under
`[Unreleased]`.

## Architecture

[ARCHITECTURE.md](ARCHITECTURE.md) covers the stack, the collection loop, the storage rules and the
caveats behind the numbers. Worth reading before changing anything in `src/collector.rs` or
`src/db/`.
