//! Devirtualized dispatch for the turbo-tasks task-local.
//!
//! The task-local that holds the current `TurboTasksApi` implementation
//! has historically been an `Arc<dyn TurboTasksApi>`. The `dyn` is
//! necessary because the prod implementor (`TurboTasks<B>`) is generic over a
//! backend type that the `task_local!` macro cannot name — but the cost
//! is an indirect vtable call on every dispatched method, and rustc
//! currently does not emit the LLVM metadata that `WholeProgramDevirt`
//! needs to inline through trait objects ([rust#68262], [rust#45774]).
//!
//! [rust#68262]: https://github.com/rust-lang/rust/issues/68262
//! [rust#45774]: https://github.com/rust-lang/rust/issues/45774
//!
//! This module replaces the production path's `dyn` with `extern "Rust"`
//! direct calls. Under `lto = "thin"` + `codegen-units = 1` (this
//! workspace's release profile), the static-arm `extern "Rust"` call
//! inlines across the `turbo-tasks` → `turbo-tasks-backend` boundary,
//! collapsing the `match` arm to a direct call to the underlying backend
//! method.
//!
//! ```text
//!  call site (static feature active)        turbo-tasks-backend
//! ┌──────────────────────────┐              ┌─────────────────────────────────────┐
//! │ tt.invalidate(task)       │              │ #[no_mangle] pub extern "Rust" fn   │
//! │   match self {            │              │ __tt_static_invalidate(ptr, task) {  │
//! │     Static(ptr) => ───┐    │              │   let tt: &TurboTasks<…> = …;     │
//! │     Dynamic(arc) => …  │   │              │   tt.invalidate(task)             │
//! │   }                     └─────────────►  │ }                                  │
//! └──────────────────────────┘              └─────────────────────────────────────┘
//! ```
//!
//! For the dynamic arm:
//! ```text
//!  call site
//! ┌──────────────────────────┐
//! │ tt.invalidate(task)       │
//! │   Dynamic(arc) =>         │
//! │     arc.invalidate(task)  │  (normal vtable dispatch on Arc<dyn TurboTasksApi>)
//! └──────────────────────────┘
//! ```
//!
//! ## Feature gating
//!
//! Each variant is feature-gated. The gates exist to avoid linker-symbol
//! breakage in crates that don't pull in the providing crate at link
//! time. Names describe the *dispatch mechanism*, not the intended user:
//! `VcStorage` from `turbo-tasks-testing` uses the dynamic arm because we
//! don't want to wire its concrete type into the static dispatch surface,
//! not because dynamic dispatch is inherently a "test" concept.
//!
//! - **`static_handle`** — activated by `turbo-tasks-backend`'s feature `static_handle` (which the
//!   napi binding opts into). Adds the `Static` variant and the `extern "Rust"` forward
//!   declarations. Any binary that activates `turbo-tasks-backend/static_handle` gets both the
//!   feature on `turbo-tasks` and the `#[no_mangle]` provider definitions, so the externs resolve
//!   cleanly at link time.
//! - **`dynamic_handle`** — activated by `turbo-tasks-testing`'s dep on `turbo-tasks`. Adds the
//!   `Dynamic` variant. Uses normal `Arc<dyn>` vtable dispatch, so there's no linker footgun if the
//!   feature unifies on without the providing crate being linked.
//!
//! With neither feature active, `HandleInner` has a single `Unreachable`
//! variant. This keeps the type inhabited (the `task_local!` declaration
//! requires that) but unconstructible. Crates in this situation
//! (`turbo-esregex`, `turbo-tasks-bytes` lib-tests, …) compile but cannot
//! actually run turbo-tasks code — which is fine because they don't.
//!
//! ## Follow-ups
//!
//! - `task_statistics` is only used by `turbo-tasks-backend/tests/ task_statistics.rs`; if that
//!   test calls it on a concrete `TurboTasks<B>` instead of via the handle, it can move off the
//!   dispatch surface (no extern, no method on `TurboTasksHandle`).
//! - `VcStorage` is the only reason the `dynamic_handle` feature exists. If `turbo-tasks-testing`'s
//!   harness used a real `TurboTasks<B>` with a noop backend, the `Dynamic` variant could be
//!   deleted entirely, collapsing the dispatch to a single `match` arm.

use std::{ptr::NonNull, sync::Arc};

#[cfg(any(feature = "static_handle", feature = "dynamic_handle"))]
use crate::TurboTasksApi;

