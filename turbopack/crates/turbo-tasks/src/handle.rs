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
//! Providers (the `__tt_<arm>_<method>` symbols) live in the
//! `turbo-tasks-handle` crate. That crate depends on both
//! `turbo-tasks-backend` (for the prod arm) and `turbo-tasks-testing`
//! (for the test arm); each arm is gated by a Cargo feature. By centralising
//! the providers in one crate, we have exactly one place that knows about
//! the dispatch contract.

use std::ptr::NonNull;

// Re-export the macro for use by `turbo-tasks-handle`'s provider macro,
// which needs to iterate over the same method list.
//
// TODO: when we add `turbo_tasks_weak()` to the dispatch surface, also
// generate `__tt_<arm>_downgrade` / `upgrade` and a `TurboTasksWeakHandle`
// type. For now `turbo_tasks_weak` keeps returning `Weak<dyn TurboTasksApi>`
// because its only consumer (`turbo_tasks_future_scope`) is not on a hot path.

/// Identifier for which concrete implementation a [`TurboTasksHandle`] points
/// at. Used as the tag in the tagged-pointer dispatch.
///
/// New variants must coordinate with `turbo-tasks-handle`'s provider
/// emission and with every caller's `match` against this tag.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HandleTag {
    /// `TurboTasks<TurboTasksBackend<…>>` — the production handle used by
    /// `next-napi-bindings`, benches, etc.
    Prod = 0,
    /// `VcStorage` — the test-only handle used by `turbo-tasks-testing`.
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

/// Generates an `unsafe extern "Rust" { fn __tt_prod_<name>(...); fn
/// __tt_test_<name>(...); }` declaration pair for one dispatched method.
macro_rules! tt_decl_extern {
    (
        fn $name:ident( $($arg:ident : $ty:ty),* $(,)? ) $(-> $ret:ty)?
    ) => {
        unsafe extern "Rust" {
            fn ${concat(__tt_prod_, $name)}(ptr: *const () $(, $arg : $ty)*) $(-> $ret)?;
            fn ${concat(__tt_test_, $name)}(ptr: *const () $(, $arg : $ty)*) $(-> $ret)?;
        }
    };
}

/// Generates an inherent method on `TurboTasksHandle` that dispatches over
/// `match self.tag` to the corresponding `extern "Rust"` symbol.
macro_rules! tt_decl_handle_method {
    (
        fn $name:ident( $($arg:ident : $ty:ty),* $(,)? ) $(-> $ret:ty)?
    ) => {
        impl TurboTasksHandle {
            #[inline]
            pub fn $name(&self $(, $arg : $ty)*) $(-> $ret)? {
                match self.tag {
                    HandleTag::Prod => unsafe {
                        ${concat(__tt_prod_, $name)}(self.ptr.as_ptr() $(, $arg)*)
                    },
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

// =====================================================================
// Clone / Drop dispatch — Arc-style refcounting through extern symbols.
// =====================================================================

unsafe extern "Rust" {
    fn __tt_prod_clone_arc(ptr: *const ());
    fn __tt_prod_drop_arc(ptr: *const ());
    fn __tt_test_clone_arc(ptr: *const ());
    fn __tt_test_drop_arc(ptr: *const ());

    // Weak-handle support. Each arm provides:
    //   downgrade : *const Arc<T> -> *const Weak<T> (transfers no refcount,
    //               creates a fresh Weak; caller owns the returned weak).
    //   upgrade   : *const Weak<T> -> *const Arc<T> (returns null if the
    //               Arc is gone; otherwise transfers one strong refcount).
    //   clone_weak: bumps the weak refcount.
    //   drop_weak : drops the weak refcount.
    fn __tt_prod_downgrade(arc_ptr: *const ()) -> *const ();
    fn __tt_prod_upgrade(weak_ptr: *const ()) -> *const ();
    fn __tt_prod_clone_weak(weak_ptr: *const ());
    fn __tt_prod_drop_weak(weak_ptr: *const ());
    fn __tt_test_downgrade(arc_ptr: *const ()) -> *const ();
    fn __tt_test_upgrade(weak_ptr: *const ()) -> *const ();
    fn __tt_test_clone_weak(weak_ptr: *const ());
    fn __tt_test_drop_weak(weak_ptr: *const ());
}

impl Clone for TurboTasksHandle {
    #[inline]
    fn clone(&self) -> Self {
        match self.tag {
            HandleTag::Prod => unsafe { __tt_prod_clone_arc(self.ptr.as_ptr()) },
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
            HandleTag::Prod => unsafe { __tt_prod_drop_arc(self.ptr.as_ptr()) },
            HandleTag::Test => unsafe { __tt_test_drop_arc(self.ptr.as_ptr()) },
        }
    }
}

impl TurboTasksHandle {
    /// Downgrades to a weak handle, equivalent to `Arc::downgrade`.
    #[inline]
    pub fn downgrade(&self) -> TurboTasksWeakHandle {
        let weak_ptr = match self.tag {
            HandleTag::Prod => unsafe { __tt_prod_downgrade(self.ptr.as_ptr()) },
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
            HandleTag::Prod => unsafe { __tt_prod_upgrade(self.ptr.as_ptr()) },
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
            HandleTag::Prod => unsafe { __tt_prod_clone_weak(self.ptr.as_ptr()) },
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
            HandleTag::Prod => unsafe { __tt_prod_drop_weak(self.ptr.as_ptr()) },
            HandleTag::Test => unsafe { __tt_test_drop_weak(self.ptr.as_ptr()) },
        }
    }
}
