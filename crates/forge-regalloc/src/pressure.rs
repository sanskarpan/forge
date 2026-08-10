use crate::interval::{Interval, RegClass};

/// Register pressure (simultaneously live interval count) at a single
/// linearized instruction position, split by class -- GPR and XMM are
/// allocated from wholly separate pools throughout this allocator (8a-8c),
/// so a combined count would conflate two numbers that are never compared
/// against the same budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PressurePoint {
    pub gpr: u32,
    pub xmm: u32,
}

/// Computes register pressure at EVERY position `0..program_length`
/// (dense, not just at positions where pressure changes) via a standard
/// sweep-line: +1 at each interval's `start`, -1 at `end + 1` (the first
/// position it's no longer live, given INCLUSIVE `[start, end]`), then a
/// running prefix sum. `program_length` must be passed explicitly rather
/// than inferred from `intervals.iter().map(|iv| iv.end).max()` -- a
/// function's real instruction count can legitimately exceed every
/// interval's own `end`, and silently under-sizing the report would leave
/// those trailing positions unrepresented rather than correctly reported
/// as zero pressure. Callers pass `selected.insts.len()`.
///
/// Dense (one entry per position, not a sparse step-function) because the
/// stated consumer is a workbench chart plotting pressure across the
/// instruction-index axis directly -- a sparse representation would just
/// push the same expansion work onto every future caller instead of
/// doing it once here.
///
/// The peak of this curve is an UPPER BOUND on register demand, not equal
/// to it: `pick_register`'s Case 2 deliberately puts two simultaneously
/// live intervals in ONE register at a touching position (the same
/// legitimate handoff `verify_allocation` exempts). Measured on
/// `if a > b then (a * c) + (b * c) else a - b`: peak 7 live XMM values,
/// only 6 distinct XMM registers ever occupied. Do not read a point on
/// this curve as "this many registers are needed here."
pub fn register_pressure(intervals: &[Interval], program_length: usize) -> Vec<PressurePoint> {
    let mut gpr_delta = vec![0i32; program_length + 1];
    let mut xmm_delta = vec![0i32; program_length + 1];
    for iv in intervals {
        let delta = match iv.reg_class {
            RegClass::Gpr => &mut gpr_delta,
            RegClass::Xmm => &mut xmm_delta,
        };
        let start = iv.start as usize;
        let end_exclusive = (iv.end as usize + 1).min(program_length);
        if start < program_length {
            delta[start] += 1;
        }
        if end_exclusive < program_length {
            delta[end_exclusive] -= 1;
        }
    }
    let mut out = Vec::with_capacity(program_length);
    let (mut gpr_running, mut xmm_running) = (0i32, 0i32);
    for pos in 0..program_length {
        gpr_running += gpr_delta[pos];
        xmm_running += xmm_delta[pos];
        out.push(PressurePoint {
            gpr: gpr_running as u32,
            xmm: xmm_running as u32,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_ir::Value;

    fn iv(value: u32, start: u32, end: u32, class: RegClass) -> Interval {
        Interval {
            value: Value(value),
            start,
            end,
            reg_class: class,
            hint: None,
            fixed: None,
            spill_weight: 0.0,
        }
    }

    #[test]
    fn reports_disjoint_intervals_correctly() {
        let intervals = vec![
            iv(0, 0, 1, RegClass::Gpr),
            iv(1, 3, 4, RegClass::Gpr),
            iv(2, 6, 7, RegClass::Gpr),
        ];
        let pressure = register_pressure(&intervals, 10);

        assert_eq!(pressure.len(), 10);
        assert_eq!(pressure[0], PressurePoint { gpr: 1, xmm: 0 });
        assert_eq!(pressure[1], PressurePoint { gpr: 1, xmm: 0 });
        assert_eq!(pressure[2], PressurePoint { gpr: 0, xmm: 0 }); // gap between intervals
        assert_eq!(pressure[3], PressurePoint { gpr: 1, xmm: 0 });
        assert_eq!(pressure[5], PressurePoint { gpr: 0, xmm: 0 });
        assert_eq!(pressure[6], PressurePoint { gpr: 1, xmm: 0 });
        assert_eq!(pressure[8], PressurePoint { gpr: 0, xmm: 0 });
    }

    #[test]
    fn reports_overlapping_intervals_correctly() {
        let intervals = vec![
            iv(0, 0, 5, RegClass::Gpr),
            iv(1, 0, 5, RegClass::Gpr),
            iv(2, 0, 5, RegClass::Gpr),
        ];
        let pressure = register_pressure(&intervals, 6);

        for (pos, p) in pressure.iter().enumerate() {
            assert_eq!(
                *p,
                PressurePoint { gpr: 3, xmm: 0 },
                "position {pos} must show all 3 intervals live"
            );
        }
    }

    #[test]
    fn keeps_gpr_and_xmm_independent() {
        let intervals = vec![
            iv(0, 0, 5, RegClass::Gpr),
            iv(1, 0, 5, RegClass::Gpr),
            iv(2, 0, 5, RegClass::Xmm),
        ];
        let pressure = register_pressure(&intervals, 6);

        assert_eq!(pressure[2], PressurePoint { gpr: 2, xmm: 1 });
    }

    #[test]
    fn is_zero_past_every_intervals_end() {
        let intervals = vec![iv(0, 0, 2, RegClass::Gpr)];
        let pressure = register_pressure(&intervals, 10);

        for (pos, p) in pressure.iter().enumerate().skip(3) {
            assert_eq!(
                *p,
                PressurePoint { gpr: 0, xmm: 0 },
                "position {pos} is past the only interval's end -- must be zero, not stale"
            );
        }
    }

    #[test]
    fn does_not_panic_when_program_length_is_smaller_than_an_intervals_end() {
        // A deliberately inconsistent input -- program_length shorter than
        // a real interval's end. Must not panic and must not wrap around
        // to a huge u32 from an unmatched decrement.
        let intervals = vec![iv(0, 1, 10, RegClass::Gpr), iv(1, 0, 100, RegClass::Xmm)];
        let pressure = register_pressure(&intervals, 4);

        assert_eq!(pressure.len(), 4);
        for p in &pressure {
            assert!(
                p.gpr < 1000 && p.xmm < 1000,
                "must not wrap around to a huge count: {p:?}"
            );
        }
    }

    #[test]
    fn handles_program_length_zero() {
        let intervals = vec![iv(0, 0, 5, RegClass::Gpr)];
        let pressure = register_pressure(&intervals, 0);

        assert!(pressure.is_empty());
    }

    #[test]
    fn matches_program_length_and_stays_within_pool_plus_spills_for_the_corpus() {
        for src in test_corpus() {
            let func = front_end(src);
            let selected = forge_x64::select(&func);
            let intervals = crate::intervals::build_intervals(&func, &selected);
            let excluded = crate::intervals::excluded_registers(&func, &selected);
            let (assignment, _bytes) =
                crate::linear_scan::allocate(intervals.clone(), &excluded, &selected);

            let pressure = register_pressure(&intervals, selected.insts.len());
            assert_eq!(
                pressure.len(),
                selected.insts.len(),
                "{src:?}: pressure report length must equal the real instruction count"
            );

            let is_spill = |iv: &&Interval| {
                matches!(
                    assignment.get(&iv.value),
                    Some(crate::linear_scan::Location::Spill(_))
                )
            };
            let spilled_gpr = intervals
                .iter()
                .filter(|iv| iv.reg_class == RegClass::Gpr)
                .filter(is_spill)
                .count() as u32;
            let spilled_xmm = intervals
                .iter()
                .filter(|iv| iv.reg_class == RegClass::Xmm)
                .filter(is_spill)
                .count() as u32;

            let peak_gpr = pressure.iter().map(|p| p.gpr).max().unwrap_or(0);
            let peak_xmm = pressure.iter().map(|p| p.xmm).max().unwrap_or(0);

            // SPILL_AWARE_ALLOCATABLE_GPR/XMM, NOT the wider ALLOCATABLE_GPR/XMM --
            // that's the pool allocate() actually scans against since Phase 8c.
            // On THIS corpus every program spills zero values (consistent with
            // linear_scan.rs's own `run_produces_only_reg_locations_never_spill_
            // for_the_corpus`), so `spilled_gpr`/`spilled_xmm` are always 0 here
            // and the bound is comfortably slack (measured: peak GPR 4 vs pool
            // 12, peak XMM 7 vs pool 14) -- this test's job is confirming the
            // bound HOLDS with real numbers, not that it's tight; a separate,
            // randomized high-pressure exercise during design review (not part
            // of this corpus) is what measured the bound as tight (zero slack)
            // on 186/200 programs that actually spill.
            let gpr_pool = crate::linear_scan::SPILL_AWARE_ALLOCATABLE_GPR.len() as u32;
            let xmm_pool = crate::linear_scan::SPILL_AWARE_ALLOCATABLE_XMM.len() as u32;
            assert!(
                peak_gpr <= gpr_pool + spilled_gpr,
                "{src:?}: peak GPR pressure {peak_gpr} exceeds pool size + spills"
            );
            assert!(
                peak_xmm <= xmm_pool + spilled_xmm,
                "{src:?}: peak XMM pressure {peak_xmm} exceeds pool size + spills"
            );
        }
    }

    /// Shared corpus, copied VERBATIM from `crates/forge-regalloc/src/linear_scan.rs`'s
    /// `test_corpus()` -- see verify.rs's identical duplication for the established
    /// reasoning (this project's convention, not an oversight).
    fn test_corpus() -> Vec<&'static str> {
        vec![
            "3.14159 * r * r",
            "sin(x) + cos(y)",
            "(n * 2654435761) >> 16",
            "x / y",
            "x + 1",
            "fma(a, b, c)",
            "base + i * 8",
            "let t = a - b in if t > 0.0 then t else -t",
            "if a > b then (if a > c then a else c) else b",
            "(if a > b then a else b) + a",
            "sqrt(x * x + y * y)",
            "abs(x) + floor(y) + ceil(z)",
            "(n >> 1) % 7 + (n >> 1) / 7",
            "if a > b then (a * c) + (b * c) else a - b",
            "if a > b then (a - b) - (a + b) else c - a",
            "if a > b then fma(a, b, c) else a * c",
            "if x > y then (x * y) + (x - y) else x / y",
            "if x > y then fma(x, y, z) * x else fma(y, x, z) - y",
        ]
    }

    fn front_end(src: &str) -> forge_ir::Function {
        let (tokens, diags) = forge_syntax::lexer::lex(src);
        assert!(diags.is_empty(), "lex errors for {src:?}: {diags:?}");
        let (ast, diags) = forge_syntax::parser::parse(&tokens);
        assert!(diags.is_empty(), "parse errors for {src:?}: {diags:?}");
        let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
            .unwrap_or_else(|e| panic!("type errors for {src:?}: {e:?}"));
        forge_ir::lower::lower(&typed)
    }
}