/// Type-erased reference to a `TurboTasksApi` implementation. See the
/// [module docs](self) for the dispatch design.
pub struct TurboTasksHandle(HandleInner);

enum HandleInner {
    /// Statically-dispatched handle. The pointer is the data pointer of
    /// an `Arc::into_raw(arc)` where `arc: Arc<TurboTasks<B>>` for the
    /// concrete backend type the `__tt_static_*` providers were
    /// generated for. Dispatch goes through `extern "Rust"` symbols
    /// defined in `turbo-tasks-backend`.
    #[cfg(feature = "static_handle")]
    Static(NonNull<()>),
    /// Dynamically-dispatched handle. Normal `Arc<dyn TurboTasksApi>`
    /// vtable dispatch. Used by `VcStorage` and any other harness that
    /// doesn't want to own a real `TurboTasks<B>`. Not on the production
    /// hot path.
    #[cfg(feature = "dynamic_handle")]
    Dynamic(Arc<dyn TurboTasksApi>),
    /// Placeholder variant kept so `HandleInner` stays inhabited when
    /// neither feature is on. Constructing a handle requires a feature;
    /// dispatch on this variant is `unreachable!()`. This lets
    /// `turbo-tasks` compile standalone for crates that merely declare
    /// value types and never construct a handle.
    #[cfg(not(any(feature = "static_handle", feature = "dynamic_handle")))]
    Unreachable,
}

impl std::fmt::Debug for TurboTasksHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            #[cfg(feature = "static_handle")]
            HandleInner::Static(ptr) => f
                .debug_tuple("TurboTasksHandle::Static")
                .field(ptr)
                .finish(),
            #[cfg(feature = "dynamic_handle")]
            HandleInner::Dynamic(_) => f.debug_tuple("TurboTasksHandle::Dynamic").finish(),
            #[cfg(not(any(feature = "static_handle", feature = "dynamic_handle")))]
            HandleInner::Unreachable => f.write_str("TurboTasksHandle::Unreachable"),
        }
    }
}

// Safety: every concrete handle behind a `TurboTasksHandle` is itself
// `Send + Sync` and reference-counted via `Arc`. For the `Static`
// variant, the raw pointer is just an erased `Arc::into_raw`. The
// `Dynamic` variant is `Arc<dyn TurboTasksApi + Send + Sync>` (the trait
// bounds make it `Send + Sync`).
unsafe impl Send for TurboTasksHandle {}
unsafe impl Sync for TurboTasksHandle {}

impl TurboTasksHandle {
    /// Construct a `Static` handle from an `Arc::into_raw` pointer.
    ///
    /// # Safety
    ///
    /// `ptr` must come from `Arc::into_raw(arc)` where `arc` is an
    /// `Arc<TurboTasks<B>>` for the concrete backend the
    /// `__tt_static_*` providers (in `turbo-tasks-backend`) target.
    /// Ownership of one strong refcount transfers into the handle.
    #[cfg(feature = "static_handle")]
    #[inline]
    pub unsafe fn from_static_raw(ptr: NonNull<()>) -> Self {
        Self(HandleInner::Static(ptr))
    }

    /// Stub for `from_static_raw` when the `static_handle` feature is off.
    /// Cannot actually be constructed and called at runtime because the
    /// dispatch paths that produce a static handle are gated on the same
    /// feature; this is kept as a `panic!` body so the `TurboTasks<B>`
    /// inherent `make_handle` (in `manager.rs`) can compile in builds
    /// that don't activate `static_handle`. Linker dead-code elimination
    /// removes both this and its callers from the final binary.
    #[cfg(not(feature = "static_handle"))]
    #[inline]
    pub unsafe fn from_static_raw(_ptr: NonNull<()>) -> Self {
        unreachable!(
            "TurboTasksHandle::from_static_raw called without `static_handle` feature on \
             `turbo-tasks`"
        )
    }

    /// Construct a `Dynamic` handle from any `TurboTasksApi` implementor.
    #[cfg(feature = "dynamic_handle")]
    #[inline]
    pub fn from_dynamic(arc: Arc<dyn TurboTasksApi>) -> Self {
        Self(HandleInner::Dynamic(arc))
    }
}

// =====================================================================
// `extern "Rust"` forward declarations for the static arm.
//
// The bodies are defined `#[no_mangle]` in `turbo-tasks-backend` and
// resolved at link time. Thin LTO inlines them across the crate boundary.
// =====================================================================

