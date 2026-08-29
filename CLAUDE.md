# CLAUDE.md — watchpost

- **Gate**: `make ci` — `cargo fmt --check`, `cargo clippy --all-targets --locked -D warnings`,
  `cargo test --locked`. Nothing is done until it passes.
- **Version source**: `version` in `Cargo.toml`, and nothing else. The tag is `v` + that value.
  `RELEASING.md` is the procedure.
- **Release notes**: `docs/release_notes/vX.Y.Z.md` — the body of the GitHub Release, passed as
  `gh release create --notes-file`. The CHANGELOG is the technical record and is not the body:
  the notes say what changed for the user, the CHANGELOG says why. Exemplar: `v1.2.0.md`.
  `docs/` is otherwise gitignored; `docs/release_notes/` is the one tracked subdirectory.
- **Spelling**: British — `colour`, `sanitised`, `honour`, `behaviour`.
