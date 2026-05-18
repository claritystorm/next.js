#![feature(arbitrary_self_types)]
#![feature(arbitrary_self_types_pointers)]

// Force linkage of `__tt_test_*` providers. See similar comment in
// `tests/eviction.rs`. Benches don't use `turbo_tasks_testing` directly
// but the feature-unified `test_handle` decl exists, so the binary
// needs the test-arm providers linked.
extern crate turbo_tasks_testing;

use criterion::{Criterion, criterion_group, criterion_main};

pub(crate) mod overhead;
pub(crate) mod scope_stress;
pub(crate) mod stress;

criterion_group!(
    name = turbo_tasks_backend_stress;
    config = Criterion::default();
    targets = stress::fibonacci, scope_stress::scope_stress, overhead::overhead
);
criterion_main!(turbo_tasks_backend_stress);