/// Generates `unsafe extern "Rust" { fn __tt_static_<name>; }` declarations
/// for one dispatched method. Only emitted when the `static_handle` feature
/// is active — i.e. when `turbo-tasks-backend` is in the dep graph and
/// will provide the matching `#[no_mangle]` definitions.
macro_rules! tt_decl_extern {
    (
        fn $name:ident( $($arg:ident : $ty:ty),* $(,)? ) $(-> $ret:ty)?
    ) => {
        #[cfg(feature = "static_handle")]
        unsafe extern "Rust" {
            fn ${concat(__tt_static_, $name)}(ptr: *const () $(, $arg : $ty)*) $(-> $ret)?;
        }
    };
}

/// Generates an inherent method on `TurboTasksHandle` that dispatches via
/// `match self.0`. The `Static` arm calls the `__tt_static_<name>` extern;
/// the `Dynamic` arm (if present) calls the method on the `Arc<dyn
/// TurboTasksApi>` (vtable dispatch). With no features active, the
/// `Unreachable` arm panics — but you can't construct such a handle, so
/// this is dead code that's kept only to make the type compile.
macro_rules! tt_decl_handle_method {
    (
        fn $name:ident( $($arg:ident : $ty:ty),* $(,)? ) $(-> $ret:ty)?
    ) => {
        impl TurboTasksHandle {
            #[inline]
            #[allow(unused_variables)]
            pub fn $name(&self $(, $arg : $ty)*) $(-> $ret)? {
                match &self.0 {
                    #[cfg(feature = "static_handle")]
                    HandleInner::Static(ptr) => unsafe {
                        ${concat(__tt_static_, $name)}(ptr.as_ptr() $(, $arg)*)
                    },
                    // The `TurboTasksApi` super-trait bound on the Arc<dyn>
                    // makes every dispatched method (whether defined on
                    // `TurboTasksApi` itself or its supertrait
                    // `TurboTasksCallApi`) callable via method-call syntax.
                    #[cfg(feature = "dynamic_handle")]
                    HandleInner::Dynamic(arc) => arc.$name($($arg),*),
                    #[cfg(not(any(feature = "static_handle", feature = "dynamic_handle")))]
                    HandleInner::Unreachable => unreachable!(
                        "TurboTasksHandle dispatch with neither `static_handle` nor `dynamic_handle` \
                         feature active on `turbo-tasks`"
                    ),
                }
            }
        }
    };
}

// ---- dispatched methods -------------------------------------------------
//
// Keep this list in sync with the matching provider implementations in
// `turbo-tasks-backend/src/handle_providers.rs`. The list is duplicated;
// a missing static provider surfaces as a link error.

// `TurboTasksCallApi` methods.
tt_decl_extern!(fn dynamic_call(
    native_fn: &'static crate::native_function::NativeFunction,
    this: Option<crate::RawVc>,
    arg: &mut dyn crate::StackDynTaskInputs,
    persistence: crate::TaskPersistence,
) -> crate::RawVc);
tt_decl_handle_method!(fn dynamic_call(
    native_fn: &'static crate::native_function::NativeFunction,
    this: Option<crate::RawVc>,
    arg: &mut dyn crate::StackDynTaskInputs,
    persistence: crate::TaskPersistence,
) -> crate::RawVc);

tt_decl_extern!(fn native_call(
    native_fn: &'static crate::native_function::NativeFunction,
    this: Option<crate::RawVc>,
    arg: &mut dyn crate::StackDynTaskInputs,
    persistence: crate::TaskPersistence,
) -> crate::RawVc);
tt_decl_handle_method!(fn native_call(
    native_fn: &'static crate::native_function::NativeFunction,
    this: Option<crate::RawVc>,
    arg: &mut dyn crate::StackDynTaskInputs,
    persistence: crate::TaskPersistence,
) -> crate::RawVc);

tt_decl_extern!(fn trait_call(
    trait_method: &'static crate::TraitMethod,
    this: crate::RawVc,
    arg: &mut dyn crate::StackDynTaskInputs,
    persistence: crate::TaskPersistence,
) -> crate::RawVc);
tt_decl_handle_method!(fn trait_call(
    trait_method: &'static crate::TraitMethod,
    this: crate::RawVc,
    arg: &mut dyn crate::StackDynTaskInputs,
    persistence: crate::TaskPersistence,
) -> crate::RawVc);

