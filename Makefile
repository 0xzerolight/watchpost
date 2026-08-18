.PHONY: check test ci

check:
	cargo fmt --check
	cargo clippy --all-targets --locked -- -D warnings

test:
	cargo test --locked

ci: check test
