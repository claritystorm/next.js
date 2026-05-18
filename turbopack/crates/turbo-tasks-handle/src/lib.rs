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

#![feature(macro_metavar_expr_concat)]

use std::sync::Arc;

use turbo_tasks::{HandleTag, TurboTasksHandle};

// =====================================================================
// Prod arm
// =====================================================================

/// The concrete prod handle type — matches what `next-napi-bindings` uses.
///
/// If a future binary needs a different `Backend`/storage combination,
/// this crate's prod arm would need to be re-parameterized (or a parallel
/// crate added). Today there is exactly one prod handle type so we
/// hardcode it.
#[cfg(feature = "prod")]
pub type ProdHandleConcrete = turbo_tasks::TurboTasks<
    turbo_tasks_backend::TurboTasksBackend<
        either::Either<
            turbo_tasks_backend::TurboBackingStorage,
            turbo_tasks_backend::NoopBackingStorage,
        >,
    >,
>;

#[cfg(feature = "prod")]
mod prod {
    use turbo_tasks::{TurboTasksApi as _, TurboTasksCallApi as _};

    use super::*;

    /// Generates `#[no_mangle] pub extern "Rust" fn __tt_prod_<name>(...)`
    /// for a single dispatched method. The body casts `ptr` back to
    /// `&ProdHandleConcrete` and forwards through method call syntax so
    /// both `TurboTasksApi` and `TurboTasksCallApi` methods resolve
    /// correctly (the former is a super-trait of the latter).
    macro_rules! provide_prod {
        (
            fn $name:ident( $($arg:ident : $ty:ty),* $(,)? ) $(-> $ret:ty)?
        ) => {
            #[unsafe(no_mangle)]
            pub extern "Rust" fn ${concat(__tt_prod_, $name)}(
                ptr: *const ()
                $(, $arg : $ty)*
            ) $(-> $ret)? {
                let tt: &ProdHandleConcrete = unsafe { &*(ptr as *const ProdHandleConcrete) };
                tt.$name($($arg),*)
            }
        };
    }

    // ---- dispatched methods ----------------------------------------------
    //
    // Keep this list in sync with the matching `tt_decl_extern!` /
    // `tt_decl_handle_method!` invocations in
    // `turbopack/crates/turbo-tasks/src/handle.rs`. A future cleanup could
    // generate both from a shared callback macro, but for now duplicating
    // the list is the simplest source-of-truth.

    // TurboTasksCallApi
    provide_prod!(fn dynamic_call(
        native_fn: &'static turbo_tasks::macro_helpers::NativeFunction,
        this: Option<turbo_tasks::RawVc>,
        arg: &mut dyn turbo_tasks::StackDynTaskInputs,
        persistence: turbo_tasks::TaskPersistence,
    ) -> turbo_tasks::RawVc);
    provide_prod!(fn native_call(
        native_fn: &'static turbo_tasks::macro_helpers::NativeFunction,
        this: Option<turbo_tasks::RawVc>,
        arg: &mut dyn turbo_tasks::StackDynTaskInputs,
        persistence: turbo_tasks::TaskPersistence,
    ) -> turbo_tasks::RawVc);
    provide_prod!(fn trait_call(
        trait_method: &'static turbo_tasks::TraitMethod,
        this: turbo_tasks::RawVc,
        arg: &mut dyn turbo_tasks::StackDynTaskInputs,
        persistence: turbo_tasks::TaskPersistence,
    ) -> turbo_tasks::RawVc);
    provide_prod!(fn send_compilation_event(
        event: ::std::sync::Arc<dyn turbo_tasks::message_queue::CompilationEvent>,
    ));
    provide_prod!(fn get_task_name(task: turbo_tasks::TaskId) -> ::std::string::String);

