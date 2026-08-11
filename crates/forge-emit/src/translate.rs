use forge_ir::Value;
use forge_x64::{AluOp, Assembler, Label, MachineInst, PhysReg, SseOp};

fn alu_binop(
    asm: &mut Assembler,
    loc: &dyn Fn(Value) -> PhysReg,
    op: AluOp,
    dst: Value,
    lhs: Value,
    rhs: Value,
) {
    let (dst_r, lhs_r, rhs_r) = (loc(dst), loc(lhs), loc(rhs));
    if dst_r != lhs_r {
        asm.mov_reg_reg(dst_r, lhs_r);
    }
    asm.alu_reg_reg(op, dst_r, rhs_r);
}

fn sse_binop(
    asm: &mut Assembler,
    loc: &dyn Fn(Value) -> PhysReg,
    op: SseOp,
    dst: Value,
    lhs: Value,
    rhs: Value,
) {
    let (dst_r, lhs_r, rhs_r) = (loc(dst), loc(lhs), loc(rhs));
    if dst_r != lhs_r {
        asm.movsd_reg_reg(dst_r, lhs_r);
    }
    asm.sse_reg_reg(op, dst_r, rhs_r);
}

/// Translates one `MachineInst` (register-only operands, `Location::Reg` only)
/// into real bytes on `asm`. `loc` resolves a `Value` to the `PhysReg` holding
/// it. `pool_labels[i]` is the label for the constant-pool entry at index `i`
/// (see `alloc_pool_labels`/`place_pool`); must already be allocated (not
/// necessarily bound yet) before any instruction referencing the pool is
/// translated.
///
/// Phase 9a scope: `Param` and `CallLibm` are not yet implemented (Phase
/// 9b/9e). `IntDiv`/`IntRem` place the dividend/result correctly but do not
/// yet protect an unrelated value that happens to be resident in rax/rdx
/// (Phase 9b). `Shl`/`Shr`/`Sar` require the shift amount to already be in
/// `Rcx` (asserted) — displacing an occupied `Rcx` is Phase 9b's job.
/// `Jump`/`Branch`/`Return` are handled by `layout.rs`'s `emit_body` before
/// this function is ever called on them.
pub fn translate_inst(
    asm: &mut Assembler,
    inst: &MachineInst,
    loc: &dyn Fn(Value) -> PhysReg,
    pool_labels: &[Label],
) {
    match inst {
        MachineInst::LoadImmI64 { dst, imm } => asm.mov_reg_imm(loc(*dst), *imm),
        MachineInst::LoadImmF64 { dst, pool_index } => {
            asm.movsd_reg_riprel(loc(*dst), pool_labels[pool_index.index()])
        }

        MachineInst::IntAdd { dst, lhs, rhs } => alu_binop(asm, loc, AluOp::Add, *dst, *lhs, *rhs),
        MachineInst::IntSub { dst, lhs, rhs } => alu_binop(asm, loc, AluOp::Sub, *dst, *lhs, *rhs),
        MachineInst::And { dst, lhs, rhs } => alu_binop(asm, loc, AluOp::And, *dst, *lhs, *rhs),
        MachineInst::Or { dst, lhs, rhs } => alu_binop(asm, loc, AluOp::Or, *dst, *lhs, *rhs),
        MachineInst::Xor { dst, lhs, rhs } => alu_binop(asm, loc, AluOp::Xor, *dst, *lhs, *rhs),

        MachineInst::IntMul { dst, lhs, rhs } => {
            let (dst_r, lhs_r, rhs_r) = (loc(*dst), loc(*lhs), loc(*rhs));
            if dst_r != lhs_r {
                asm.mov_reg_reg(dst_r, lhs_r);
            }
            asm.imul_reg_reg(dst_r, rhs_r);
        }

        MachineInst::IntDiv { dst, lhs, rhs } => {
            let (dst_r, lhs_r, rhs_r) = (loc(*dst), loc(*lhs), loc(*rhs));
            if lhs_r != PhysReg::Rax {
                asm.mov_reg_reg(PhysReg::Rax, lhs_r);
            }
            asm.cqo();
            assert_div_rhs_not_rax_rdx(rhs_r);
            asm.idiv_reg(rhs_r);
            if dst_r != PhysReg::Rax {
                asm.mov_reg_reg(dst_r, PhysReg::Rax);
            }
        }
        MachineInst::IntRem { dst, lhs, rhs } => {
            let (dst_r, lhs_r, rhs_r) = (loc(*dst), loc(*lhs), loc(*rhs));
            if lhs_r != PhysReg::Rax {
                asm.mov_reg_reg(PhysReg::Rax, lhs_r);
            }
            asm.cqo();
            assert_div_rhs_not_rax_rdx(rhs_r);
            asm.idiv_reg(rhs_r);
            if dst_r != PhysReg::Rdx {
                asm.mov_reg_reg(dst_r, PhysReg::Rdx);
            }
        }

        MachineInst::IntNeg { dst, src } => {
            let (dst_r, src_r) = (loc(*dst), loc(*src));
            if dst_r != src_r {
                asm.mov_reg_reg(dst_r, src_r);
            }
            asm.neg_reg(dst_r);
        }
        MachineInst::Not { dst, src } => {
            let (dst_r, src_r) = (loc(*dst), loc(*src));
            if dst_r != src_r {
                asm.mov_reg_reg(dst_r, src_r);
            }
            asm.not_reg(dst_r);
        }

        MachineInst::Shl { dst, lhs, rhs } => {
            shift_op(asm, loc, forge_x64::ShiftOp::Shl, *dst, *lhs, *rhs)
        }
        MachineInst::Shr { dst, lhs, rhs } => {
            shift_op(asm, loc, forge_x64::ShiftOp::Shr, *dst, *lhs, *rhs)
        }
        MachineInst::Sar { dst, lhs, rhs } => {
            shift_op(asm, loc, forge_x64::ShiftOp::Sar, *dst, *lhs, *rhs)
        }

        MachineInst::Lea {
            dst,
            base,
            index,
            scale,
            disp,
        } => asm.lea_reg_scaled(loc(*dst), loc(*base), loc(*index), *scale, *disp),

        MachineInst::FloatAdd { dst, lhs, rhs } => {
            sse_binop(asm, loc, SseOp::Add, *dst, *lhs, *rhs)
        }
        MachineInst::FloatSub { dst, lhs, rhs } => {
            sse_binop(asm, loc, SseOp::Sub, *dst, *lhs, *rhs)
        }
        MachineInst::FloatMul { dst, lhs, rhs } => {
            sse_binop(asm, loc, SseOp::Mul, *dst, *lhs, *rhs)
        }
        MachineInst::FloatDiv { dst, lhs, rhs } => {
            sse_binop(asm, loc, SseOp::Div, *dst, *lhs, *rhs)
        }
        MachineInst::FloatMin { dst, lhs, rhs } => {
            sse_binop(asm, loc, SseOp::Min, *dst, *lhs, *rhs)
        }
        MachineInst::FloatMax { dst, lhs, rhs } => {
            sse_binop(asm, loc, SseOp::Max, *dst, *lhs, *rhs)
        }

        MachineInst::FloatSqrt { dst, src } => {
            let (dst_r, src_r) = (loc(*dst), loc(*src));
            if dst_r != src_r {
                asm.movsd_reg_reg(dst_r, src_r);
            }
            asm.sse_reg_reg(SseOp::Sqrt, dst_r, dst_r);
        }
        MachineInst::FloatRound { dst, src, mode } => asm.roundsd(*mode, loc(*dst), loc(*src)),

        MachineInst::FloatAbs {
            dst,
            src,
            mask_pool,
        } => float_mask_op(asm, loc, pool_labels, *dst, *src, *mask_pool, MaskOp::Abs),
        MachineInst::FloatNeg {
            dst,
            src,
            mask_pool,
        } => float_mask_op(asm, loc, pool_labels, *dst, *src, *mask_pool, MaskOp::Neg),

        MachineInst::IntToFloat { dst, src } => asm.cvtsi2sd(loc(*dst), loc(*src)),
        MachineInst::FloatToInt { dst, src } => asm.cvttsd2si(loc(*dst), loc(*src)),

        MachineInst::IntCmp { .. } | MachineInst::FloatCmp { .. } | MachineInst::IntCmov { .. } => {
            unimplemented!("filled in by Task 5: {inst:?}")
        }

        MachineInst::Param { .. } => {
            panic!("forge-emit (Phase 9a): Param placement not yet implemented — Phase 9b")
        }
        MachineInst::CallLibm { .. } => {
            panic!("forge-emit (Phase 9a): CallLibm sequence not yet implemented — Phase 9e")
        }

        MachineInst::Jump { .. } | MachineInst::Branch { .. } | MachineInst::Return { .. } => {
            unreachable!(
                "forge-emit: {inst:?} is a terminator, handled by layout.rs::emit_body before \
                 translate_inst is ever called on it"
            )
        }
    }
}

