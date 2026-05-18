//! `#[no_mangle] pub extern "Rust" fn __tt_prod_*` providers for the
//! production arm of the `TurboTasksHandle` dispatch.
//!
//! The forward declarations live in `turbo_tasks::handle` and are gated by
//! the `prod_handle` Cargo feature on `turbo-tasks`. `turbo-tasks-backend`
//! activates that feature in its dep entry, so these `#[no_mangle]`
//! symbols are linked into any binary that pulls in `turbo-tasks-backend`.
//!
//! Each provider:
//! 1. Casts the opaque `*const ()` receiver back to `&ProdHandleConcrete` (the production handle
//!    type — `TurboTasks<TurboTasksBackend<…>>`).
//! 2. Calls the trait method on the concrete type.
//!
//! Under thin LTO + `codegen-units = 1`, every step inlines into the
//! caller and the dispatch shape is `match tag => direct call` with no
//! indirect calls. See `turbo_tasks::handle` for the experiment that
//! verified this.

use std::sync::Arc;

use either::Either;
use turbo_tasks::{TurboTasksApi as _, TurboTasksCallApi as _};

use crate::{NoopBackingStorage, TurboBackingStorage, TurboTasksBackend};

/// The concrete prod handle type — matches what `next-napi-bindings` uses
/// (one of `Either<TurboBackingStorage, NoopBackingStorage>`).
///
/// If a future binary needs a different `Backend`/storage combination,
/// this provider crate would need a parallel module. Today there is
/// exactly one prod handle type so we hardcode it.
pub type ProdHandleConcrete =
    turbo_tasks::TurboTasks<TurboTasksBackend<Either<TurboBackingStorage, NoopBackingStorage>>>;

/// Generates `#[no_mangle] pub extern "Rust" fn __tt_prod_<name>(...)`
/// for a single dispatched method, dispatched via method call syntax.
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

/// Same as `provide_prod!`, but forces UFCS dispatch to a specific
/// trait. Used for methods (`run`, `run_once`, `run_once_with_reason`,
/// `start_once_process`, `stop_and_wait`) where the concrete type has
/// an inherent method with the same name but a different return type —
/// without UFCS, the inherent method wins and the macro fails type
/// checking.
macro_rules! provide_prod_trait {
    (
        $trait:path,
        fn $name:ident( $($arg:ident : $ty:ty),* $(,)? ) $(-> $ret:ty)?
    ) => {
        #[unsafe(no_mangle)]
        pub extern "Rust" fn ${concat(__tt_prod_, $name)}(
            ptr: *const ()
            $(, $arg : $ty)*
        ) $(-> $ret)? {
            let tt: &ProdHandleConcrete = unsafe { &*(ptr as *const ProdHandleConcrete) };
            <ProdHandleConcrete as $trait>::$name(tt $(, $arg)*)
        }
    };
}

// ---- dispatched methods ---------------------------------------------------
//
// Keep this list in sync with the matching `tt_decl_extern!` /
// `tt_decl_handle_method!` invocations in
// `turbopack/crates/turbo-tasks/src/handle.rs`.

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

provide_prod_trait!(turbo_tasks::TurboTasksCallApi, fn run(
    future: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::anyhow::Result<()>> + ::core::marker::Send + 'static>>,
) -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::core::result::Result<(), turbo_tasks::backend::TurboTasksExecutionError>> + ::core::marker::Send>>);
provide_prod_trait!(turbo_tasks::TurboTasksCallApi, fn run_once(
    future: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::anyhow::Result<()>> + ::core::marker::Send + 'static>>,
) -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::anyhow::Result<()>> + ::core::marker::Send>>);
provide_prod_trait!(turbo_tasks::TurboTasksCallApi, fn run_once_with_reason(
    reason: turbo_tasks::util::StaticOrArc<dyn turbo_tasks::InvalidationReason>,
    future: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::anyhow::Result<()>> + ::core::marker::Send + 'static>>,
) -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::anyhow::Result<()>> + ::core::marker::Send>>);
provide_prod_trait!(turbo_tasks::TurboTasksCallApi, fn start_once_process(
    future: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ()> + ::core::marker::Send + 'static>>,
));
provide_prod_trait!(turbo_tasks::TurboTasksApi, fn stop_and_wait() -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ()> + ::core::marker::Send>>);

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