    // TurboTasksApi
    provide_prod!(fn invalidate(task: turbo_tasks::TaskId));
    provide_prod!(fn invalidate_with_reason(
        task: turbo_tasks::TaskId,
        reason: turbo_tasks::util::StaticOrArc<dyn turbo_tasks::InvalidationReason>,
    ));
    provide_prod!(fn invalidate_serialization(task: turbo_tasks::TaskId));
    provide_prod!(fn try_read_task_output(
        task: turbo_tasks::TaskId,
        options: turbo_tasks::ReadOutputOptions,
    ) -> ::anyhow::Result<::core::result::Result<turbo_tasks::RawVc, turbo_tasks::event::EventListener>>);
    provide_prod!(fn try_read_task_cell(
        task: turbo_tasks::TaskId,
        index: turbo_tasks::CellId,
        options: turbo_tasks::ReadCellOptions,
    ) -> ::anyhow::Result<::core::result::Result<turbo_tasks::backend::TypedCellContent, turbo_tasks::event::EventListener>>);
    provide_prod!(fn try_read_local_output(
        execution_id: turbo_tasks::ExecutionId,
        local_task_id: turbo_tasks::LocalTaskId,
    ) -> ::anyhow::Result<::core::result::Result<turbo_tasks::RawVc, turbo_tasks::event::EventListener>>);
    provide_prod!(fn read_task_collectibles(
        task: turbo_tasks::TaskId,
        trait_id: turbo_tasks::TraitTypeId,
    ) -> turbo_tasks::backend::TaskCollectiblesMap);
    provide_prod!(fn emit_collectible(
        trait_type: turbo_tasks::TraitTypeId,
        collectible: turbo_tasks::RawVc,
    ));
    provide_prod!(fn unemit_collectible(
        trait_type: turbo_tasks::TraitTypeId,
        collectible: turbo_tasks::RawVc,
        count: u32,
    ));
    provide_prod!(fn unemit_collectibles(
        trait_type: turbo_tasks::TraitTypeId,
        collectibles: &turbo_tasks::backend::TaskCollectiblesMap,
    ));
    provide_prod!(fn try_read_own_task_cell(
        current_task: turbo_tasks::TaskId,
        index: turbo_tasks::CellId,
    ) -> ::anyhow::Result<turbo_tasks::backend::TypedCellContent>);
    provide_prod!(fn read_own_task_cell(
        task: turbo_tasks::TaskId,
        index: turbo_tasks::CellId,
    ) -> ::anyhow::Result<turbo_tasks::backend::TypedCellContent>);
    provide_prod!(fn update_own_task_cell(
        task: turbo_tasks::TaskId,
        index: turbo_tasks::CellId,
        content: turbo_tasks::backend::CellContent,
        updated_key_hashes: ::core::option::Option<::smallvec::SmallVec<[u64; 2]>>,
        content_hash: ::core::option::Option<turbo_tasks::backend::CellHash>,
        verification_mode: turbo_tasks::backend::VerificationMode,
    ));
    provide_prod!(fn mark_own_task_as_finished(task: turbo_tasks::TaskId));
    provide_prod!(fn connect_task(task: turbo_tasks::TaskId));
    provide_prod!(fn spawn_detached_for_testing(
        f: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ()> + ::core::marker::Send + 'static>>,
    ));
    provide_prod!(fn subscribe_to_compilation_events(
        event_types: ::core::option::Option<::std::vec::Vec<::std::string::String>>,
    ) -> ::tokio::sync::mpsc::Receiver<::std::sync::Arc<dyn turbo_tasks::message_queue::CompilationEvent>>);
    provide_prod!(fn is_tracking_dependencies() -> bool);

    // ---- Arc clone / drop -------------------------------------------------

    #[unsafe(no_mangle)]
    pub extern "Rust" fn __tt_prod_clone_arc(ptr: *const ()) {
        // Bump the refcount of the Arc whose data pointer is `ptr`. The
        // caller (`<TurboTasksHandle as Clone>::clone`) is responsible for
        // reusing the same `ptr` value in the new handle, so we don't need
        // to return anything.
        unsafe {
            Arc::<ProdHandleConcrete>::increment_strong_count(ptr as *const ProdHandleConcrete)
        }
    }

