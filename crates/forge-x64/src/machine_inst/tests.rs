use super::*;
use forge_ir::builder::Builder;
use forge_ir::{Inst, Terminator, Ty};
use forge_syntax::span::Span;

fn dummy_span() -> Span {
    Span::new(0, 0)
}

#[test]
fn select_lowers_a_single_i64_constant_and_return() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let c = b.emit(entry, Inst::ConstI64(42), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(c));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts,
        vec![
            MachineInst::LoadImmI64 { dst: c, imm: 42 },
            MachineInst::Return { value: c },
        ]
    );
    assert!(selected.synthetic_types.is_empty());
}

#[test]
fn select_lowers_an_f64_constant() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let bits = 3.5f64.to_bits();
    let c = b.emit(entry, Inst::ConstF64(bits), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(c));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts,
        vec![
            MachineInst::LoadImmF64 { dst: c, bits },
            MachineInst::Return { value: c },
        ]
    );
}

#[test]
fn select_lowers_a_bool_constant_as_zero_or_one() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let t = b.emit(entry, Inst::ConstBool(true), Ty::Bool, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(t));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts[0],
        MachineInst::LoadImmI64 { dst: t, imm: 1 }
    );
}

#[test]
fn select_lowers_a_param() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let p = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(p));

    let selected = select(&b.f);

    assert_eq!(selected.insts[0], MachineInst::Param { dst: p, index: 0 });
}

/// Two blocks joined by an unconditional jump -- confirms RPO block
/// ordering (entry's Jump comes before target's contents in the
/// output, matching visitation order, not just definition order).
#[test]
fn select_lowers_jump_and_visits_blocks_in_rpo() {
    let mut b = Builder::new();
    let entry = b.create_block();
    let target = b.create_block();
    b.add_pred(target, entry);
    b.seal_block(entry);
    b.seal_block(target);
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Jump(target));
    let c = b.emit(target, Inst::ConstI64(7), Ty::I64, dummy_span());
    b.f.blocks[target.0 as usize].term = Some(Terminator::Return(c));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts,
        vec![
            MachineInst::Jump { target },
            MachineInst::LoadImmI64 { dst: c, imm: 7 },
            MachineInst::Return { value: c },
        ]
    );
}

/// Genuinely discriminates real reverse-postorder from naive
/// block-creation-order iteration -- the previous test above can't,
/// since its blocks happen to be created in the same order the CFG
/// visits them. Here `x` is created BEFORE `y`, but the CFG only
/// reaches `x` THROUGH `y` (entry -> y -> x), so creation order is
/// `[entry, x, y]` while real RPO is `[entry, y, x]`. If `select()`
/// ever regressed to iterating `func.blocks` in creation order instead
/// of calling `reverse_postorder`, this test would catch it: `x`'s
/// body would wrongly appear before `y`'s `Jump` in the output.
#[test]
fn select_visits_blocks_in_true_rpo_not_creation_order() {
    let mut b = Builder::new();
    let entry = b.create_block();
    let x = b.create_block(); // created 2nd, but visited LAST
    let y = b.create_block(); // created 3rd, but visited 2nd
    b.add_pred(y, entry);
    b.add_pred(x, y);
    b.seal_block(entry);
    b.seal_block(y);
    b.seal_block(x);
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Jump(y));
    b.f.blocks[y.0 as usize].term = Some(Terminator::Jump(x));
    let c = b.emit(x, Inst::ConstI64(9), Ty::I64, dummy_span());
    b.f.blocks[x.0 as usize].term = Some(Terminator::Return(c));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts,
        vec![
            MachineInst::Jump { target: y },
            MachineInst::Jump { target: x },
            MachineInst::LoadImmI64 { dst: c, imm: 9 },
            MachineInst::Return { value: c },
        ]
    );
}