tt_decl_extern!(fn send_compilation_event(
    event: ::std::sync::Arc<dyn crate::message_queue::CompilationEvent>,
));
tt_decl_handle_method!(fn send_compilation_event(
    event: ::std::sync::Arc<dyn crate::message_queue::CompilationEvent>,
));

tt_decl_extern!(fn get_task_name(task: crate::TaskId) -> ::std::string::String);
tt_decl_handle_method!(fn get_task_name(task: crate::TaskId) -> ::std::string::String);

tt_decl_extern!(fn run(
    future: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::anyhow::Result<()>> + ::core::marker::Send + 'static>>,
) -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::core::result::Result<(), crate::backend::TurboTasksExecutionError>> + ::core::marker::Send>>);
tt_decl_handle_method!(fn run(
    future: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::anyhow::Result<()>> + ::core::marker::Send + 'static>>,
) -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::core::result::Result<(), crate::backend::TurboTasksExecutionError>> + ::core::marker::Send>>);

tt_decl_extern!(fn run_once(
    future: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::anyhow::Result<()>> + ::core::marker::Send + 'static>>,
) -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::anyhow::Result<()>> + ::core::marker::Send>>);
tt_decl_handle_method!(fn run_once(
    future: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::anyhow::Result<()>> + ::core::marker::Send + 'static>>,
) -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::anyhow::Result<()>> + ::core::marker::Send>>);

tt_decl_extern!(fn run_once_with_reason(
    reason: crate::util::StaticOrArc<dyn crate::InvalidationReason>,
    future: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::anyhow::Result<()>> + ::core::marker::Send + 'static>>,
) -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::anyhow::Result<()>> + ::core::marker::Send>>);
tt_decl_handle_method!(fn run_once_with_reason(
    reason: crate::util::StaticOrArc<dyn crate::InvalidationReason>,
    future: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::anyhow::Result<()>> + ::core::marker::Send + 'static>>,
) -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::anyhow::Result<()>> + ::core::marker::Send>>);

tt_decl_extern!(fn start_once_process(
    future: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ()> + ::core::marker::Send + 'static>>,
));
tt_decl_handle_method!(fn start_once_process(
    future: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ()> + ::core::marker::Send + 'static>>,
));

tt_decl_extern!(fn stop_and_wait()
    -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ()> + ::core::marker::Send>>);
tt_decl_handle_method!(fn stop_and_wait()
    -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ()> + ::core::marker::Send>>);

// `TurboTasksApi` methods (inherits TurboTasksCallApi above).
tt_decl_extern!(fn invalidate(task: crate::TaskId));
tt_decl_handle_method!(fn invalidate(task: crate::TaskId));

tt_decl_extern!(fn invalidate_with_reason(
    task: crate::TaskId,
    reason: crate::util::StaticOrArc<dyn crate::InvalidationReason>,
));
tt_decl_handle_method!(fn invalidate_with_reason(
    task: crate::TaskId,
    reason: crate::util::StaticOrArc<dyn crate::InvalidationReason>,
));

tt_decl_extern!(fn invalidate_serialization(task: crate::TaskId));
tt_decl_handle_method!(fn invalidate_serialization(task: crate::TaskId));

tt_decl_extern!(fn try_read_task_output(
    task: crate::TaskId,
    options: crate::ReadOutputOptions,
) -> ::anyhow::Result<::core::result::Result<crate::RawVc, crate::event::EventListener>>);
tt_decl_handle_method!(fn try_read_task_output(
    task: crate::TaskId,
    options: crate::ReadOutputOptions,
) -> ::anyhow::Result<::core::result::Result<crate::RawVc, crate::event::EventListener>>);

tt_decl_extern!(fn try_read_task_cell(
    task: crate::TaskId,
    index: crate::CellId,
    options: crate::ReadCellOptions,
) -> ::anyhow::Result<::core::result::Result<crate::backend::TypedCellContent, crate::event::EventListener>>);
tt_decl_handle_method!(fn try_read_task_cell(
    task: crate::TaskId,
    index: crate::CellId,
    options: crate::ReadCellOptions,
) -> ::anyhow::Result<::core::result::Result<crate::backend::TypedCellContent, crate::event::EventListener>>);