// `task_statistics` is special: the trait method returns
// `&TaskStatisticsApi` borrowed from `&self`, but extern "Rust" can't
// carry that lifetime through a `*const ()` receiver. The provider
// returns a raw pointer; the handle wrapper in `turbo-tasks` re-binds
// the lifetime to `&self`. This is sound because the underlying Arc
// (held by the handle) keeps the `TaskStatisticsApi` alive.
#[unsafe(no_mangle)]
pub extern "Rust" fn __tt_prod_task_statistics(
    ptr: *const (),
) -> *const turbo_tasks::task_statistics::TaskStatisticsApi {
    let tt: &ProdHandleConcrete = unsafe { &*(ptr as *const ProdHandleConcrete) };
    tt.task_statistics() as *const _
}

// ---- Arc clone / drop -----------------------------------------------------

#[unsafe(no_mangle)]
pub extern "Rust" fn __tt_prod_clone_arc(ptr: *const ()) {
    // Bump the refcount of the Arc whose data pointer is `ptr`. The caller
    // (`<TurboTasksHandle as Clone>::clone`) is responsible for reusing
    // the same `ptr` value in the new handle, so we don't need to return
    // anything.
    unsafe { Arc::<ProdHandleConcrete>::increment_strong_count(ptr as *const ProdHandleConcrete) }
}

#[unsafe(no_mangle)]
pub extern "Rust" fn __tt_prod_drop_arc(ptr: *const ()) {
    // Decrement the refcount; runs the destructor when it reaches zero.
    unsafe { Arc::<ProdHandleConcrete>::decrement_strong_count(ptr as *const ProdHandleConcrete) }
}

// ---- Weak refcount providers ---------------------------------------------

#[unsafe(no_mangle)]
pub extern "Rust" fn __tt_prod_downgrade(arc_ptr: *const ()) -> *const () {
    // Reconstitute the Arc transiently to call `downgrade`, then leak the
    // Arc back so its refcount is unchanged. The Weak we produce owns its
    // own weak refcount.
    let arc = unsafe { Arc::from_raw(arc_ptr as *const ProdHandleConcrete) };
    let weak = Arc::downgrade(&arc);
    ::std::mem::forget(arc);
    ::std::sync::Weak::into_raw(weak) as *const ()
}

#[unsafe(no_mangle)]
pub extern "Rust" fn __tt_prod_upgrade(weak_ptr: *const ()) -> *const () {
    // Reconstitute the Weak transiently to attempt upgrade, then leak it
    // back so its refcount is unchanged.
    let weak = unsafe { ::std::sync::Weak::from_raw(weak_ptr as *const ProdHandleConcrete) };
    let maybe_arc = weak.upgrade();
    ::std::mem::forget(weak);
    match maybe_arc {
        Some(arc) => Arc::into_raw(arc) as *const (),
        None => ::std::ptr::null(),
    }
}

#[unsafe(no_mangle)]
pub extern "Rust" fn __tt_prod_clone_weak(weak_ptr: *const ()) {
    // `Weak` has no `increment_weak_count` API, so we round-trip through
    // `Weak::clone` and leak both copies.
    let weak = unsafe { ::std::sync::Weak::from_raw(weak_ptr as *const ProdHandleConcrete) };
    let cloned = weak.clone();
    ::std::mem::forget(weak);
    ::std::mem::forget(cloned);
}

#[unsafe(no_mangle)]
pub extern "Rust" fn __tt_prod_drop_weak(weak_ptr: *const ()) {
    drop(unsafe { ::std::sync::Weak::from_raw(weak_ptr as *const ProdHandleConcrete) });
}
