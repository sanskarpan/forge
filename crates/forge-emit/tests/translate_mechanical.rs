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
fn int_sub_emits_two_addr_mov_then_sub() {
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
        &forge_x64::MachineInst::IntSub { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    let lines = disassemble(asm.code());
    assert_eq!(lines, vec!["mov rax,rcx", "sub rax,rdx"]);
}

#[test]
fn and_or_xor_use_the_matching_alu_op() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Rax));
    assignment.insert(rhs, Location::Reg(PhysReg::Rdx));
    let loc = loc_of(&assignment);

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::And { dst, lhs, rhs },
        &loc,
        &[],
    );
    assert_eq!(disassemble(asm.code()), vec!["and rax,rdx"]);

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::Or { dst, lhs, rhs },
        &loc,
        &[],
    );
    assert_eq!(disassemble(asm.code()), vec!["or rax,rdx"]);

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::Xor { dst, lhs, rhs },
        &loc,
        &[],
    );
    assert_eq!(disassemble(asm.code()), vec!["xor rax,rdx"]);
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
fn int_neg_uses_dst_as_src_and_negates() {
    let dst = Value(0);
    let src = Value(1);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(src, Location::Reg(PhysReg::Rcx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::IntNeg { dst, src },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(disassemble(asm.code()), vec!["mov rax,rcx", "neg rax"]);
}

#[test]
fn not_emits_bitwise_not() {
    let dst = Value(0);
    let src = Value(1);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(src, Location::Reg(PhysReg::Rcx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::Not { dst, src },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(disassemble(asm.code()), vec!["mov rax,rcx", "not rax"]);
}

#[test]
fn shr_and_sar_use_the_matching_shift_op() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Rax));
    assignment.insert(rhs, Location::Reg(PhysReg::Rcx));
    let loc = loc_of(&assignment);

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::Shr { dst, lhs, rhs },
        &loc,
        &[],
    );
    assert_eq!(disassemble(asm.code()), vec!["shr rax,cl"]);

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::Sar { dst, lhs, rhs },
        &loc,
        &[],
    );
    assert_eq!(disassemble(asm.code()), vec!["sar rax,cl"]);
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
fn shl_moves_amount_into_rcx_before_shifting() {
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
    assert_eq!(disassemble(asm.code()), vec!["mov rcx,rdx", "shl rax,cl"]);
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
fn float_binops_use_the_matching_sse_op() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Xmm0));
    assignment.insert(lhs, Location::Reg(PhysReg::Xmm0));
    assignment.insert(rhs, Location::Reg(PhysReg::Xmm2));
    let loc = loc_of(&assignment);

    let cases: &[(forge_x64::MachineInst, &str)] = &[
        (
            forge_x64::MachineInst::FloatSub { dst, lhs, rhs },
            "subsd xmm0,xmm2",
        ),
        (
            forge_x64::MachineInst::FloatMul { dst, lhs, rhs },
            "mulsd xmm0,xmm2",
        ),
        (
            forge_x64::MachineInst::FloatDiv { dst, lhs, rhs },
            "divsd xmm0,xmm2",
        ),
    ];

    for (inst, expected) in cases {
        let mut asm = Assembler::new();
        forge_emit::translate_inst(&mut asm, inst, &loc, &[]);
        // dst == lhs here (both Xmm0), so the two-address `movsd` is
        // elided and only the op mnemonic should appear.
        assert_eq!(disassemble(asm.code()), vec![expected.to_string()]);
    }

    for (inst, op) in [
        (
            forge_x64::MachineInst::FloatMin { dst, lhs, rhs },
            "minsd xmm0,xmm2",
        ),
        (
            forge_x64::MachineInst::FloatMax { dst, lhs, rhs },
            "maxsd xmm0,xmm2",
        ),
    ] {
        let mut asm = Assembler::new();
        forge_emit::translate_inst(&mut asm, &inst, &loc, &[]);
        let actual = disassemble(asm.code());
        assert_eq!(actual.len(), 7);
        assert_eq!(actual[0], "ucomisd xmm0,xmm0");
        assert!(actual[1].starts_with("jp near "));
        assert_eq!(actual[2], "ucomisd xmm2,xmm2");
        assert!(actual[3].starts_with("jp near "));
        assert_eq!(actual[4], op);
        assert!(actual[5].starts_with("jmp "));
        assert_eq!(actual[6], "movsd xmm0,xmm2");
    }
}

