.PHONY: check test ci

check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

test:
	cargo test

ci: check test