    #[unsafe(no_mangle)]
    pub extern "Rust" fn __tt_prod_drop_arc(ptr: *const ()) {
        // Decrement the refcount; runs the destructor when it reaches zero.
        unsafe {
            Arc::<ProdHandleConcrete>::decrement_strong_count(ptr as *const ProdHandleConcrete)
        }
    }

    /// Constructs a `TurboTasksHandle` pointing at the given prod Arc.
    ///
    /// The caller transfers ownership of one refcount into the handle.
    pub fn from_prod(arc: Arc<ProdHandleConcrete>) -> TurboTasksHandle {
        let ptr = Arc::into_raw(arc) as *mut ();
        // Safety: ptr came from Arc::into_raw on a ProdHandleConcrete and
        // the tag is consistent with that type.
        unsafe {
            TurboTasksHandle::from_raw_parts(HandleTag::Prod, std::ptr::NonNull::new_unchecked(ptr))
        }
    }
}

#[cfg(feature = "prod")]
pub use prod::from_prod;

// =====================================================================
// Test arm
// =====================================================================

#[cfg(feature = "test")]
mod test_arm {
    use turbo_tasks::{TurboTasksApi as _, TurboTasksCallApi as _};

    use super::*;

    pub type TestHandleConcrete = turbo_tasks_testing::VcStorage;

    macro_rules! provide_test {
        (
            fn $name:ident( $($arg:ident : $ty:ty),* $(,)? ) $(-> $ret:ty)?
        ) => {
            #[unsafe(no_mangle)]
            pub extern "Rust" fn ${concat(__tt_test_, $name)}(
                ptr: *const ()
                $(, $arg : $ty)*
            ) $(-> $ret)? {
                let tt: &TestHandleConcrete = unsafe { &*(ptr as *const TestHandleConcrete) };
                tt.$name($($arg),*)
            }
        };
    }

    // ---- dispatched methods ----------------------------------------------
    // (Mirror of the `prod` block — keep in sync.)

    // TurboTasksCallApi
    provide_test!(fn dynamic_call(
        native_fn: &'static turbo_tasks::macro_helpers::NativeFunction,
        this: Option<turbo_tasks::RawVc>,
        arg: &mut dyn turbo_tasks::StackDynTaskInputs,
        persistence: turbo_tasks::TaskPersistence,
    ) -> turbo_tasks::RawVc);
    provide_test!(fn native_call(
        native_fn: &'static turbo_tasks::macro_helpers::NativeFunction,
        this: Option<turbo_tasks::RawVc>,
        arg: &mut dyn turbo_tasks::StackDynTaskInputs,
        persistence: turbo_tasks::TaskPersistence,
    ) -> turbo_tasks::RawVc);
    provide_test!(fn trait_call(
        trait_method: &'static turbo_tasks::TraitMethod,
        this: turbo_tasks::RawVc,
        arg: &mut dyn turbo_tasks::StackDynTaskInputs,
        persistence: turbo_tasks::TaskPersistence,
    ) -> turbo_tasks::RawVc);
    provide_test!(fn send_compilation_event(
        event: ::std::sync::Arc<dyn turbo_tasks::message_queue::CompilationEvent>,
    ));
    provide_test!(fn get_task_name(task: turbo_tasks::TaskId) -> ::std::string::String);

