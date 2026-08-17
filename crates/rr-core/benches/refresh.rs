//! What an incremental refresh costs before any file is opened.
//!
//! A refresh asks one question per discovered file — may this record be kept —
//! and the answer has to come from the plan without touching the disk. That
//! makes the lookup the only part of the decision that scales with repository
//! size rather than with the size of the change, and the one place where a
//! regression from a binary search to a scan turns "nothing happened" from
//! microseconds into a visible pause on a large repository.
//!
//! Assembling the plan is measured alongside it, because it scales the other
//! way — with the change rather than the repository — and the two together are
//! the whole of what this crate contributes to the fast path.

#![allow(clippy::unwrap_used)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rr_core::path::RelPath;
use rr_core::refresh::{PlanDraft, RefreshPlan};

/// Paths shaped like the ones a real repository produces: nested, sharing
/// prefixes, and long enough that comparing two of them is not free.
fn corpus(count: usize) -> Vec<RelPath> {
    (0..count)
        .map(|i| {
            RelPath::try_from(format!(
                "crates/rr-core/src/module_{:05}/file_{i:05}.rs",
                i / 32
            ))
            .unwrap()
        })
        .collect()
}

/// A plan that rechecks `changed` of the `total` paths, which is the shape the
/// incremental path is for: a small delta against a large corpus.
fn plan_over(paths: &[RelPath], changed: usize) -> RefreshPlan {
    let mut draft = PlanDraft::new();
    let stride = (paths.len() / changed.max(1)).max(1);
    for path in paths.iter().step_by(stride).take(changed) {
        draft.recheck(path.clone());
    }
    draft.build().unwrap()
}

/// The per-file question, asked once for every discovered file.
fn bench_may_retain(c: &mut Criterion) {
    let mut group = c.benchmark_group("refresh_may_retain");

    for &total in &[1_000usize, 10_000, 100_000] {
        let paths = corpus(total);
        let plan = plan_over(&paths, 16);
        group.throughput(Throughput::Elements(total as u64));
        group.bench_with_input(BenchmarkId::from_parameter(total), &total, |b, _| {
            b.iter(|| {
                let mut kept = 0usize;
                for path in &paths {
                    if plan.may_retain(black_box(path)) {
                        kept += 1;
                    }
                }
                kept
            });
        });
    }
    group.finish();
}

/// Turning an observation into a plan, which scales with the change.
fn bench_plan_build(c: &mut Criterion) {
    let paths = corpus(100_000);
    let mut group = c.benchmark_group("refresh_plan_build");

    for &changed in &[1usize, 100, 10_000] {
        group.throughput(Throughput::Elements(changed as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(changed),
            &changed,
            |b, &changed| {
                b.iter(|| black_box(plan_over(&paths, changed)).touched_len());
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_may_retain, bench_plan_build);
criterion_main!(benches);
