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
    use super::*;

    /// Generates `#[no_mangle] pub extern "Rust" fn __tt_prod_<name>(...)`
    /// for a single dispatched method. The body casts `ptr` back to
    /// `&ProdHandleConcrete` and forwards to the trait method.
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
                <ProdHandleConcrete as turbo_tasks::TurboTasksApi>::$name(tt $(, $arg)*)
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

    provide_prod!(fn invalidate(task: turbo_tasks::TaskId));

    // ---- Arc clone / drop -------------------------------------------------

    #[unsafe(no_mangle)]
    pub extern "Rust" fn __tt_prod_clone_arc(ptr: *const ()) {
        // Reconstitute the Arc transiently to bump the refcount, then leak
        // it again so the next drop sees the same pointer.
        let arc = unsafe { Arc::from_raw(ptr as *const ProdHandleConcrete) };
        let cloned = arc.clone();
        // Re-leak the original to keep the original handle alive.
        std::mem::forget(arc);
        // The clone is owned by the new handle that triggered this call;
        // leak its pointer so the new handle owns it.
        let _new_ptr = Arc::into_raw(cloned);
        // `_new_ptr` must equal `ptr` because Arc::into_raw is reproducible
        // for the same Arc — the new handle keeps `ptr` and we discard
        // `_new_ptr`. (See the Drop impl in turbo-tasks for the matching
        // decrement; both ends use the same pointer value.)
        debug_assert_eq!(_new_ptr as *const (), ptr);
    }

    #[unsafe(no_mangle)]
    pub extern "Rust" fn __tt_prod_drop_arc(ptr: *const ()) {
        // Reconstitute the Arc to decrement and drop.
        drop(unsafe { Arc::from_raw(ptr as *const ProdHandleConcrete) });
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
                <TestHandleConcrete as turbo_tasks::TurboTasksApi>::$name(tt $(, $arg)*)
            }
        };
    }

    // ---- dispatched methods ----------------------------------------------
    // (Mirror of the `prod` block — keep in sync.)

    provide_test!(fn invalidate(task: turbo_tasks::TaskId));

    // ---- Arc clone / drop -------------------------------------------------

    #[unsafe(no_mangle)]
    pub extern "Rust" fn __tt_test_clone_arc(ptr: *const ()) {
        let arc = unsafe { Arc::from_raw(ptr as *const TestHandleConcrete) };
        let cloned = arc.clone();
        std::mem::forget(arc);
        let _new_ptr = Arc::into_raw(cloned);
        debug_assert_eq!(_new_ptr as *const (), ptr);
    }

    #[unsafe(no_mangle)]
    pub extern "Rust" fn __tt_test_drop_arc(ptr: *const ()) {
        drop(unsafe { Arc::from_raw(ptr as *const TestHandleConcrete) });
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
