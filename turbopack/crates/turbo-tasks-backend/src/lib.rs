#![feature(anonymous_lifetime_in_impl_trait)]
#![feature(box_patterns)]
#![feature(macro_metavar_expr_concat)]

mod backend;
mod backing_storage;
mod data;
mod database;
mod error;
mod handle_providers;
mod kv_backing_storage;
mod utils;

use std::path::Path;

use anyhow::Result;
use turbo_persistence::{CompactConfig, TurboPersistence};

use crate::database::turbo::{self, TurboKeyValueDatabase};
pub use crate::{
    backend::{BackendOptions, StorageMode, TurboTasksBackend},
    backing_storage::BackingStorage,
    database::{
        db_invalidation,
        db_invalidation::StartupCacheState,
        db_versioning::{GitVersionInfo, handle_db_versioning},
    },
    kv_backing_storage::KeyValueDatabaseBackingStorage,
};

pub type TurboBackingStorage = KeyValueDatabaseBackingStorage<TurboKeyValueDatabase>;

/// Concrete `BackingStorage` type accepted by the prod dispatch arm. The
/// `__tt_static_*` providers in [`handle_providers`] are monomorphized
/// for `TurboTasks<TurboTasksBackend<ProdBackingStorage>>`; any caller
/// that wants its `TurboTasks::new(...)` instance to be drivable through
/// `TurboTasksHandle` must produce this exact type. The `Either` lets
/// the same handle type cover both the real on-disk cache and the noop
/// in-memory variant used by tests and the napi cdylib's `no-cache`
/// mode.
pub type ProdBackingStorage = either::Either<TurboBackingStorage, NoopBackingStorage>;

// Re-exported so consumers (test config files, the napi binding) can
// build a `ProdBackingStorage` without needing a direct dep on `either`.
pub use either::Either;

/// Wraps a [`TurboBackingStorage`] into [`ProdBackingStorage`] so callers
/// can hand the result to [`TurboTasksBackend::new`] and end up with the
/// concrete `TurboTasks<TurboTasksBackend<ProdBackingStorage>>` type the
/// `__tt_static_*` dispatch providers cast to. Equivalent to writing
/// `Either::Left(storage)` but doesn't require turbofish for type
/// inference at the call site.
pub fn prod_backing_storage_turbo(storage: TurboBackingStorage) -> ProdBackingStorage {
    Either::Left(storage)
}

/// Wraps a [`NoopBackingStorage`] into [`ProdBackingStorage`]. See
/// [`prod_backing_storage_turbo`] for the rationale.
pub fn prod_backing_storage_noop(storage: NoopBackingStorage) -> ProdBackingStorage {
    Either::Right(storage)
}

/// Creates a `BackingStorage` to be passed to [`TurboTasksBackend::new`].
///
/// Information about the state of the on-disk cache is returned using [`StartupCacheState`].
pub fn turbo_backing_storage(
    base_path: &Path,
    version_info: &GitVersionInfo,
    is_ci: bool,
    is_short_session: bool,
    skip_compaction: bool,
) -> Result<(TurboBackingStorage, StartupCacheState)> {
    KeyValueDatabaseBackingStorage::open_versioned_on_disk(
        base_path.to_owned(),
        version_info,
        is_ci,
        |path| TurboKeyValueDatabase::new(path, is_ci, is_short_session, skip_compaction),
    )
}

/// Creates an in-memory `BackingStorage` to be passed to [`TurboTasksBackend::new`]. Backed by
/// an empty, read-only [`TurboPersistence`] — reads return `None`, writes are not expected
/// (callers should set [`BackendOptions::storage_mode`] to `None`).
pub fn noop_backing_storage() -> TurboBackingStorage {
    KeyValueDatabaseBackingStorage::new_in_memory(TurboKeyValueDatabase::empty_in_memory())
}

/// Opens a Turbopack persistent cache database at the given base path and performs a full
/// compaction. This is intended for use by the `next internal post-build` CLI command to optimize
/// the database after a build, without requiring the full turbo-tasks runtime.
///
/// The parallel scheduler requires a Tokio runtime. If one is already active (e.g. when called
/// from a NAPI async function), it is reused. Otherwise a new multi-threaded runtime is created.
pub fn compact_database(
    base_path: &Path,
    version_info: &GitVersionInfo,
    is_ci: bool,
) -> Result<()> {
    let versioned_path = handle_db_versioning(base_path, version_info, is_ci)?;
    // The parallel scheduler uses `tokio::task::block_in_place` internally, which
    // requires a multi-threaded Tokio runtime. Create one only if there is no
    // active runtime (e.g. when called from a standalone CLI context).
    let _owned_runtime = if tokio::runtime::Handle::try_current().is_ok() {
        None
    } else {
        Some(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?,
        )
    };
    // If we created a runtime, enter it so the scheduler can find it.
    let _guard = _owned_runtime.as_ref().map(|rt| rt.enter());
    let db =
        TurboPersistence::<turbo::TurboTasksParallelScheduler, { turbo::FAMILIES }>::open_with_config(
            versioned_path,
            turbo::db_config(),
        )?;
    // Fully compact with no segment count limit (unlike the runtime shutdown path
    // which caps segments based on available parallelism).
    db.compact(&CompactConfig {
        max_merge_segment_count: usize::MAX,
        ..turbo::COMPACT_CONFIG
    })?;
    db.shutdown()
}
