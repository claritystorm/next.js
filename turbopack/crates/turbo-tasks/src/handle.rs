//! Tagged-pointer dispatch for the turbo-tasks task-local.
//!
//! The task-local that holds the current `TurboTasksApi` implementation has
//! historically been an `Arc<dyn TurboTasksApi>`. The `dyn` is necessary
//! because the prod handle (`TurboTasks<B>`) is generic over a backend
//! type that the `task_local!` macro cannot name — but the cost is an
//! indirect vtable call on every dispatched method, and rustc currently does
//! not emit the LLVM metadata that `WholeProgramDevirt` needs to inline
//! through trait objects ([rust#68262], [rust#45774]).
//!
//! [rust#68262]: https://github.com/rust-lang/rust/issues/68262
//! [rust#45774]: https://github.com/rust-lang/rust/issues/45774
//!
//! This module replaces the `dyn` indirection with a tagged pointer that
//! dispatches through `extern "Rust"` forward declarations:
//!
//! ```text
//!  call site                                provider crate
//! ┌──────────────────────────┐              ┌─────────────────────────────────────┐
//! │ tt.invalidate(task) ─────┼──────────────► #[no_mangle] pub extern "Rust" fn   │
//! │ match self.tag { … }      │              │ __tt_prod_invalidate(ptr, task) {  │
//! │                          │              │   let tt: &TurboTasks<…> = …;      │
//! │                          │              │   tt.invalidate(task)              │
//! │                          │              │ }                                  │
//! └──────────────────────────┘              └─────────────────────────────────────┘
//! ```
//!
//! Under `lto = "thin"` + `codegen-units = 1` (this workspace's release
//! profile), the linker fully inlines the `extern "Rust"` call into the
//! caller. The `match` over a one-arm enum collapses entirely; over a
//! two-arm enum it compiles to `cmp + b.ne + direct call` and LLVM can
//! sometimes fuse the arms further.
//!
//! ## Feature gating
//!
//! Two Cargo features on `turbo-tasks` control which dispatch arms exist:
//!
//! - `prod_handle` — activated by `turbo-tasks-backend` (which owns the concrete production handle
//!   type and emits the `__tt_prod_*` providers). Pulled in transitively by anything that links the
//!   backend, including the napi binding.
//! - `test_handle` — activated by `turbo-tasks-testing` (which owns `VcStorage` and emits the
//!   `__tt_test_*` providers).
//!
//! A pure-prod binary (e.g. the napi binding) sees `HandleTag` with only
//! the `Prod` variant and the `match` in each forwarder collapses to a
//! direct call. A workspace test build sees both variants and gets the
//! two-arm match. Features unify across the dep graph; consumers do not
//! enable the features directly.

use std::ptr::NonNull;

/// Identifier for which concrete implementation a [`TurboTasksHandle`] points
/// at. Used as the tag in the tagged-pointer dispatch.
///
/// Variants are feature-gated. A pure-prod build (only `prod_handle`
/// active) has only the `Prod` variant, which makes every dispatch
/// `match` a one-arm collapse — LLVM emits a direct call to the
/// `__tt_prod_*` symbol with no comparison. The two-arm case (both
/// features active, e.g. workspace tests) compiles to `cmp + b.ne +
/// direct call`.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HandleTag {
    /// `TurboTasks<TurboTasksBackend<…>>` — the production handle used by
    /// `next-napi-bindings`, benches, etc.
    #[cfg(feature = "prod_handle")]
    Prod = 0,
    /// `VcStorage` — the test-only handle used by `turbo-tasks-testing`.
    #[cfg(feature = "test_handle")]
    Test = 1,
}

/// A type-erased reference to a concrete `TurboTasksApi` implementation.
///
/// Logically equivalent to an `Arc<dyn TurboTasksApi>`: the pointer is the
/// raw `Arc::into_raw(...)` of the concrete handle, the tag tells the
/// dispatch which provider to call.
///
/// `Clone` and `Drop` route through `__tt_<arm>_clone_arc` /
/// `__tt_<arm>_drop_arc` so refcounting stays correct.
#[derive(Debug)]
pub struct TurboTasksHandle {
    tag: HandleTag,
    /// Points at the inner of an `Arc<ConcreteHandle>` owned via
    /// `Arc::into_raw`. The lifetime is managed by `Clone` / `Drop`
    /// dispatching through the provider crate.
    ptr: NonNull<()>,
}

