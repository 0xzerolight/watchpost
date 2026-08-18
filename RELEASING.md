# Releasing

watchpost ships as a Docker image on GHCR. A release is a git tag: CI builds and
publishes the multi-arch image, then you cut a GitHub Release to hold the notes.
There is no crates.io package and no manual image build.

## Versioning

- Single source of truth: `version` in `Cargo.toml`. Nothing else. The binary
  reads it at compile time via `env!("CARGO_PKG_VERSION")` for the GitHub
  User-Agent (`src/gh_client.rs`), and the README release badge reads it out of
  `Cargo.toml` on `main` — so the badge tracks `main`, not the newest tag. Keep
  the two together by tagging as soon as you bump.
- Semantic Versioning `MAJOR.MINOR.PATCH`:
  - PATCH (x.y.Z): bug fixes only.
  - MINOR (x.Y.0): new backward-compatible features — anything under `### Added`
    or `### Changed` in `[Unreleased]` that does not force the user to act.
  - MAJOR (X.0.0): breaking changes. A schema migration is not breaking on its
    own (migrations run automatically and back the database up first); a config
    or URL change the user has to make by hand is.
- The git tag is the version prefixed with `v` (e.g. `v1.1.0`).

## Release steps

All on `main`, fully merged and green.

1. Decide the new version from what is under `## [Unreleased]` in `CHANGELOG.md`.

2. Bump `Cargo.toml`:

       version = "X.Y.Z"

3. Refresh the lockfile and commit it. `Cargo.lock` is tracked, and it carries
   the crate's own version, so a bump without this breaks the `--locked` builds
   CI runs:

       cargo check

4. Promote the CHANGELOG. Rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD`
   (today), add a fresh empty `## [Unreleased]` above it, and drop any empty
   `### Added/Changed/Fixed/Security` subsection.

5. Run the gate:

       make ci

6. Commit:

       git add Cargo.toml Cargo.lock CHANGELOG.md
       git commit -m "chore: release vX.Y.Z"

7. Push, tag, push the tag:

       git push origin main
       git tag vX.Y.Z
       git push origin vX.Y.Z

8. [`.github/workflows/docker-publish.yml`](.github/workflows/docker-publish.yml)
   builds `linux/amd64` and `linux/arm64` on native runners, pushes each by
   digest, and stitches one manifest list tagged `X.Y.Z`, `X.Y` and `latest` at
   `ghcr.io/0xzerolight/watchpost`. `ci.yml` runs on the tag push as well, so the
   full gate covers the release commit.

9. Cut the GitHub Release — this is where users read the notes:

       gh release create vX.Y.Z --title "vX.Y.Z" --notes-file notes.md

   `notes.md` is this version's `## [X.Y.Z]` section from `CHANGELOG.md`, which
   is the source of truth. The Release carries notes only; the image publish
   already happened on the tag push.

## Verify

- Watch the run: `gh run watch`, or the Actions tab.
- Confirm both architectures are in the manifest:

      docker buildx imagetools inspect ghcr.io/0xzerolight/watchpost:X.Y.Z

- Confirm an anonymous pull works, which is what an installing stranger does:

      docker logout ghcr.io
      docker pull ghcr.io/0xzerolight/watchpost:latest

  A `denied` here means the GHCR package is private. Fix it once, under the
  package's settings on GitHub → Change visibility → Public. The install scripts
  print this hint when their `docker compose pull` fails.
- Confirm the Release is live on `/releases` and its notes read correctly.

## Upgrading a deployment

    cd ~/watchpost
    docker compose pull && docker compose up -d

or `scripts/update.sh`. Pin a specific release with `WATCHPOST_REF=vX.Y.Z` — both
install scripts honour it, for the compose file they download as well as for
themselves. The database is backed up automatically before any schema migration.

## Notes

- Pushing to `main` runs CI only and publishes no image. Images come solely from
  a `v*` tag. Always tag for a real release.
- `compose.prod.yml` — what the installers download — pins `:latest`, so every
  existing install follows the newest tag on its next `docker compose pull`.
