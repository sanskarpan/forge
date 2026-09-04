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

/// Translates one `MachineInst` whose virtual operands have already been
/// materialized in physical registers by the layout emitter.
/// into real bytes on `asm`. `loc` resolves a `Value` to the `PhysReg` holding
/// it. `pool_labels[i]` is the label for the constant-pool entry at index `i`
/// (see `alloc_pool_labels`/`place_pool`); must already be allocated (not
/// necessarily bound yet) before any instruction referencing the pool is
/// translated.
///
/// `Param` copies from the ABI's incoming register, and `CallLibm` emits a
/// correctly aligned indirect call. The layout emitter surrounds calls and
/// implicit-register instructions with the required live-register saves.
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
        MachineInst::FloatRound { dst, src, mode } => {
            let (dst_r, src_r) = (loc(*dst), loc(*src));
            if dst_r != src_r {
                asm.movsd_reg_reg(dst_r, src_r);
            }
            asm.roundsd(*mode, dst_r, dst_r);
        }

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

        MachineInst::IntCmp { op, dst, lhs, rhs } => {
            let (dst_r, lhs_r, rhs_r) = (loc(*dst), loc(*lhs), loc(*rhs));
            asm.alu_reg_reg(AluOp::Cmp, lhs_r, rhs_r);
            asm.mov_reg_imm(dst_r, 0);
            asm.setcc(int_condition_code(*op), dst_r);
        }
        MachineInst::FloatCmp { op, dst, lhs, rhs } => {
            let (dst_r, lhs_r, rhs_r) = (loc(*dst), loc(*lhs), loc(*rhs));
            asm.ucomisd_reg_reg(lhs_r, rhs_r);
            asm.mov_reg_imm(dst_r, 0);
            asm.setcc(float_condition_code(*op), dst_r);
        }
        MachineInst::IntCmov {
            dst,
            cond,
            then_val,
            else_val,
        } => {
            let (dst_r, cond_r, then_r, else_r) =
                (loc(*dst), loc(*cond), loc(*then_val), loc(*else_val));
            if dst_r != then_r {
                asm.mov_reg_reg(dst_r, then_r);
            }
            asm.test_reg_reg(cond_r, cond_r);
            asm.cmovcc(forge_x64::ConditionCode::Equal, dst_r, else_r);
        }

        MachineInst::Param { dst, index } => {
            let dst_r = loc(*dst);
            let src_r = if is_xmm(dst_r) {
                forge_regalloc::SYSV_FLOAT_ARGS
                    .get(*index as usize)
                    .copied()
                    .unwrap_or_else(|| panic!("float parameter index {index} exceeds SysV ABI"))
            } else {
                forge_regalloc::SYSV_INT_ARGS
                    .get(*index as usize)
                    .copied()
                    .unwrap_or_else(|| panic!("integer parameter index {index} exceeds SysV ABI"))
            };
            if dst_r != src_r {
                if is_xmm(dst_r) {
                    asm.movsd_reg_reg(dst_r, src_r);
                } else {
                    asm.mov_reg_reg(dst_r, src_r);
                }
            }
        }
        MachineInst::CallLibm { dst, func, args } => {
            assert!(
                !args.is_empty() && args.len() <= 2,
                "libm call has invalid arity"
            );
            let dst_r = loc(*dst);
            let arg_regs: Vec<PhysReg> = args.iter().map(|v| loc(*v)).collect();
            // Handle the only two-argument register swap without clobbering
            // either source. Xmm15 is reserved by the allocator for scratch
            // traffic and therefore cannot contain another live value.
            if arg_regs.len() == 2 && arg_regs[0] == PhysReg::Xmm1 && arg_regs[1] == PhysReg::Xmm0 {
                asm.movsd_reg_reg(PhysReg::Xmm15, PhysReg::Xmm0);
                asm.movsd_reg_reg(PhysReg::Xmm0, PhysReg::Xmm1);
                asm.movsd_reg_reg(PhysReg::Xmm1, PhysReg::Xmm15);
            } else {
                for (i, &src) in arg_regs.iter().enumerate() {
                    let abi = [PhysReg::Xmm0, PhysReg::Xmm1][i];
                    if src != abi {
                        asm.movsd_reg_reg(abi, src);
                    }
                }
            }
            // A JIT function enters SysV with RSP % 16 == 8. Keep the
            // caller's stack aligned immediately before CALL.
            asm.alu_reg_imm(AluOp::Sub, PhysReg::Rsp, 8);
            asm.mov_reg_imm(PhysReg::R11, forge_x64::libm_address(*func));
            asm.call_reg(PhysReg::R11);
            asm.alu_reg_imm(AluOp::Add, PhysReg::Rsp, 8);
            assert!(
                is_xmm(dst_r),
                "libm result must be assigned to an XMM register"
            );
            if dst_r != PhysReg::Xmm0 {
                asm.movsd_reg_reg(dst_r, PhysReg::Xmm0);
            }
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
        "forge-emit: IntDiv/IntRem divisor must not be Rax/Rdx — the real allocator's \
         excluded_registers() prevents this; this input is malformed"
    );
}

