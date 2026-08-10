use forge_ir::Value;
use forge_regalloc::{allocate, build_intervals, excluded_registers, verify_allocation, Location};
use forge_syntax::span::Span;

/// CHECKLIST Phase 8 bullet 19: "Test: 3 values, 16 registers, no
/// spills". "16 registers" is stale CHECKLIST wording from before Phase
/// 8c introduced SCRATCH_GPR/XMM reservation -- the real pool this test
/// runs against is SPILL_AWARE_ALLOCATABLE_GPR (12), via the real
/// allocate() the crate actually ships. A real 3-VARIABLE source program
/// cannot produce exactly 3 values (3 Params plus at least 1 combining op
/// is always at least 4 values, and untyped surface arithmetic lowers to
/// F64/XMM anyway, not GPR) -- confirmed by execution during design
/// review. A hand-built I64 function (2 Params plus 1 Add equals exactly
/// 3 values) goes through the SAME real select, build_intervals, allocate
/// pipeline; only the front-end source-text stage is bypassed, not any
/// part of what this bullet is actually testing.
#[test]
fn bullet_19_three_values_no_spills() {
    let mut b = forge_ir::builder::Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        forge_ir::Inst::Param {
            index: 0,
            ty: forge_ir::Ty::I64,
        },
        forge_ir::Ty::I64,
        Span::new(0, 0),
    );
    let y = b.emit(
        entry,
        forge_ir::Inst::Param {
            index: 1,
            ty: forge_ir::Ty::I64,
        },
        forge_ir::Ty::I64,
        Span::new(0, 0),
    );
    let sum = b.emit(
        entry,
        forge_ir::Inst::Add(x, y),
        forge_ir::Ty::I64,
        Span::new(0, 0),
    );
    b.f.blocks[entry.0 as usize].term = Some(forge_ir::Terminator::Return(sum));

    let selected = forge_x64::select(&b.f);
    let intervals = build_intervals(&b.f, &selected);
    let excluded = excluded_registers(&b.f, &selected);

    assert_eq!(
        intervals.len(),
        3,
        "2 Params + 1 Add must produce exactly 3 values"
    );
    for iv in &intervals {
        assert_eq!(
            iv.reg_class,
            forge_regalloc::RegClass::Gpr,
            "I64 params/results must be GPR-class"
        );
    }

    let (assignment, bytes) = allocate(intervals.clone(), &excluded, &selected);

    for iv in &intervals {
        assert!(
            matches!(assignment.get(&iv.value), Some(Location::Reg(_))),
            "3 values into a 12-register pool must never spill: {:?}",
            iv.value
        );
    }
    assert_eq!(bytes, 0);
    assert!(verify_allocation(&intervals, &assignment).is_ok());
}

/// CHECKLIST Phase 8 bullet 20: "Test: 40 simultaneously live values, 16
/// registers -> correct results with spills". "Correct RESULTS" (i.e.
/// verified against real program execution) needs the not-yet-built
/// MachineInst-to-bytes emission pipeline (task #68) and is out of scope
/// here -- what's checkable now is that the ALLOCATION itself is sound
/// (independently verified via verify_allocation), which is the
/// load-bearing precondition for execution correctness once emission
/// exists. On this specific fixture (every interval hint: None, so the
/// handoff exemption never fires, and every interval shares one
/// identical range so no two spilled values can ever land in the same
/// slot) verify_allocation's Ok is a real but narrow regression guard --
/// it can only fail if allocate() double-books a register outright.
#[test]
fn bullet_20_forty_live_values_forces_spilling_and_stays_valid() {
    let intervals: Vec<forge_regalloc::Interval> = (0..40)
        .map(|n| forge_regalloc::Interval {
            value: Value(n),
            start: 0,
            end: 50,
            reg_class: forge_regalloc::RegClass::Gpr,
            hint: None,
            fixed: None,
            spill_weight: 0.0,
        })
        .collect();

    let selected = forge_x64::SelectedFunction {
        insts: Vec::new(),
        synthetic_types: std::collections::HashMap::new(),
        coalescing_hints: std::collections::HashMap::new(),
        pool: forge_x64::ConstantPool::default(),
        block_starts: Vec::new(),
    };
    let (assignment, bytes) = allocate(
        intervals.clone(),
        &std::collections::HashMap::new(),
        &selected,
    );

    let spilled = intervals
        .iter()
        .filter(|iv| matches!(assignment.get(&iv.value), Some(Location::Spill(_))))
        .count();
    assert_eq!(
        spilled, 28,
        "40 intervals into a 12-register pool must spill exactly 28"
    );
    assert_eq!(
        bytes, 224,
        "28 spills that can never reuse a slot must need exactly 224 bytes"
    );
    assert_eq!(
        assignment.len(),
        40,
        "every interval must get SOME Location"
    );
    assert!(verify_allocation(&intervals, &assignment).is_ok());
}