#[test]
fn sse_binop_elides_movsd_when_dst_already_equals_lhs() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Xmm3));
    assignment.insert(lhs, Location::Reg(PhysReg::Xmm3));
    assignment.insert(rhs, Location::Reg(PhysReg::Xmm4));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::FloatAdd { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    // Only the addsd should be emitted -- no movsd, since dst already
    // equals lhs (Xmm3 in both cases).
    assert_eq!(disassemble(asm.code()), vec!["addsd xmm3,xmm4"]);
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
    assert!(lines[1].starts_with("movsd xmm13,"), "got: {}", lines[1]);
    assert_eq!(lines[2], "andpd xmm0,xmm13");
}

#[test]
fn float_neg_flips_sign_bit_via_pool_mask() {
    let dst = Value(0);
    let src = Value(1);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Xmm0));
    assignment.insert(src, Location::Reg(PhysReg::Xmm1));

    let mut pool = forge_x64::ConstantPool::default();
    let mask = pool.intern(0x8000000000000000u64);

    let mut asm = Assembler::new();
    let labels = forge_emit::alloc_pool_labels(&mut asm, &pool);
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::FloatNeg {
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
    assert!(lines[1].starts_with("movsd xmm13,"), "got: {}", lines[1]);
    // FloatNeg must use xorpd (flip the sign bit), not andpd (FloatAbs's
    // mnemonic) -- this is the bit that distinguishes it from FloatAbs.
    assert_eq!(lines[2], "xorpd xmm0,xmm13");
}

#[test]
fn float_round_passes_mode_through_to_roundsd() {
    let dst = Value(0);
    let src = Value(1);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Xmm0));
    assignment.insert(src, Location::Reg(PhysReg::Xmm1));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::FloatRound {
            dst,
            src,
            mode: forge_x64::RoundMode::Floor,
        },
        &loc_of(&assignment),
        &[],
    );

    let lines = disassemble(asm.code());
    assert_eq!(lines.len(), 2);
    assert!(lines[0] == "movsd xmm0,xmm1", "got: {}", lines[0]);
    assert!(
        lines[1].starts_with("roundsd xmm0,xmm0,"),
        "got: {}",
        lines[1]
    );
}

#[test]
fn int_to_float_and_float_to_int_use_the_matching_convert_op() {
    let dst = Value(0);
    let src = Value(1);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Xmm0));
    assignment.insert(src, Location::Reg(PhysReg::Rax));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::IntToFloat { dst, src },
        &loc_of(&assignment),
        &[],
    );
    assert_eq!(disassemble(asm.code()), vec!["cvtsi2sd xmm0,rax"]);

    let dst2 = Value(2);
    let src2 = Value(3);
    let mut assignment2 = HashMap::new();
    assignment2.insert(dst2, Location::Reg(PhysReg::Rax));
    assignment2.insert(src2, Location::Reg(PhysReg::Xmm0));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::FloatToInt {
            dst: dst2,
            src: src2,
        },
        &loc_of(&assignment2),
        &[],
    );
    assert_eq!(disassemble(asm.code()), vec!["cvttsd2si rax,xmm0"]);
}

#[test]
fn load_imm_i64_moves_the_immediate_into_dst() {
    let dst = Value(0);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::LoadImmI64 { dst, imm: 42 },
        &loc_of(&assignment),
        &[],
    );

    // NASM formatter renders immediates in hex.
    assert_eq!(disassemble(asm.code()), vec!["mov rax,2Ah"]);
}

#[test]
fn param_is_copied_from_the_integer_abi_register() {
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
    assert_eq!(disassemble(asm.code()), vec!["mov rax,rdi"]);
}

#[test]
fn call_libm_emits_aligned_indirect_call() {
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
    let lines = disassemble(asm.code());
    assert!(lines.iter().any(|line| line == "sub rsp,8"));
    assert!(lines.iter().any(|line| line.starts_with("call ")));
    assert!(lines.iter().any(|line| line == "add rsp,8"));
}
