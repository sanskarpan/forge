mod disasm;
use disasm::disassemble;

use forge_ir::{CmpOp, Value};
use forge_regalloc::Location;
use forge_x64::{Assembler, MachineInst, PhysReg};
use std::collections::HashMap;

fn loc_of(assignment: &HashMap<Value, Location>) -> impl Fn(Value) -> PhysReg + '_ {
    move |v| match assignment[&v] {
        Location::Reg(r) => r,
        Location::Spill(_) => panic!("test assignment must use only Location::Reg"),
    }
}

#[test]
fn int_cmp_lt_emits_cmp_zero_setl() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Rcx));
    assignment.insert(rhs, Location::Reg(PhysReg::Rdx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &MachineInst::IntCmp {
            op: CmpOp::Lt,
            dst,
            lhs,
            rhs,
        },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["cmp rcx,rdx", "mov rax,0", "setl al"]
    );
}

#[test]
fn int_cmp_dst_aliases_lhs_is_still_correct() {
    // dst and lhs share a register — the compare must read lhs BEFORE dst is zeroed.
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rcx));
    assignment.insert(lhs, Location::Reg(PhysReg::Rcx));
    assignment.insert(rhs, Location::Reg(PhysReg::Rdx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &MachineInst::IntCmp {
            op: CmpOp::Eq,
            dst,
            lhs,
            rhs,
        },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["cmp rcx,rdx", "mov rcx,0", "sete cl"]
    );
}

#[test]
fn int_cmp_ne_emits_setne_not_sete() {
    // Guards against a copy-paste swap of Ne onto Eq's condition code: without
    // this test, mapping CmpOp::Ne to ConditionCode::Equal survives the suite
    // undetected because no other test exercises Ne for IntCmp.
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Rcx));
    assignment.insert(rhs, Location::Reg(PhysReg::Rdx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &MachineInst::IntCmp {
            op: CmpOp::Ne,
            dst,
            lhs,
            rhs,
        },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["cmp rcx,rdx", "mov rax,0", "setne al"]
    );
}

#[test]
fn float_cmp_lt_uses_unsigned_below_condition() {
    // ucomisd sets flags like an unsigned integer compare; Lt must map to
    // Below, not Less, or the wrong branch is taken on unordered operands.
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Xmm0));
    assignment.insert(rhs, Location::Reg(PhysReg::Xmm1));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &MachineInst::FloatCmp {
            op: CmpOp::Lt,
            dst,
            lhs,
            rhs,
        },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["ucomisd xmm0,xmm1", "mov rax,0", "setb al"]
    );
}

#[test]
fn float_cmp_ne_emits_setne_not_sete() {
    // Same copy-paste-swap guard as int_cmp_ne_emits_setne_not_sete, but for
    // float_condition_code's Ne mapping.
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Xmm0));
    assignment.insert(rhs, Location::Reg(PhysReg::Xmm1));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &MachineInst::FloatCmp {
            op: CmpOp::Ne,
            dst,
            lhs,
            rhs,
        },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["ucomisd xmm0,xmm1", "mov rax,0", "setne al"]
    );
}

#[test]
fn int_cmov_picks_then_val_when_cond_true() {
    let dst = Value(0);
    let cond = Value(1);
    let then_val = Value(2);
    let else_val = Value(3);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(cond, Location::Reg(PhysReg::Rcx));
    assignment.insert(then_val, Location::Reg(PhysReg::Rax));
    assignment.insert(else_val, Location::Reg(PhysReg::Rdx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &MachineInst::IntCmov {
            dst,
            cond,
            then_val,
            else_val,
        },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["test rcx,rcx", "cmove rax,rdx"]
    );
}

#[test]
fn int_cmov_emits_mov_when_dst_differs_from_then_val() {
    // The existing int_cmov_picks_then_val_when_cond_true test happens to use
    // dst_r == then_r, which already exercises the elision path (no mov
    // emitted). This test covers the complementary dst_r != then_r path,
    // where the leading mov_reg_reg(dst, then_val) IS required to copy
    // then_val into dst before the cmov conditionally overwrites it with
    // else_val.
    let dst = Value(0);
    let cond = Value(1);
    let then_val = Value(2);
    let else_val = Value(3);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(cond, Location::Reg(PhysReg::Rcx));
    assignment.insert(then_val, Location::Reg(PhysReg::Rsi));
    assignment.insert(else_val, Location::Reg(PhysReg::Rdx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &MachineInst::IntCmov {
            dst,
            cond,
            then_val,
            else_val,
        },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["mov rax,rsi", "test rcx,rcx", "cmove rax,rdx"]
    );
}