/// CHECKLIST Phase 8 bullet 22: "Test: expression calling libm ->
/// caller-saved values are spilled around the call". "Spilled around the
/// call" describes an EMISSION-time save/restore sequence -- the exact
/// same category of problem as `idiv`'s third-party rax/rdx clobber
/// (Phase 8c's design doc: "resolvable at emission time via ordinary
/// stack push/pop for the displaced occupants"), deferred to the
/// not-yet-built emission pipeline (task #68), same as bullet 20. What's
/// checkable and worth checking NOW: (1) verify_allocation returns Ok,
/// confirming the CURRENT, documented scope boundary (this allocator
/// does not model call clobbers -- see verify.rs's own doc comment,
/// added in Phase 8d's holistic review, commit 53193fb); (2) at least
/// one XMM interval's range STRICTLY contains a real CallLibm's
/// position, proving the hazard is REAL on this program, not a
/// hypothetical the test can't actually trigger. The strict predicate
/// (`iv.start < pos && pos < iv.end`) is required, not the inclusive
/// `<=`/`<=` form -- confirmed by execution during design review that
/// the inclusive form is trivially satisfiable by any libm call's own
/// argument/result intervals with ZERO genuine cross-call liveness
/// (e.g. `sin(x)` alone scores 2 hits under the inclusive form and 0
/// under the strict one -- the strict form is what actually distinguishes
/// "genuinely live across the call" from "merely borders the call").
#[test]
fn bullet_22_libm_call_clobber_hazard_is_real_and_currently_unverified() {
    let src = "sin(x) + cos(y) + x + y";
    let (tokens, diags) = forge_syntax::lexer::lex(src);
    assert!(diags.is_empty(), "lex errors: {diags:?}");
    let (ast, diags) = forge_syntax::parser::parse(&tokens);
    assert!(diags.is_empty(), "parse errors: {diags:?}");
    let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
        .unwrap_or_else(|e| panic!("type errors: {e:?}"));
    let func = forge_ir::lower::lower(&typed);

    let selected = forge_x64::select(&func);
    let intervals = build_intervals(&func, &selected);
    let excluded = excluded_registers(&func, &selected);
    let (assignment, _bytes) = allocate(intervals.clone(), &excluded, &selected);

    assert!(
        verify_allocation(&intervals, &assignment).is_ok(),
        "current, documented scope boundary: this allocator doesn't model call clobbers"
    );

    let call_positions: Vec<usize> = selected
        .insts
        .iter()
        .enumerate()
        .filter(|(_, inst)| matches!(inst, forge_x64::MachineInst::CallLibm { .. }))
        .map(|(pos, _)| pos)
        .collect();
    assert!(
        !call_positions.is_empty(),
        "this program must contain at least one CallLibm"
    );

    let hazard_is_real = intervals.iter().any(|iv| {
        iv.reg_class == forge_regalloc::RegClass::Xmm
            && call_positions
                .iter()
                .any(|&pos| (iv.start as usize) < pos && pos < (iv.end as usize))
    });
    assert!(
        hazard_is_real,
        "no XMM interval is genuinely live across a CallLibm on this program -- the test is \
         vacuous and doesn't actually exercise the hazard it claims to"
    );
}
