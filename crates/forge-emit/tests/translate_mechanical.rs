mod disasm;
use disasm::disassemble;

use forge_ir::Value;
use forge_regalloc::Location;
use forge_x64::{Assembler, PhysReg};
use std::collections::HashMap;

fn loc_of(assignment: &HashMap<Value, Location>) -> impl Fn(Value) -> PhysReg + '_ {
    move |v| match assignment[&v] {
        Location::Reg(r) => r,
        Location::Spill(_) => panic!("test assignment must use only Location::Reg"),
    }
}

#[test]
fn int_add_emits_two_addr_mov_then_add() {
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
        &forge_x64::MachineInst::IntAdd { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    let lines = disassemble(asm.code());
    assert_eq!(lines, vec!["mov rax,rcx", "add rax,rdx"]);
}

#[test]
fn int_add_elides_mov_when_dst_already_equals_lhs() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Rax));
    assignment.insert(rhs, Location::Reg(PhysReg::Rdx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::IntAdd { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(disassemble(asm.code()), vec!["add rax,rdx"]);
}

#[test]
fn int_mul_uses_imul() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Rax));
    assignment.insert(rhs, Location::Reg(PhysReg::Rbx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::IntMul { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(disassemble(asm.code()), vec!["imul rax,rbx"]);
}

#[test]
fn int_div_places_dividend_in_rax_and_result_out() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rbx));
    assignment.insert(lhs, Location::Reg(PhysReg::Rcx));
    assignment.insert(rhs, Location::Reg(PhysReg::Rsi));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::IntDiv { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["mov rax,rcx", "cqo", "idiv rsi", "mov rbx,rax"]
    );
}

#[test]
fn int_rem_reads_result_from_rdx() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax)); // already rax, but IntRem wants Rdx->dst
    assignment.insert(lhs, Location::Reg(PhysReg::Rax));
    assignment.insert(rhs, Location::Reg(PhysReg::Rsi));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::IntRem { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["cqo", "idiv rsi", "mov rax,rdx"]
    );
}

#[test]
fn shl_with_amount_already_in_rcx_emits_shift_cl() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Rax));
    assignment.insert(rhs, Location::Reg(PhysReg::Rcx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::Shl { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(disassemble(asm.code()), vec!["shl rax,cl"]);
}

#[test]
#[should_panic(expected = "shift amount not in RCX")]
fn shl_with_amount_not_in_rcx_panics() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Rax));
    assignment.insert(rhs, Location::Reg(PhysReg::Rdx)); // NOT Rcx

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::Shl { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );
}

#[test]
fn lea_encodes_scaled_addressing() {
    let dst = Value(0);
    let base = Value(1);
    let index = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(base, Location::Reg(PhysReg::Rcx));
    assignment.insert(index, Location::Reg(PhysReg::Rdx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::Lea {
            dst,
            base,
            index,
            scale: 4,
            disp: 8,
        },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(disassemble(asm.code()), vec!["lea rax,[rcx+rdx*4+8]"]);
}

#[test]
fn float_add_emits_two_addr_movsd_then_addsd() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Xmm0));
    assignment.insert(lhs, Location::Reg(PhysReg::Xmm1));
    assignment.insert(rhs, Location::Reg(PhysReg::Xmm2));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::FloatAdd { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["movsd xmm0,xmm1", "addsd xmm0,xmm2"]
    );
}

#[test]
fn float_sqrt_uses_dst_as_both_operands() {
    let dst = Value(0);
    let src = Value(1);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Xmm0));
    assignment.insert(src, Location::Reg(PhysReg::Xmm1));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::FloatSqrt { dst, src },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["movsd xmm0,xmm1", "sqrtsd xmm0,xmm0"]
    );
}

#[test]
fn load_imm_f64_reads_from_pool_via_riprel() {
    let dst = Value(0);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Xmm0));

    let mut pool = forge_x64::ConstantPool::default();
    let idx = pool.intern(0x3ff0000000000000u64);

    let mut asm = Assembler::new();
    let labels = forge_emit::alloc_pool_labels(&mut asm, &pool);
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::LoadImmF64 {
            dst,
            pool_index: idx,
        },
        &loc_of(&assignment),
        &labels,
    );
    forge_emit::place_pool(&mut asm, &pool, &labels);

    let lines = disassemble(asm.code());
    // Only assert on lines[0]: the pool's raw data bytes physically follow
    // the code in the same buffer, and iced-x86 (a decoder, not a
    // code/data-aware disassembler) happily decodes those trailing bytes as
    // further "instructions" too. That's expected, not a bug -- the real
    // encoded instruction under test is lines[0].
    assert!(lines[0].starts_with("movsd xmm0,"), "got: {}", lines[0]);
}

#[test]
fn float_abs_clears_sign_bit_via_pool_mask() {
    let dst = Value(0);
    let src = Value(1);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Xmm0));
    assignment.insert(src, Location::Reg(PhysReg::Xmm1));

    let mut pool = forge_x64::ConstantPool::default();
    let mask = pool.intern(0x7fffffffffffffffu64);

    let mut asm = Assembler::new();
    let labels = forge_emit::alloc_pool_labels(&mut asm, &pool);
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::FloatAbs {
            dst,
            src,
            mask_pool: mask,
        },
        &loc_of(&assignment),
        &labels,
    );
    forge_emit::place_pool(&mut asm, &pool, &labels);

    let lines = disassemble(asm.code());
    assert_eq!(lines[0], "movsd xmm0,xmm1");
    assert!(lines[1].starts_with("movsd xmm14,"), "got: {}", lines[1]);
    assert_eq!(lines[2], "andpd xmm0,xmm14");
}

#[test]
#[should_panic(expected = "Param placement not yet implemented")]
fn param_panics_in_this_slice() {
    let dst = Value(0);
    let assignment: HashMap<Value, Location> =
        [(dst, Location::Reg(PhysReg::Rax))].into_iter().collect();
    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::Param { dst, index: 0 },
        &loc_of(&assignment),
        &[],
    );
}

#[test]
#[should_panic(expected = "CallLibm sequence not yet implemented")]
fn call_libm_panics_in_this_slice() {
    let dst = Value(0);
    let assignment: HashMap<Value, Location> =
        [(dst, Location::Reg(PhysReg::Xmm0))].into_iter().collect();
    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::CallLibm {
            dst,
            func: forge_ir::LibFunc::Sin,
            args: smallvec::smallvec![dst],
        },
        &loc_of(&assignment),
        &[],
    );
}
