//! Library crate: declares all modules once. The binary (`main.rs`)
//! consumes this crate via `use watchpost::...` so there is a single
//! compilation of each module (one set of nominal types shared by the
//! bin, unit tests, and integration tests).

pub mod config;
pub mod db;
pub mod errors;
pub mod gh_client;
pub mod ratelimit;
pub mod types;