tt_decl_extern!(fn try_read_local_output(
    execution_id: crate::ExecutionId,
    local_task_id: crate::LocalTaskId,
) -> ::anyhow::Result<::core::result::Result<crate::RawVc, crate::event::EventListener>>);
tt_decl_handle_method!(fn try_read_local_output(
    execution_id: crate::ExecutionId,
    local_task_id: crate::LocalTaskId,
) -> ::anyhow::Result<::core::result::Result<crate::RawVc, crate::event::EventListener>>);

tt_decl_extern!(fn read_task_collectibles(
    task: crate::TaskId,
    trait_id: crate::TraitTypeId,
) -> crate::backend::TaskCollectiblesMap);
tt_decl_handle_method!(fn read_task_collectibles(
    task: crate::TaskId,
    trait_id: crate::TraitTypeId,
) -> crate::backend::TaskCollectiblesMap);

tt_decl_extern!(fn emit_collectible(
    trait_type: crate::TraitTypeId,
    collectible: crate::RawVc,
));
tt_decl_handle_method!(fn emit_collectible(
    trait_type: crate::TraitTypeId,
    collectible: crate::RawVc,
));

tt_decl_extern!(fn unemit_collectible(
    trait_type: crate::TraitTypeId,
    collectible: crate::RawVc,
    count: u32,
));
tt_decl_handle_method!(fn unemit_collectible(
    trait_type: crate::TraitTypeId,
    collectible: crate::RawVc,
    count: u32,
));

tt_decl_extern!(fn unemit_collectibles(
    trait_type: crate::TraitTypeId,
    collectibles: &crate::backend::TaskCollectiblesMap,
));
tt_decl_handle_method!(fn unemit_collectibles(
    trait_type: crate::TraitTypeId,
    collectibles: &crate::backend::TaskCollectiblesMap,
));

tt_decl_extern!(fn try_read_own_task_cell(
    current_task: crate::TaskId,
    index: crate::CellId,
) -> ::anyhow::Result<crate::backend::TypedCellContent>);
tt_decl_handle_method!(fn try_read_own_task_cell(
    current_task: crate::TaskId,
    index: crate::CellId,
) -> ::anyhow::Result<crate::backend::TypedCellContent>);

tt_decl_extern!(fn read_own_task_cell(
    task: crate::TaskId,
    index: crate::CellId,
) -> ::anyhow::Result<crate::backend::TypedCellContent>);
tt_decl_handle_method!(fn read_own_task_cell(
    task: crate::TaskId,
    index: crate::CellId,
) -> ::anyhow::Result<crate::backend::TypedCellContent>);

tt_decl_extern!(fn update_own_task_cell(
    task: crate::TaskId,
    index: crate::CellId,
    content: crate::backend::CellContent,
    updated_key_hashes: ::core::option::Option<::smallvec::SmallVec<[u64; 2]>>,
    content_hash: ::core::option::Option<crate::backend::CellHash>,
    verification_mode: crate::backend::VerificationMode,
));
tt_decl_handle_method!(fn update_own_task_cell(
    task: crate::TaskId,
    index: crate::CellId,
    content: crate::backend::CellContent,
    updated_key_hashes: ::core::option::Option<::smallvec::SmallVec<[u64; 2]>>,
    content_hash: ::core::option::Option<crate::backend::CellHash>,
    verification_mode: crate::backend::VerificationMode,
));

tt_decl_extern!(fn mark_own_task_as_finished(task: crate::TaskId));
tt_decl_handle_method!(fn mark_own_task_as_finished(task: crate::TaskId));

tt_decl_extern!(fn connect_task(task: crate::TaskId));
tt_decl_handle_method!(fn connect_task(task: crate::TaskId));

tt_decl_extern!(fn spawn_detached_for_testing(
    f: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ()> + ::core::marker::Send + 'static>>,
));
tt_decl_handle_method!(fn spawn_detached_for_testing(
    f: ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ()> + ::core::marker::Send + 'static>>,
));

tt_decl_extern!(fn subscribe_to_compilation_events(
    event_types: ::core::option::Option<::std::vec::Vec<::std::string::String>>,
) -> ::tokio::sync::mpsc::Receiver<::std::sync::Arc<dyn crate::message_queue::CompilationEvent>>);
tt_decl_handle_method!(fn subscribe_to_compilation_events(
    event_types: ::core::option::Option<::std::vec::Vec<::std::string::String>>,
) -> ::tokio::sync::mpsc::Receiver<::std::sync::Arc<dyn crate::message_queue::CompilationEvent>>);