// Safety: the underlying concrete handle types (`TurboTasks<…>` and
// `VcStorage`) are themselves `Send + Sync` and are reference-counted via
// `Arc`. The raw pointer is just an erased `Arc::into_raw` result; it does
// not introduce additional aliasing beyond what the Arc allowed.
unsafe impl Send for TurboTasksHandle {}
unsafe impl Sync for TurboTasksHandle {}

impl TurboTasksHandle {
    /// Constructs a handle from raw parts. Intended to be called only by
    /// `turbo-tasks-handle`'s `from_prod` / `from_test` constructors, which
    /// own the safety contract that `ptr` is a valid `Arc::into_raw` pointer
    /// for the concrete type associated with `tag`.
    ///
    /// # Safety
    ///
    /// `ptr` must be a pointer obtained from `Arc::into_raw` on the concrete
    /// handle type whose tag matches `tag`. Ownership of the refcount
    /// transfers into the new `TurboTasksHandle`.
    #[inline]
    pub unsafe fn from_raw_parts(tag: HandleTag, ptr: NonNull<()>) -> Self {
        Self { tag, ptr }
    }

    /// The tag indicating which concrete implementation this handle points
    /// at. Exposed for tests and diagnostics; the dispatch macro handles
    /// the normal case.
    #[inline]
    pub fn tag(&self) -> HandleTag {
        self.tag
    }

    /// Raw pointer access, intended only for the dispatch macro and for
    /// `turbo-tasks-handle`. Callers must respect the tag to interpret it.
    #[inline]
    pub fn raw_ptr(&self) -> *const () {
        self.ptr.as_ptr()
    }
}

// =====================================================================
// `extern "Rust"` forward declarations.
//
// Each dispatched `TurboTasksApi` method has two extern symbols — one per
// arm. The bodies are defined in `turbo-tasks-handle` and resolved at link
// time. Thin LTO inlines them.
// =====================================================================

/// Generates feature-gated `unsafe extern "Rust" { fn __tt_<arm>_<name>; }`
/// declarations. With only `prod_handle` active, only the prod decl is
/// emitted; with only `test_handle`, only the test decl. With both, both.
macro_rules! tt_decl_extern {
    (
        fn $name:ident( $($arg:ident : $ty:ty),* $(,)? ) $(-> $ret:ty)?
    ) => {
        unsafe extern "Rust" {
            #[cfg(feature = "prod_handle")]
            fn ${concat(__tt_prod_, $name)}(ptr: *const () $(, $arg : $ty)*) $(-> $ret)?;
            #[cfg(feature = "test_handle")]
            fn ${concat(__tt_test_, $name)}(ptr: *const () $(, $arg : $ty)*) $(-> $ret)?;
        }
    };
}

/// Generates an inherent method on `TurboTasksHandle` that dispatches over
/// `match self.tag` to the corresponding `extern "Rust"` symbol. The match
/// arms are feature-gated alongside the [`HandleTag`] variants and the
/// extern decls, so single-variant builds get a one-arm match that LLVM
/// collapses to a direct call.
macro_rules! tt_decl_handle_method {
    (
        fn $name:ident( $($arg:ident : $ty:ty),* $(,)? ) $(-> $ret:ty)?
    ) => {
        impl TurboTasksHandle {
            #[inline]
            pub fn $name(&self $(, $arg : $ty)*) $(-> $ret)? {
                match self.tag {
                    #[cfg(feature = "prod_handle")]
                    HandleTag::Prod => unsafe {
                        ${concat(__tt_prod_, $name)}(self.ptr.as_ptr() $(, $arg)*)
                    },
                    #[cfg(feature = "test_handle")]
                    HandleTag::Test => unsafe {
                        ${concat(__tt_test_, $name)}(self.ptr.as_ptr() $(, $arg)*)
                    },
                }
            }
        }
    };
}

// ---- dispatched methods -------------------------------------------------
//
// Add new entries here when adding a method to the dispatch surface. Each
// entry must also be implemented in `turbo-tasks-handle`'s provider macro
// (which currently lists them explicitly — a future cleanup could share
// this list via a callback macro, but for now duplication is the cost of
// keeping both places easy to read).

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