fn int_condition_code(op: forge_ir::CmpOp) -> forge_x64::ConditionCode {
    use forge_ir::CmpOp;
    use forge_x64::ConditionCode;
    match op {
        CmpOp::Eq => ConditionCode::Equal,
        CmpOp::Ne => ConditionCode::NotEqual,
        CmpOp::Lt => ConditionCode::Less,
        CmpOp::Le => ConditionCode::LessOrEqual,
        CmpOp::Gt => ConditionCode::Greater,
        CmpOp::Ge => ConditionCode::GreaterOrEqual,
    }
}

fn float_condition_code(op: forge_ir::CmpOp) -> forge_x64::ConditionCode {
    use forge_ir::CmpOp;
    use forge_x64::ConditionCode;
    match op {
        CmpOp::Eq => ConditionCode::Equal,
        CmpOp::Ne => ConditionCode::NotEqual,
        CmpOp::Lt => ConditionCode::Below,
        CmpOp::Le => ConditionCode::BelowOrEqual,
        CmpOp::Gt => ConditionCode::Above,
        CmpOp::Ge => ConditionCode::AboveOrEqual,
    }
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
    if rhs_r != PhysReg::Rcx {
        assert_ne!(
            dst_r,
            PhysReg::Rcx,
            "variable shift destination cannot be RCX when its count is elsewhere"
        );
        asm.mov_reg_reg(PhysReg::Rcx, rhs_r);
    }
    asm.shift_reg_cl(op, dst_r);
}

fn is_xmm(reg: PhysReg) -> bool {
    matches!(
        reg,
        PhysReg::Xmm0
            | PhysReg::Xmm1
            | PhysReg::Xmm2
            | PhysReg::Xmm3
            | PhysReg::Xmm4
            | PhysReg::Xmm5
            | PhysReg::Xmm6
            | PhysReg::Xmm7
            | PhysReg::Xmm8
            | PhysReg::Xmm9
            | PhysReg::Xmm10
            | PhysReg::Xmm11
            | PhysReg::Xmm12
            | PhysReg::Xmm13
            | PhysReg::Xmm14
            | PhysReg::Xmm15
            | PhysReg::Xmm16
            | PhysReg::Xmm17
            | PhysReg::Xmm18
            | PhysReg::Xmm19
            | PhysReg::Xmm20
            | PhysReg::Xmm21
            | PhysReg::Xmm22
            | PhysReg::Xmm23
            | PhysReg::Xmm24
            | PhysReg::Xmm25
            | PhysReg::Xmm26
            | PhysReg::Xmm27
            | PhysReg::Xmm28
            | PhysReg::Xmm29
            | PhysReg::Xmm30
            | PhysReg::Xmm31
    )
}

/// Which bitwise sign-mask operation `float_mask_op` should apply: `Abs`
/// clears the sign bit (`andpd`), `Neg` flips it (`xorpd`). See
/// `float_mask_op`'s doc comment for why both share one helper.
enum MaskOp {
    Abs,
    Neg,
}

/// Shared lowering for `FloatAbs`/`FloatNeg`: both are "load a 128-bit sign
/// mask from the constant pool, then `andpd`/`xorpd` it into `dst`" and
/// differ only in which bitwise op is used, so `op` picks that.
///
/// The mask is loaded into `PhysReg::Xmm13` — hardcoded, not resolved via
/// `loc`. This is not an arbitrary choice: `Xmm13` is
/// `forge_regalloc::linear_scan::SCRATCH_XMM[0]`
/// (`SCRATCH_XMM = [PhysReg::Xmm13, PhysReg::Xmm14, PhysReg::Xmm15]`), the
/// first register the real allocator reserves as scratch and never assigns
/// to a live `Value` across a `FloatAbs`/`FloatNeg` instruction. That
/// invariant is what makes it safe to clobber `Xmm13` here without going
/// through `loc`/consulting
/// liveness — if `forge-regalloc` ever reorders or changes `SCRATCH_XMM`,
/// this hardcoded literal would silently stop matching the allocator's
/// reserved register and this function could clobber a live value.
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
    // Xmm13 == forge_regalloc::linear_scan::SCRATCH_XMM[0]; see doc comment
    // above for the invariant this depends on.
    asm.movsd_reg_riprel(PhysReg::Xmm13, pool_labels[mask_pool.index()]);
    match op {
        MaskOp::Abs => asm.andpd_reg_reg(dst_r, PhysReg::Xmm13),
        MaskOp::Neg => asm.xorpd_reg_reg(dst_r, PhysReg::Xmm13),
    }
}
