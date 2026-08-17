//! Library surface exposing modules needed by integration tests (e.g.
//! `tests/gh_client_test.rs`). The binary (`main.rs`) declares its own
//! `mod` statements for the same source files; that duplication is
//! intentional and keeps this task's footprint limited to what Task 5
//! needs, without refactoring `main.rs`'s existing module wiring.

pub mod errors;
pub mod gh_client;
pub mod ratelimit;
pub mod types;