// `run`, `run_once`, `run_once_with_reason`, `start_once_process`, and
// `stop_and_wait` need to be on the dispatch surface because the test
// harness in `turbo-tasks-testing` constructs a type-erased
// `TestInstance.tt: TurboTasksHandle` and passes it to the free
// `turbo_tasks::run_once` / `turbo_tasks::run` helpers. Without these on
// the handle, we'd have to either expose the concrete backend type
// through `TestInstance` (cascades into `Registration`) or duplicate the
// helpers per arm. Putting them on the dispatch surface is one extern
// symbol per method per arm — cheap.
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
// receiver, so the providers return `*const TaskStatisticsApi` and the
// handle wrapper re-binds the lifetime to `&self`.
unsafe extern "Rust" {
    #[cfg(feature = "prod_handle")]
    fn __tt_prod_task_statistics(
        ptr: *const (),
    ) -> *const crate::task_statistics::TaskStatisticsApi;
    #[cfg(feature = "test_handle")]
    fn __tt_test_task_statistics(
        ptr: *const (),
    ) -> *const crate::task_statistics::TaskStatisticsApi;
}

impl TurboTasksHandle {
    #[inline]
    pub fn task_statistics(&self) -> &crate::task_statistics::TaskStatisticsApi {
        // SAFETY: the provider returns a pointer to a `TaskStatisticsApi`
        // owned by the underlying `TurboTasks<B>` / `VcStorage`, which the
        // handle holds alive via its Arc. The returned reference is bound
        // to `&self`.
        let ptr = match self.tag {
            #[cfg(feature = "prod_handle")]
            HandleTag::Prod => unsafe { __tt_prod_task_statistics(self.ptr.as_ptr()) },
            #[cfg(feature = "test_handle")]
            HandleTag::Test => unsafe { __tt_test_task_statistics(self.ptr.as_ptr()) },
        };
        unsafe { &*ptr }
    }
}

// =====================================================================
// Clone / Drop dispatch — Arc-style refcounting through extern symbols.
// =====================================================================

unsafe extern "Rust" {
    #[cfg(feature = "prod_handle")]
    fn __tt_prod_clone_arc(ptr: *const ());
    #[cfg(feature = "prod_handle")]
    fn __tt_prod_drop_arc(ptr: *const ());
    #[cfg(feature = "test_handle")]
    fn __tt_test_clone_arc(ptr: *const ());
    #[cfg(feature = "test_handle")]
    fn __tt_test_drop_arc(ptr: *const ());

    // Weak-handle support. Each arm provides:
    //   downgrade : *const Arc<T> -> *const Weak<T> (transfers no refcount,
    //               creates a fresh Weak; caller owns the returned weak).
    //   upgrade   : *const Weak<T> -> *const Arc<T> (returns null if the
    //               Arc is gone; otherwise transfers one strong refcount).
    //   clone_weak: bumps the weak refcount.
    //   drop_weak : drops the weak refcount.
    #[cfg(feature = "prod_handle")]
    fn __tt_prod_downgrade(arc_ptr: *const ()) -> *const ();
    #[cfg(feature = "prod_handle")]
    fn __tt_prod_upgrade(weak_ptr: *const ()) -> *const ();
    #[cfg(feature = "prod_handle")]
    fn __tt_prod_clone_weak(weak_ptr: *const ());
    #[cfg(feature = "prod_handle")]
    fn __tt_prod_drop_weak(weak_ptr: *const ());
    #[cfg(feature = "test_handle")]
    fn __tt_test_downgrade(arc_ptr: *const ()) -> *const ();
    #[cfg(feature = "test_handle")]
    fn __tt_test_upgrade(weak_ptr: *const ()) -> *const ();
    #[cfg(feature = "test_handle")]
    fn __tt_test_clone_weak(weak_ptr: *const ());
    #[cfg(feature = "test_handle")]
    fn __tt_test_drop_weak(weak_ptr: *const ());
}

impl Clone for TurboTasksHandle {
    #[inline]
    fn clone(&self) -> Self {
        match self.tag {
            #[cfg(feature = "prod_handle")]
            HandleTag::Prod => unsafe { __tt_prod_clone_arc(self.ptr.as_ptr()) },
            #[cfg(feature = "test_handle")]
            HandleTag::Test => unsafe { __tt_test_clone_arc(self.ptr.as_ptr()) },
        }
        Self {
            tag: self.tag,
            ptr: self.ptr,
        }
    }
}