#[test]
fn select_lowers_branch() {
    let mut b = Builder::new();
    let entry = b.create_block();
    let then_b = b.create_block();
    let else_b = b.create_block();
    b.add_pred(then_b, entry);
    b.add_pred(else_b, entry);
    b.seal_block(entry);
    b.seal_block(then_b);
    b.seal_block(else_b);
    let cond = b.emit(entry, Inst::ConstBool(true), Ty::Bool, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
        cond,
        then_: then_b,
        else_: else_b,
    });
    let t = b.emit(then_b, Inst::ConstI64(1), Ty::I64, dummy_span());
    b.f.blocks[then_b.0 as usize].term = Some(Terminator::Return(t));
    let e = b.emit(else_b, Inst::ConstI64(0), Ty::I64, dummy_span());
    b.f.blocks[else_b.0 as usize].term = Some(Terminator::Return(e));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts[1],
        MachineInst::Branch {
            cond,
            then_: then_b,
            else_: else_b
        }
    );
}

/// Builds a block with two i64 params and one binary-op instruction
/// between them, returning the op's result -- the shared shape every
/// test below uses, parameterized by which Inst to build and which
/// MachineInst it should lower to.
fn select_i64_binop(
    inst_ctor: impl FnOnce(Value, Value) -> Inst,
) -> (SelectedFunction, Value, Value, Value) {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let y = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let r = b.emit(entry, inst_ctor(x, y), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));
    let selected = select(&b.f);
    (selected, x, y, r)
}