tt_decl_extern!(fn is_tracking_dependencies() -> bool);
tt_decl_handle_method!(fn is_tracking_dependencies() -> bool);

// `task_statistics` returns `&TaskStatisticsApi` borrowed from `&self`.
// The macro can't express the lifetime relationship through a `*const ()`
// receiver, so the static provider returns `*const TaskStatisticsApi` and
// the handle wrapper re-binds the lifetime to `&self`. The dynamic arm
// just calls the trait method on the Arc.
#[cfg(feature = "static_handle")]
unsafe extern "Rust" {
    fn __tt_static_task_statistics(
        ptr: *const (),
    ) -> *const crate::task_statistics::TaskStatisticsApi;
}

impl TurboTasksHandle {
    #[inline]
    pub fn task_statistics(&self) -> &crate::task_statistics::TaskStatisticsApi {
        match &self.0 {
            #[cfg(feature = "static_handle")]
            HandleInner::Static(ptr) => {
                // SAFETY: the provider returns a pointer to a
                // `TaskStatisticsApi` owned by the underlying
                // `TurboTasks<B>`, which this handle keeps alive via its
                // Arc. The returned reference is bound to `&self`.
                let p = unsafe { __tt_static_task_statistics(ptr.as_ptr()) };
                unsafe { &*p }
            }
            #[cfg(feature = "dynamic_handle")]
            HandleInner::Dynamic(arc) => arc.task_statistics(),
            #[cfg(not(any(feature = "static_handle", feature = "dynamic_handle")))]
            HandleInner::Unreachable => unreachable!(),
        }
    }
}

// =====================================================================
// Clone / Drop — Arc refcounting through extern symbols (prod) or
// the std `Arc` impls (test).
// =====================================================================

#[cfg(feature = "static_handle")]
unsafe extern "Rust" {
    fn __tt_static_clone_arc(ptr: *const ());
    fn __tt_static_drop_arc(ptr: *const ());

    // Weak-handle support for the static arm. Each provides:
    //   downgrade : *const Arc<T> -> *const Weak<T> (creates a fresh
    //               Weak; caller owns the returned weak).
    //   upgrade   : *const Weak<T> -> *const Arc<T> (returns null if
    //               the Arc is gone; otherwise transfers one strong
    //               refcount).
    //   clone_weak: bumps the weak refcount.
    //   drop_weak : drops the weak refcount.
    fn __tt_static_downgrade(arc_ptr: *const ()) -> *const ();
    fn __tt_static_upgrade(weak_ptr: *const ()) -> *const ();
    fn __tt_static_clone_weak(weak_ptr: *const ());
    fn __tt_static_drop_weak(weak_ptr: *const ());
}

impl Clone for TurboTasksHandle {
    #[inline]
    fn clone(&self) -> Self {
        match &self.0 {
            #[cfg(feature = "static_handle")]
            HandleInner::Static(ptr) => {
                unsafe { __tt_static_clone_arc(ptr.as_ptr()) }
                Self(HandleInner::Static(*ptr))
            }
            #[cfg(feature = "dynamic_handle")]
            HandleInner::Dynamic(arc) => Self(HandleInner::Dynamic(Arc::clone(arc))),
            #[cfg(not(any(feature = "static_handle", feature = "dynamic_handle")))]
            HandleInner::Unreachable => unreachable!(),
        }
    }
}

impl Drop for TurboTasksHandle {
    #[inline]
    fn drop(&mut self) {
        match &self.0 {
            #[cfg(feature = "static_handle")]
            HandleInner::Static(ptr) => unsafe { __tt_static_drop_arc(ptr.as_ptr()) },
            // Dynamic variant: the `Arc` field drops itself naturally.
            #[cfg(feature = "dynamic_handle")]
            HandleInner::Dynamic(_) => {}
            #[cfg(not(any(feature = "static_handle", feature = "dynamic_handle")))]
            HandleInner::Unreachable => {}
        }
    }
}

impl TurboTasksHandle {
    /// Downgrades to a weak handle, equivalent to `Arc::downgrade`.
    #[inline]
    pub fn downgrade(&self) -> TurboTasksWeakHandle {
        match &self.0 {
            #[cfg(feature = "static_handle")]
            HandleInner::Static(ptr) => {
                let weak_ptr = unsafe { __tt_static_downgrade(ptr.as_ptr()) };
                // `Weak::into_raw` always produces a valid (non-null)
                // pointer even when the strong count is zero.
                TurboTasksWeakHandle(WeakInner::Static(unsafe {
                    NonNull::new_unchecked(weak_ptr as *mut ())
                }))
            }
            #[cfg(feature = "dynamic_handle")]
            HandleInner::Dynamic(arc) => {
                TurboTasksWeakHandle(WeakInner::Dynamic(Arc::downgrade(arc)))
            }
            #[cfg(not(any(feature = "static_handle", feature = "dynamic_handle")))]
            HandleInner::Unreachable => unreachable!(),
        }
    }
}

