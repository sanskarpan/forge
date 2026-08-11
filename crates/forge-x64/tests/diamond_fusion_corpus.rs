// NOTE: These tests check `verify_allocation().is_ok()`, i.e. "no two
// overlapping intervals share a location." They complement but do not
// substitute for the dedicated regression test in
// `forge-regalloc/src/liveness.rs`
// (`value_live_across_a_fused_diamond_survives_in_pred_live_out`), which
// is the only test that catches CFG-successor/liveness-undercounting bugs
// (a value silently dropped from `live_out` too early can still pass
// `verify_allocation` if nothing happens to collide with it).

#[test]
fn fused_output_across_the_whole_corpus_still_produces_valid_allocations() {
    let corpus = [
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
    ];
    let mut fused_any = 0;
    for src in corpus {
        let (tokens, diags) = forge_syntax::lexer::lex(src);
        assert!(diags.is_empty(), "lex errors for {src:?}: {diags:?}");
        let (ast, diags) = forge_syntax::parser::parse(&tokens);
        assert!(diags.is_empty(), "parse errors for {src:?}: {diags:?}");
        let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
            .unwrap_or_else(|e| panic!("type errors for {src:?}: {e:?}"));
        let func = forge_ir::lower::lower(&typed);

        let (fusions, _) = forge_x64::find_fusable_diamonds(&func);
        if !fusions.is_empty() {
            fused_any += 1;
        }

        let selected = forge_x64::select(&func);
        let intervals = forge_regalloc::build_intervals(&func, &selected);
        let excluded = forge_regalloc::excluded_registers(&func, &selected);
        let (assignment, _bytes) =
            forge_regalloc::allocate(intervals.clone(), &excluded, &selected);

        assert!(
            forge_regalloc::verify_allocation(&intervals, &assignment).is_ok(),
            "{src:?}: fused output must still produce a valid, independently-verified allocation"
        );
    }
    assert_eq!(
        fused_any, 2,
        "expected exactly 2 of the 18 corpus programs to fuse (both classify as \
         DiamondFusion::FloatMinMax, since all corpus params are F64-typed): \
         \"(if a > b then a else b) + a\" and the inner diamond of \
         \"if a > b then (if a > c then a else c) else b\". A different count means \
         fusion coverage regressed (or improved) and this test's expectations \
         must be revisited, not just \"at least one\""
    );
}

/// This corpus is entirely F64-typed, so the test above gives zero coverage
/// of the `DiamondFusion::IntCmov` path (see `find_fusable_diamonds` /
/// `MachineInst::IntCmov` in `forge_x64::machine_inst`). Neither the
/// front end nor this corpus can reliably produce an I64-typed diamond of
/// the exact shape `find_fusable_diamonds` requires, so this test builds
/// the IR by hand, mirroring the `push_inst`/`empty_func`/`build_diamond`
/// helpers in `forge_x64::machine_inst::tests::diamond_fusion_tests`
/// (adapted here since that module is private to the `forge-x64` crate).
#[test]
fn int_cmov_diamond_produces_a_valid_allocation() {
    use forge_ir::{Block, BlockData, Function, Inst, Terminator, Ty, Value};

    fn push_inst(func: &mut Function, block: Block, inst: Inst, ty: Ty) -> Value {
        let v = Value(func.insts.len() as u32);
        func.insts.push(inst);
        func.types.push(ty);
        func.spans.push(forge_syntax::span::Span::new(0, 0));
        func.blocks[block.0 as usize].insts.push(v);
        v
    }

    // entry(0) --Branch(cond)--> t(1) / e(2), both --Jump--> m(3).
    // t and e are empty; m has a single Phi(t: a, e: c) and returns it.
    // This is the exact shape `eligible_diamond_is_detected_as_int_cmov`
    // in forge_x64::machine_inst::diamond_fusion_tests uses to force
    // DiamondFusion::IntCmov (Ty::I64 payload, no floating point).
    let mut func = Function {
        insts: Vec::new(),
        types: Vec::new(),
        spans: Vec::new(),
        blocks: vec![BlockData::default(); 4],
        entry: Block(0),
        params: Vec::new(),
    };
    let (entry, t, e, m) = (Block(0), Block(1), Block(2), Block(3));

    let a = push_inst(
        &mut func,
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
    );
    let c = push_inst(
        &mut func,
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::I64,
        },
        Ty::I64,
    );
    let cond = push_inst(
        &mut func,
        entry,
        Inst::Param {
            index: 2,
            ty: Ty::Bool,
        },
        Ty::Bool,
    );
    func.blocks[entry.0 as usize].term = Some(Terminator::Branch {
        cond,
        then_: t,
        else_: e,
    });
    func.blocks[t.0 as usize].term = Some(Terminator::Jump(m));
    func.blocks[e.0 as usize].term = Some(Terminator::Jump(m));
    let phi_dst = push_inst(
        &mut func,
        m,
        Inst::Phi {
            incoming: smallvec::smallvec![(t, a), (e, c)],
        },
        Ty::I64,
    );
    func.blocks[m.0 as usize].term = Some(Terminator::Return(phi_dst));

    // Confirm this hand-built IR genuinely produces an IntCmov fusion --
    // otherwise this test would exercise nothing beyond the plain,
    // already-covered non-fused path.
    let (fusions, _) = forge_x64::find_fusable_diamonds(&func);
    assert_eq!(fusions.len(), 1, "expected exactly one fusable diamond");
    assert!(
        matches!(
            fusions.values().next().unwrap(),
            forge_x64::DiamondFusion::IntCmov { .. }
        ),
        "expected the diamond to classify as DiamondFusion::IntCmov, got {:?}",
        fusions.values().next().unwrap()
    );

    let selected = forge_x64::select(&func);
    assert!(
        selected
            .insts
            .iter()
            .any(|i| matches!(i, forge_x64::MachineInst::IntCmov { .. })),
        "expected select() to actually emit an IntCmov for this diamond"
    );
    assert!(
        !selected
            .insts
            .iter()
            .any(|i| matches!(i, forge_x64::MachineInst::Branch { .. })),
        "expected select() to fuse this diamond away entirely, leaving no Branch"
    );

    let intervals = forge_regalloc::build_intervals(&func, &selected);
    let excluded = forge_regalloc::excluded_registers(&func, &selected);
    let (assignment, _bytes) = forge_regalloc::allocate(intervals.clone(), &excluded, &selected);

    assert!(
        forge_regalloc::verify_allocation(&intervals, &assignment).is_ok(),
        "IntCmov-fused output must produce a valid, independently-verified allocation"
    );
}