impl Drop for TurboTasksHandle {
    #[inline]
    fn drop(&mut self) {
        match self.tag {
            #[cfg(feature = "prod_handle")]
            HandleTag::Prod => unsafe { __tt_prod_drop_arc(self.ptr.as_ptr()) },
            #[cfg(feature = "test_handle")]
            HandleTag::Test => unsafe { __tt_test_drop_arc(self.ptr.as_ptr()) },
        }
    }
}

impl TurboTasksHandle {
    /// Downgrades to a weak handle, equivalent to `Arc::downgrade`.
    #[inline]
    pub fn downgrade(&self) -> TurboTasksWeakHandle {
        let weak_ptr = match self.tag {
            #[cfg(feature = "prod_handle")]
            HandleTag::Prod => unsafe { __tt_prod_downgrade(self.ptr.as_ptr()) },
            #[cfg(feature = "test_handle")]
            HandleTag::Test => unsafe { __tt_test_downgrade(self.ptr.as_ptr()) },
        };
        TurboTasksWeakHandle {
            tag: self.tag,
            // `downgrade` always produces a valid pointer (a `Weak` is never
            // null even when the strong count is zero); we can safely
            // `NonNull::new_unchecked` it.
            ptr: unsafe { NonNull::new_unchecked(weak_ptr as *mut ()) },
        }
    }
}

// =====================================================================
// Weak-handle dispatch.
//
// Mirrors the strong-handle dispatch but holds the data pointer of a
// `Weak<T>`. Used by long-lived non-task contexts (e.g. the filesystem
// watcher in `turbo-tasks-fs`) that need to reach back into TurboTasks
// without keeping it alive.
// =====================================================================

/// Weak counterpart to [`TurboTasksHandle`]. Constructed via
/// [`TurboTasksHandle::downgrade`]; upgraded via
/// [`TurboTasksWeakHandle::upgrade`].
#[derive(Debug)]
pub struct TurboTasksWeakHandle {
    tag: HandleTag,
    /// Points at the inner of a `Weak<ConcreteHandle>` owned via
    /// `Weak::into_raw`. The strong count may be zero by the time we
    /// try to upgrade.
    ptr: NonNull<()>,
}

// Safety: as with `TurboTasksHandle`, the concrete weak pointer's data
// is `Send + Sync` for any `T: Send + Sync`.
unsafe impl Send for TurboTasksWeakHandle {}
unsafe impl Sync for TurboTasksWeakHandle {}

impl TurboTasksWeakHandle {
    /// Tries to recover a strong handle. Returns `None` if the underlying
    /// concrete handle has been dropped.
    #[inline]
    pub fn upgrade(&self) -> Option<TurboTasksHandle> {
        let strong_ptr = match self.tag {
            #[cfg(feature = "prod_handle")]
            HandleTag::Prod => unsafe { __tt_prod_upgrade(self.ptr.as_ptr()) },
            #[cfg(feature = "test_handle")]
            HandleTag::Test => unsafe { __tt_test_upgrade(self.ptr.as_ptr()) },
        };
        let strong_ptr = NonNull::new(strong_ptr as *mut ())?;
        // Safety: the provider returned a non-null `Arc::into_raw` pointer
        // for the concrete type indicated by `self.tag`. Ownership of one
        // strong refcount transfers in.
        Some(unsafe { TurboTasksHandle::from_raw_parts(self.tag, strong_ptr) })
    }
}

impl Clone for TurboTasksWeakHandle {
    #[inline]
    fn clone(&self) -> Self {
        match self.tag {
            #[cfg(feature = "prod_handle")]
            HandleTag::Prod => unsafe { __tt_prod_clone_weak(self.ptr.as_ptr()) },
            #[cfg(feature = "test_handle")]
            HandleTag::Test => unsafe { __tt_test_clone_weak(self.ptr.as_ptr()) },
        }
        Self {
            tag: self.tag,
            ptr: self.ptr,
        }
    }
}

impl Drop for TurboTasksWeakHandle {
    #[inline]
    fn drop(&mut self) {
        match self.tag {
            #[cfg(feature = "prod_handle")]
            HandleTag::Prod => unsafe { __tt_prod_drop_weak(self.ptr.as_ptr()) },
            #[cfg(feature = "test_handle")]
            HandleTag::Test => unsafe { __tt_test_drop_weak(self.ptr.as_ptr()) },
        }
    }
}
