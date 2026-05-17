//! See the crate-level docs in `Cargo.toml`.
//!
//! This crate is intentionally tiny: it only emits `#[no_mangle] pub extern
//! "Rust" fn` provider symbols that match the forward declarations in
//! [`turbo_tasks::handle`]. Consumers should depend on this crate with the
//! `prod` and/or `test` features enabled to pull in the corresponding arm.
//!
//! Building this crate with neither feature compiles to an empty lib that
//! exports no symbols. This is intentional so that `cargo check --workspace`
//! works without explicit feature flags; binaries and test harnesses that
//! actually consume the handle dispatch must enable the appropriate feature(s).