    // TurboTasksApi
    provide_test!(fn invalidate(task: turbo_tasks::TaskId));
    provide_test!(fn invalidate_with_reason(
        task: turbo_tasks::TaskId,
        reason: turbo_tasks::util::StaticOrArc<dyn turbo_tasks::InvalidationReason>,
    ));
    provide_test!(fn invalidate_serialization(task: turbo_tasks::TaskId));
    provide_test!(fn try_read_task_output(
        task: turbo_tasks::TaskId,
        options: turbo_tasks::ReadOutputOptions,
    ) -> ::anyhow::Result<::core::result::Result<turbo_tasks::RawVc, turbo_tasks::event::EventListener>>);
    provide_test!(fn try_read_task_cell(
        task: turbo_tasks::TaskId,
        index: turbo_tasks::CellId,
        options: turbo_tasks::ReadCellOptions,
    ) -> ::anyhow::Result<::core::result::Result<turbo_tasks::backend::TypedCellContent, turbo_tasks::event::EventListener>>);
    provide_test!(fn try_read_local_output(
        execution_id: turbo_tasks::ExecutionId,
        local_task_id: turbo_tasks::LocalTaskId,
    ) -> ::anyhow::Result<::core::result::Result<turbo_tasks::RawVc, turbo_tasks::event::EventListener>>);
    provide_test!(fn read_task_collectibles(
        task: turbo_tasks::TaskId,
        trait_id: turbo_tasks::TraitTypeId,
    ) -> turbo_tasks::backend::TaskCollectiblesMap);
    provide_test!(fn emit_collectible(
        trait_type: turbo_tasks::TraitTypeId,
        collectible: turbo_tasks::RawVc,
    ));
    provide_test!(fn unemit_collectible(
        trait_type: turbo_tasks::TraitTypeId,
        collectible: turbo_tasks::RawVc,
        count: u32,
    ));
    provide_test!(fn unemit_collectibles(
        trait_type: turbo_tasks::TraitTypeId,
        collectibles: &turbo_tasks::backend::TaskCollectiblesMap,
    ));
    provide_test!(fn try_read_own_task_cell(
        current_task: turbo_tasks::TaskId,
        index: turbo_tasks::CellId,
    ) -> ::anyhow::Result<turbo_tasks::backend::TypedCellContent>);
    provide_test!(fn read_own_task_cell(
        task: turbo_tasks::TaskId,
        index: turbo_tasks::CellId,
    ) -> ::anyhow::Result<turbo_tasks::backend::TypedCellContent>);
    provide_test!(fn update_own_task_cell(
        task: turbo_tasks::TaskId,
        index: turbo_tasks::CellId,
        content: turbo_tasks::backend::CellContent,
        updated_key_hashes: ::core::option::Option<::smallvec::SmallVec<[u64; 2]>>,
        content_hash: ::core::option::Option<turbo_tasks::backend::CellHash>,
        verification_mode: turbo_tasks::backend::VerificationMode,
    ));
    provide_test!(fn mark_own_task_as_finished(task: turbo_tasks::TaskId));
    provide_test!(fn connect_task(task: turbo_tasks::TaskId));
    provide_test!(fn spawn_detached_for_testing(
        f: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ()> + ::core::marker::Send + 'static>>,
    ));
    provide_test!(fn subscribe_to_compilation_events(
        event_types: ::core::option::Option<::std::vec::Vec<::std::string::String>>,
    ) -> ::tokio::sync::mpsc::Receiver<::std::sync::Arc<dyn turbo_tasks::message_queue::CompilationEvent>>);
    provide_test!(fn is_tracking_dependencies() -> bool);

    // ---- Arc clone / drop -------------------------------------------------

    #[unsafe(no_mangle)]
    pub extern "Rust" fn __tt_test_clone_arc(ptr: *const ()) {
        unsafe {
            Arc::<TestHandleConcrete>::increment_strong_count(ptr as *const TestHandleConcrete)
        }
    }

    #[unsafe(no_mangle)]
    pub extern "Rust" fn __tt_test_drop_arc(ptr: *const ()) {
        unsafe {
            Arc::<TestHandleConcrete>::decrement_strong_count(ptr as *const TestHandleConcrete)
        }
    }

    pub fn from_test(arc: Arc<TestHandleConcrete>) -> TurboTasksHandle {
        let ptr = Arc::into_raw(arc) as *mut ();
        unsafe {
            TurboTasksHandle::from_raw_parts(HandleTag::Test, std::ptr::NonNull::new_unchecked(ptr))
        }
    }
}

#[cfg(feature = "test")]
pub use test_arm::from_test;