/// `idiv_reg`'s divisor operand must not itself be Rax/Rdx: `cqo` has already
/// overwritten Rdx as the sign-extension of Rax by this point, so a divisor
/// aliasing either would read corrupted/wrong-purpose data. The real
/// allocator's `excluded_registers()` keeps this from happening in practice;
/// this assert exists so a malformed hand-built test (or a future caller)
/// fails loudly instead of silently computing garbage.
fn assert_div_rhs_not_rax_rdx(rhs_r: PhysReg) {
    assert!(
        rhs_r != PhysReg::Rax && rhs_r != PhysReg::Rdx,
        "forge-emit (Phase 9a): IntDiv/IntRem divisor must not be Rax/Rdx — the real allocator's \
         excluded_registers() prevents this; this input is malformed"
    );
}

fn shift_op(
    asm: &mut Assembler,
    loc: &dyn Fn(Value) -> PhysReg,
    op: forge_x64::ShiftOp,
    dst: Value,
    lhs: Value,
    rhs: Value,
) {
    let (dst_r, lhs_r, rhs_r) = (loc(dst), loc(lhs), loc(rhs));
    if dst_r != lhs_r {
        asm.mov_reg_reg(dst_r, lhs_r);
    }
    assert_eq!(
        rhs_r,
        PhysReg::Rcx,
        "forge-emit (Phase 9a): shift amount not in RCX/CL — displacing an occupied RCX is \
         Phase 9b's job"
    );
    asm.shift_reg_cl(op, dst_r);
}

enum MaskOp {
    Abs,
    Neg,
}

fn float_mask_op(
    asm: &mut Assembler,
    loc: &dyn Fn(Value) -> PhysReg,
    pool_labels: &[Label],
    dst: Value,
    src: Value,
    mask_pool: forge_x64::PoolIndex,
    op: MaskOp,
) {
    let (dst_r, src_r) = (loc(dst), loc(src));
    if dst_r != src_r {
        asm.movsd_reg_reg(dst_r, src_r);
    }
    asm.movsd_reg_riprel(PhysReg::Xmm14, pool_labels[mask_pool.index()]);
    match op {
        MaskOp::Abs => asm.andpd_reg_reg(dst_r, PhysReg::Xmm14),
        MaskOp::Neg => asm.xorpd_reg_reg(dst_r, PhysReg::Xmm14),
    }
}