// =====================================================================
// Weak-handle dispatch.
//
// Used by long-lived non-task contexts (e.g. the filesystem watcher in
// `turbo-tasks-fs`) that need to reach back into TurboTasks without
// keeping it alive.
// =====================================================================

/// Weak counterpart to [`TurboTasksHandle`]. Constructed via
/// [`TurboTasksHandle::downgrade`]; upgraded via
/// [`TurboTasksWeakHandle::upgrade`].
pub struct TurboTasksWeakHandle(WeakInner);

enum WeakInner {
    #[cfg(feature = "static_handle")]
    Static(NonNull<()>),
    #[cfg(feature = "dynamic_handle")]
    Dynamic(std::sync::Weak<dyn TurboTasksApi>),
    #[cfg(not(any(feature = "static_handle", feature = "dynamic_handle")))]
    Unreachable,
}

impl std::fmt::Debug for TurboTasksWeakHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            #[cfg(feature = "static_handle")]
            WeakInner::Static(ptr) => f
                .debug_tuple("TurboTasksWeakHandle::Static")
                .field(ptr)
                .finish(),
            #[cfg(feature = "dynamic_handle")]
            WeakInner::Dynamic(_) => f.debug_tuple("TurboTasksWeakHandle::Dynamic").finish(),
            #[cfg(not(any(feature = "static_handle", feature = "dynamic_handle")))]
            WeakInner::Unreachable => f.write_str("TurboTasksWeakHandle::Unreachable"),
        }
    }
}

// Safety: same reasoning as for `TurboTasksHandle`.
unsafe impl Send for TurboTasksWeakHandle {}
unsafe impl Sync for TurboTasksWeakHandle {}

impl TurboTasksWeakHandle {
    /// Tries to recover a strong handle. Returns `None` if the underlying
    /// concrete handle has been dropped.
    #[inline]
    pub fn upgrade(&self) -> Option<TurboTasksHandle> {
        match &self.0 {
            #[cfg(feature = "static_handle")]
            WeakInner::Static(ptr) => {
                let strong = unsafe { __tt_static_upgrade(ptr.as_ptr()) };
                let strong = NonNull::new(strong as *mut ())?;
                // SAFETY: provider returned a valid `Arc::into_raw`
                // pointer for the static concrete type, transferring one
                // strong refcount.
                Some(unsafe { TurboTasksHandle::from_static_raw(strong) })
            }
            #[cfg(feature = "dynamic_handle")]
            WeakInner::Dynamic(weak) => weak.upgrade().map(TurboTasksHandle::from_dynamic),
            #[cfg(not(any(feature = "static_handle", feature = "dynamic_handle")))]
            WeakInner::Unreachable => unreachable!(),
        }
    }
}

impl Clone for TurboTasksWeakHandle {
    #[inline]
    fn clone(&self) -> Self {
        match &self.0 {
            #[cfg(feature = "static_handle")]
            WeakInner::Static(ptr) => {
                unsafe { __tt_static_clone_weak(ptr.as_ptr()) }
                Self(WeakInner::Static(*ptr))
            }
            #[cfg(feature = "dynamic_handle")]
            WeakInner::Dynamic(weak) => Self(WeakInner::Dynamic(weak.clone())),
            #[cfg(not(any(feature = "static_handle", feature = "dynamic_handle")))]
            WeakInner::Unreachable => unreachable!(),
        }
    }
}

impl Drop for TurboTasksWeakHandle {
    #[inline]
    fn drop(&mut self) {
        match &self.0 {
            #[cfg(feature = "static_handle")]
            WeakInner::Static(ptr) => unsafe { __tt_static_drop_weak(ptr.as_ptr()) },
            #[cfg(feature = "dynamic_handle")]
            WeakInner::Dynamic(_) => {}
            #[cfg(not(any(feature = "static_handle", feature = "dynamic_handle")))]
            WeakInner::Unreachable => {}
        }
    }
}
