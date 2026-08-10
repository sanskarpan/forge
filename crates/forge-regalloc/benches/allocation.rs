use criterion::{criterion_group, criterion_main, Criterion};
use forge_ir::Value;
use forge_regalloc::{allocate, Interval, RegClass};
use std::collections::HashMap;

/// 1000 values, staggered SHORT-lived ranges (NOT all-overlapping -- the
/// all-overlapping shape belongs to bullet 20's correctness stress test
/// in tests/integration.rs, not this performance benchmark, which should
/// reflect realistic throughput: mostly short-lived SSA values with
/// localized overlap, not one maximally-adversarial all-live-at-once
/// block). Split evenly GPR/XMM to exercise both of allocate()'s passes.
fn thousand_value_intervals() -> Vec<Interval> {
    (0..1000)
        .map(|n| Interval {
            value: Value(n),
            start: n,
            end: n + 4,
            reg_class: if n % 2 == 0 {
                RegClass::Gpr
            } else {
                RegClass::Xmm
            },
            hint: None,
            fixed: None,
            spill_weight: 0.0,
        })
        .collect()
}

fn bench_allocate(c: &mut Criterion) {
    let intervals = thousand_value_intervals();
    let selected = forge_x64::SelectedFunction {
        insts: Vec::new(), // spill_weight isn't what this benchmark measures
        synthetic_types: HashMap::new(),
        coalescing_hints: HashMap::new(),
        pool: forge_x64::ConstantPool::default(),
        block_starts: Vec::new(),
    };
    c.bench_function("allocate_1000_values", |b| {
        b.iter(|| allocate(intervals.clone(), &HashMap::new(), &selected))
    });
}

criterion_group!(benches, bench_allocate);
criterion_main!(benches);
