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

tt_decl_extern!(fn invalidate(task: crate::TaskId));
tt_decl_handle_method!(fn invalidate(task: crate::TaskId));

// =====================================================================
// Clone / Drop dispatch
// =====================================================================

unsafe extern "Rust" {
    fn __tt_prod_clone_arc(ptr: *const ());
    fn __tt_prod_drop_arc(ptr: *const ());
    fn __tt_test_clone_arc(ptr: *const ());
    fn __tt_test_drop_arc(ptr: *const ());
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