#[test]
fn select_lowers_int_add() {
    let (selected, x, y, r) = select_i64_binop(Inst::Add);
    assert_eq!(
        selected.insts[2],
        MachineInst::IntAdd {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_int_sub() {
    let (selected, x, y, r) = select_i64_binop(Inst::Sub);
    assert_eq!(
        selected.insts[2],
        MachineInst::IntSub {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_int_mul() {
    let (selected, x, y, r) = select_i64_binop(Inst::Mul);
    assert_eq!(
        selected.insts[2],
        MachineInst::IntMul {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_int_div() {
    let (selected, x, y, r) = select_i64_binop(Inst::Div);
    assert_eq!(
        selected.insts[2],
        MachineInst::IntDiv {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_int_rem() {
    let (selected, x, y, r) = select_i64_binop(Inst::Rem);
    assert_eq!(
        selected.insts[2],
        MachineInst::IntRem {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

/// Float remainder (`x % y` on f64) is a real, exercised language
/// feature (see interp.rs's oracle) but has no native x86 instruction
/// and no libm route yet (LibFunc has no Fmod variant) -- deferred
/// with a clear panic, exactly like Call. This is NOT the same kind
/// of "acceptable interim approximation" as Fma's Mul+Add decomposition:
/// a naive `x - trunc(x/y)*y` software sequence can diverge
/// arbitrarily (catastrophic cancellation) from Rust's `%` for large
/// x/y ratios, which would be a real, unbounded correctness bug, not
/// a bounded/documented precision difference -- so it's deferred
/// entirely rather than approximated.
#[test]
#[should_panic(expected = "float remainder")]
fn select_panics_on_float_rem_with_a_clear_deferral_message() {
    let (selected, ..) = select_f64_binop(Inst::Rem);
    let _ = selected; // unreachable if select_f64_binop itself panics, which it must
}

#[test]
fn select_lowers_and() {
    let (selected, x, y, r) = select_i64_binop(Inst::And);
    assert_eq!(
        selected.insts[2],
        MachineInst::And {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_or() {
    let (selected, x, y, r) = select_i64_binop(Inst::Or);
    assert_eq!(
        selected.insts[2],
        MachineInst::Or {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_xor() {
    let (selected, x, y, r) = select_i64_binop(Inst::Xor);
    assert_eq!(
        selected.insts[2],
        MachineInst::Xor {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_shl() {
    let (selected, x, y, r) = select_i64_binop(Inst::Shl);
    assert_eq!(
        selected.insts[2],
        MachineInst::Shl {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_shr() {
    let (selected, x, y, r) = select_i64_binop(Inst::Shr);
    assert_eq!(
        selected.insts[2],
        MachineInst::Shr {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_sar() {
    let (selected, x, y, r) = select_i64_binop(Inst::Sar);
    assert_eq!(
        selected.insts[2],
        MachineInst::Sar {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

fn select_f64_binop(
    inst_ctor: impl FnOnce(Value, Value) -> Inst,
) -> (SelectedFunction, Value, Value, Value) {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let y = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let r = b.emit(entry, inst_ctor(x, y), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));
    let selected = select(&b.f);
    (selected, x, y, r)
}

/// Proves the SAME `Inst::Add` variant dispatches to FloatAdd (not
/// IntAdd) for f64 operands -- the exact risk this task's dispatch
/// exists to resolve correctly.
#[test]
fn select_lowers_float_add() {
    let (selected, x, y, r) = select_f64_binop(Inst::Add);
    assert_eq!(
        selected.insts[2],
        MachineInst::FloatAdd {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_float_sub() {
    let (selected, x, y, r) = select_f64_binop(Inst::Sub);
    assert_eq!(
        selected.insts[2],
        MachineInst::FloatSub {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_float_mul() {
    let (selected, x, y, r) = select_f64_binop(Inst::Mul);
    assert_eq!(
        selected.insts[2],
        MachineInst::FloatMul {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_float_div() {
    let (selected, x, y, r) = select_f64_binop(Inst::Div);
    assert_eq!(
        selected.insts[2],
        MachineInst::FloatDiv {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_int_neg() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let r = b.emit(entry, Inst::Neg(x), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    assert_eq!(selected.insts[1], MachineInst::IntNeg { dst: r, src: x });
}

/// Proves Neg's OTHER branch: an f64 operand mints a synthetic mask
/// temp and lowers to FloatNeg, not IntNeg -- the float counterpart to
/// select_lowers_int_neg above, both exercising the same dispatching
/// `Inst::Neg` arm.
#[test]
fn select_lowers_float_neg_via_a_synthetic_mask_temp() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let r = b.emit(entry, Inst::Neg(x), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    let mask_tmp = match &selected.insts[1] {
        MachineInst::LoadImmI64 { dst, imm } => {
            assert_eq!(*imm, i64::MIN);
            *dst
        }
        other => panic!("expected LoadImmI64 for the mask temp, got {:?}", other),
    };
    assert_eq!(
        selected.insts[2],
        MachineInst::FloatNeg {
            dst: r,
            src: x,
            mask_tmp
        }
    );
    assert_eq!(selected.synthetic_types.get(&mask_tmp), Some(&Ty::I64));
}

#[test]
fn select_lowers_not() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let r = b.emit(entry, Inst::Not(x), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    assert_eq!(selected.insts[1], MachineInst::Not { dst: r, src: x });
}

fn select_f64_unop(inst_ctor: impl FnOnce(Value) -> Inst) -> (SelectedFunction, Value, Value) {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let r = b.emit(entry, inst_ctor(x), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));
    let selected = select(&b.f);
    (selected, x, r)
}

#[test]
fn select_lowers_float_min() {
    let (selected, x, y, r) = select_f64_binop(Inst::Min);
    assert_eq!(
        selected.insts[2],
        MachineInst::FloatMin {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_float_max() {
    let (selected, x, y, r) = select_f64_binop(Inst::Max);
    assert_eq!(
        selected.insts[2],
        MachineInst::FloatMax {
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_sqrt() {
    let (selected, x, r) = select_f64_unop(Inst::Sqrt);
    assert_eq!(selected.insts[1], MachineInst::FloatSqrt { dst: r, src: x });
}

#[test]
fn select_lowers_floor() {
    let (selected, x, r) = select_f64_unop(Inst::Floor);
    assert_eq!(
        selected.insts[1],
        MachineInst::FloatRound {
            dst: r,
            src: x,
            mode: crate::RoundMode::Floor
        }
    );
}

#[test]
fn select_lowers_ceil() {
    let (selected, x, r) = select_f64_unop(Inst::Ceil);
    assert_eq!(
        selected.insts[1],
        MachineInst::FloatRound {
            dst: r,
            src: x,
            mode: crate::RoundMode::Ceil
        }
    );
}

#[test]
fn select_lowers_round() {
    let (selected, x, r) = select_f64_unop(Inst::Round);
    assert_eq!(
        selected.insts[1],
        MachineInst::FloatRound {
            dst: r,
            src: x,
            mode: crate::RoundMode::Nearest
        }
    );
}

#[test]
fn select_lowers_trunc() {
    let (selected, x, r) = select_f64_unop(Inst::Trunc);
    assert_eq!(
        selected.insts[1],
        MachineInst::FloatRound {
            dst: r,
            src: x,
            mode: crate::RoundMode::Truncate
        }
    );
}

#[test]
fn select_lowers_int_cmp() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let y = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let r = b.emit(
        entry,
        Inst::Cmp {
            op: CmpOp::Lt,
            lhs: x,
            rhs: y,
        },
        Ty::Bool,
        dummy_span(),
    );
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts[2],
        MachineInst::IntCmp {
            op: CmpOp::Lt,
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

/// Proves the dispatch reads the OPERAND's type, not dst's -- dst here
/// is Ty::Bool, identical to select_lowers_int_cmp's dst, so this test
/// only discriminates correctly if the match keys off `lhs`/`rhs`.
#[test]
fn select_lowers_float_cmp() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let y = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let r = b.emit(
        entry,
        Inst::Cmp {
            op: CmpOp::Lt,
            lhs: x,
            rhs: y,
        },
        Ty::Bool,
        dummy_span(),
    );
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts[2],
        MachineInst::FloatCmp {
            op: CmpOp::Lt,
            dst: r,
            lhs: x,
            rhs: y
        }
    );
}

#[test]
fn select_lowers_i_to_f() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let r = b.emit(entry, Inst::IToF(x), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts[1],
        MachineInst::IntToFloat { dst: r, src: x }
    );
}

#[test]
fn select_lowers_f_to_i() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let r = b.emit(entry, Inst::FToI(x), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts[1],
        MachineInst::FloatToInt { dst: r, src: x }
    );
}

#[test]
fn select_lowers_abs_via_a_synthetic_mask_temp() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let r = b.emit(entry, Inst::Abs(x), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    // insts[0] = Param x, insts[1] = LoadImmI64 for the mask temp,
    // insts[2] = FloatAbs, insts[3] = Return.
    let mask_tmp = match &selected.insts[1] {
        MachineInst::LoadImmI64 { dst, imm } => {
            assert_eq!(*imm, 0x7FFF_FFFF_FFFF_FFFFi64);
            *dst
        }
        other => panic!("expected LoadImmI64 for the mask temp, got {:?}", other),
    };
    assert_eq!(
        selected.insts[2],
        MachineInst::FloatAbs {
            dst: r,
            src: x,
            mask_tmp
        }
    );
    assert_eq!(selected.synthetic_types.get(&mask_tmp), Some(&Ty::I64));
    // The mask temp's Value must not collide with any real IR value --
    // the highest real Value index is `r` (the Abs instruction itself).
    assert!(mask_tmp.0 > r.0);
}

#[test]
fn select_lowers_fma_as_mul_then_add() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let y = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let z = b.emit(
        entry,
        Inst::Param {
            index: 2,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let r = b.emit(entry, Inst::Fma { a: x, b: y, c: z }, Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    // insts[0..3] = the three Params, insts[3] = FloatMul into a
    // synthetic temp, insts[4] = FloatAdd combining that temp with z.
    let mul_tmp = match &selected.insts[3] {
        MachineInst::FloatMul { dst, lhs, rhs } => {
            assert_eq!(*lhs, x);
            assert_eq!(*rhs, y);
            *dst
        }
        other => panic!("expected FloatMul, got {:?}", other),
    };
    assert_eq!(
        selected.insts[4],
        MachineInst::FloatAdd {
            dst: r,
            lhs: mul_tmp,
            rhs: z
        }
    );
    assert_eq!(selected.synthetic_types.get(&mul_tmp), Some(&Ty::F64));
}

/// A diamond CFG (entry branches to then/else, both jump to merge,
/// merge has a phi) -- confirms Phi produces NO MachineInst, per the
/// design doc's explicit deferral of phi resolution to Phase 8.
#[test]
fn select_emits_nothing_for_phi() {
    let mut b = Builder::new();
    let entry = b.create_block();
    let then_b = b.create_block();
    let else_b = b.create_block();
    let merge = b.create_block();
    b.add_pred(then_b, entry);
    b.add_pred(else_b, entry);
    b.seal_block(entry);
    b.seal_block(then_b);
    b.seal_block(else_b);
    b.add_pred(merge, then_b);
    b.add_pred(merge, else_b);
    b.seal_block(merge);

    let cond = b.emit(entry, Inst::ConstBool(true), Ty::Bool, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
        cond,
        then_: then_b,
        else_: else_b,
    });
    let t = b.emit(then_b, Inst::ConstI64(1), Ty::I64, dummy_span());
    b.f.blocks[then_b.0 as usize].term = Some(Terminator::Jump(merge));
    let e = b.emit(else_b, Inst::ConstI64(0), Ty::I64, dummy_span());
    b.f.blocks[else_b.0 as usize].term = Some(Terminator::Jump(merge));
    let phi = b.emit(
        merge,
        Inst::Phi {
            incoming: smallvec::smallvec![(then_b, t), (else_b, e)],
        },
        Ty::I64,
        dummy_span(),
    );
    b.f.blocks[merge.0 as usize].term = Some(Terminator::Return(phi));

    let selected = select(&b.f);

    // No MachineInst variant anywhere in the output should be a
    // stand-in "phi" op -- the only thing referencing `phi`'s Value at
    // all is the final Return.
    assert_eq!(
        selected.insts.last(),
        Some(&MachineInst::Return { value: phi })
    );
    let phi_producing_insts: Vec<_> = selected
        .insts
        .iter()
        .filter(|i| matches!(i, MachineInst::LoadImmI64 { dst, .. } if *dst == phi))
        .collect();
    assert!(
        phi_producing_insts.is_empty(),
        "Phi's destination Value must not be produced by any MachineInst in Phase 7a"
    );
}

#[test]
#[should_panic(expected = "Phase 7e")]
fn select_panics_on_call_with_a_clear_deferral_message() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let r = b.emit(
        entry,
        Inst::Call {
            func: forge_ir::LibFunc::Sin,
            args: smallvec::smallvec![x],
        },
        Ty::F64,
        dummy_span(),
    );
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    select(&b.f); // must panic
}

#[test]
fn coalescing_hints_binary_op_hints_dst_to_lhs_not_rhs() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let y = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let r = b.emit(entry, Inst::Sub(x, y), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    assert_eq!(selected.coalescing_hints.get(&r), Some(&x));
    assert_ne!(selected.coalescing_hints.get(&r), Some(&y));
}

#[test]
fn coalescing_hints_unary_op_hints_dst_to_src() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let r = b.emit(entry, Inst::Neg(x), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    assert_eq!(selected.coalescing_hints.get(&r), Some(&x));
}

#[test]
fn coalescing_hints_exclude_int_div_and_rem() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let y = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let d = b.emit(entry, Inst::Div(x, y), Ty::I64, dummy_span());
    let r = b.emit(entry, Inst::Rem(x, y), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    assert_eq!(selected.coalescing_hints.get(&d), None);
    assert_eq!(selected.coalescing_hints.get(&r), None);
}

#[test]
fn coalescing_hints_no_entry_for_ops_with_no_natural_hint() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let p = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(p));

    let selected = select(&b.f);

    assert_eq!(selected.coalescing_hints.get(&p), None);
}

/// Distinct from the test above: `Param` has a `dst` field that's
/// simply not one of the hinted variants. `Jump`/`Return` are a
/// structurally different case -- they have no `dst` at all, so
/// nothing about compute_coalescing_hints's match should even
/// consider a "hint" concept for them. Rust's exhaustive match plus
/// the `_ => {}` catch-all makes this safe by construction, but the
/// point is worth an explicit test rather than only inferring it.
#[test]
fn coalescing_hints_no_entry_for_terminators_with_no_dst_at_all() {
    let mut b = Builder::new();
    let entry = b.create_block();
    let target = b.create_block();
    b.add_pred(target, entry);
    b.seal_block(entry);
    b.seal_block(target);
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Jump(target));
    let p = b.emit(
        target,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    b.f.blocks[target.0 as usize].term = Some(Terminator::Return(p));

    let selected = select(&b.f);

    // Jump/Return themselves never produce a Value, so there's
    // nothing to look up in coalescing_hints for them directly --
    // this test's real assertion is simply that select() doesn't
    // panic or misbehave when the sequence contains dst-less
    // MachineInsts (Jump, Return), and that Param's own absence
    // still holds alongside them.
    assert_eq!(selected.coalescing_hints.get(&p), None);
    assert!(selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::Jump { .. })));
    assert!(selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::Return { .. })));
}

#[test]
fn lea_synthesis_mul_shape_operand_order_a() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let base_v = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let idx = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let four = b.emit(entry, Inst::ConstI64(4), Ty::I64, dummy_span());
    let mul = b.emit(entry, Inst::Mul(idx, four), Ty::I64, dummy_span());
    let add = b.emit(entry, Inst::Add(mul, base_v), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts[selected.insts.len() - 2],
        MachineInst::Lea {
            dst: add,
            base: base_v,
            index: idx,
            scale: 4,
            disp: 0
        }
    );
    // No standalone IntMul for `mul` -- it was fully subsumed by fusion.
    assert!(!selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::IntMul { dst, .. } if *dst == mul)));
}

#[test]
fn lea_synthesis_mul_shape_operand_order_b() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let base_v = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let idx = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let four = b.emit(entry, Inst::ConstI64(4), Ty::I64, dummy_span());
    let mul = b.emit(entry, Inst::Mul(idx, four), Ty::I64, dummy_span());
    let add = b.emit(entry, Inst::Add(base_v, mul), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts[selected.insts.len() - 2],
        MachineInst::Lea {
            dst: add,
            base: base_v,
            index: idx,
            scale: 4,
            disp: 0
        }
    );
    assert!(!selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::IntMul { dst, .. } if *dst == mul)));
}

#[test]
fn lea_synthesis_shl_shape() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let base_v = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let idx = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let two = b.emit(entry, Inst::ConstI64(2), Ty::I64, dummy_span());
    let shl = b.emit(entry, Inst::Shl(idx, two), Ty::I64, dummy_span());
    let add = b.emit(entry, Inst::Add(shl, base_v), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts[selected.insts.len() - 2],
        MachineInst::Lea {
            dst: add,
            base: base_v,
            index: idx,
            scale: 4,
            disp: 0
        }
    );
    assert!(!selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::Shl { dst, .. } if *dst == shl)));
}

#[test]
fn lea_synthesis_rejects_non_pow2_multiplier() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let base_v = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let idx = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let three = b.emit(entry, Inst::ConstI64(3), Ty::I64, dummy_span());
    let mul = b.emit(entry, Inst::Mul(idx, three), Ty::I64, dummy_span());
    let add = b.emit(entry, Inst::Add(mul, base_v), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

    let selected = select(&b.f);

    assert!(!selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::Lea { .. })));
    assert_eq!(
        selected.insts[3],
        MachineInst::IntMul {
            dst: mul,
            lhs: idx,
            rhs: three
        }
    );
    assert_eq!(
        selected.insts[4],
        MachineInst::IntAdd {
            dst: add,
            lhs: mul,
            rhs: base_v
        }
    );
}

#[test]
fn lea_synthesis_rejects_non_constant_multiplier() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let base_v = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let idx = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let other = b.emit(
        entry,
        Inst::Param {
            index: 2,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let mul = b.emit(entry, Inst::Mul(idx, other), Ty::I64, dummy_span());
    let add = b.emit(entry, Inst::Add(mul, base_v), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

    let selected = select(&b.f);

    assert!(!selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::Lea { .. })));
}

#[test]
fn lea_synthesis_rejects_out_of_range_shift() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let base_v = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let idx = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let four_shift = b.emit(entry, Inst::ConstI64(4), Ty::I64, dummy_span());
    let shl = b.emit(entry, Inst::Shl(idx, four_shift), Ty::I64, dummy_span());
    let add = b.emit(entry, Inst::Add(shl, base_v), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

    let selected = select(&b.f);

    assert!(!selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::Lea { .. })));
    assert_eq!(
        selected.insts[3],
        MachineInst::Shl {
            dst: shl,
            lhs: idx,
            rhs: four_shift
        }
    );
}

#[test]
fn lea_synthesis_shared_consumer_both_fuse_and_suppress() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let idx = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let c1 = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let c2 = b.emit(
        entry,
        Inst::Param {
            index: 2,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let two = b.emit(entry, Inst::ConstI64(2), Ty::I64, dummy_span());
    let shl = b.emit(entry, Inst::Shl(idx, two), Ty::I64, dummy_span());
    let add1 = b.emit(entry, Inst::Add(shl, c1), Ty::I64, dummy_span());
    let add2 = b.emit(entry, Inst::Add(shl, c2), Ty::I64, dummy_span());
    let sum = b.emit(entry, Inst::Add(add1, add2), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(sum));

    let selected = select(&b.f);

    let leas: Vec<_> = selected
        .insts
        .iter()
        .filter(|i| matches!(i, MachineInst::Lea { .. }))
        .collect();
    assert_eq!(leas.len(), 2);
    assert!(!selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::Shl { dst, .. } if *dst == shl)));
}

#[test]
fn lea_synthesis_escaping_use_prevents_suppression() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let base_v = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let idx = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let four = b.emit(entry, Inst::ConstI64(4), Ty::I64, dummy_span());
    let mul = b.emit(entry, Inst::Mul(idx, four), Ty::I64, dummy_span());
    let _add = b.emit(entry, Inst::Add(mul, base_v), Ty::I64, dummy_span());
    // `mul` is ALSO directly returned -- an escaping use, so it must
    // NOT be suppressed even though the Add fuses it.
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(mul));

    let selected = select(&b.f);

    assert!(selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::Lea { .. })));
    assert!(selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::IntMul { dst, .. } if *dst == mul)));
}

#[test]
fn lea_synthesis_mixed_shape_both_operands_fusable_prefers_a() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let y = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let four = b.emit(entry, Inst::ConstI64(4), Ty::I64, dummy_span());
    let three_shift = b.emit(entry, Inst::ConstI64(3), Ty::I64, dummy_span());
    let mul_x = b.emit(entry, Inst::Mul(x, four), Ty::I64, dummy_span());
    let shl_y = b.emit(entry, Inst::Shl(y, three_shift), Ty::I64, dummy_span());
    let add = b.emit(entry, Inst::Add(mul_x, shl_y), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

    let selected = select(&b.f);

    // mul_x (the `a` operand) is preferred as the fused index; shl_y
    // (the `b` operand) remains an ordinary Value used as `base`, and
    // gets its own independent, non-suppressed computation.
    assert_eq!(
        selected.insts[selected.insts.len() - 2],
        MachineInst::Lea {
            dst: add,
            base: shl_y,
            index: x,
            scale: 4,
            disp: 0
        }
    );
    assert!(selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::Shl { dst, .. } if *dst == shl_y)));
    assert!(!selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::IntMul { dst, .. } if *dst == mul_x)));
}

#[test]
fn lea_synthesis_self_referential_add_never_suppresses() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let idx = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let four = b.emit(entry, Inst::ConstI64(4), Ty::I64, dummy_span());
    let mul = b.emit(entry, Inst::Mul(idx, four), Ty::I64, dummy_span());
    let add = b.emit(entry, Inst::Add(mul, mul), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts[selected.insts.len() - 2],
        MachineInst::Lea {
            dst: add,
            base: mul,
            index: idx,
            scale: 4,
            disp: 0
        }
    );
    // mul's own register genuinely still needs to exist (it's the
    // Lea's `base` operand too) -- must NOT be suppressed.
    assert!(selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::IntMul { dst, .. } if *dst == mul)));
}
