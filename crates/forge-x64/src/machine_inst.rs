use forge_ir::{Block, CmpOp, Function, Inst, Terminator, Ty, Value};
use std::collections::HashMap;

/// A machine-level instruction operating on virtual registers (SSA Values,
/// reused directly from forge-ir -- one virtual register per SSA value, no
/// separate VReg type). Sits between forge-ir's `Inst` and forge-x64's
/// `Assembler` calls. Still in 3-address SSA form: `dst` is always a fresh
/// value distinct from its operands, even for opcodes that are 2-address-
/// destructive on real x86 (IntAdd/FloatAdd/And/etc). Two-address fixup
/// (Phase 7b) doesn't rewrite this form -- it only attaches coalescing
/// hints, consumed later by Phase 8's allocator and by the final
/// MachineInst-to-bytes emission step (built once Phase 8 exists), which
/// decides whether an actual copy is needed based on real register
/// assignments.
#[derive(Clone, Debug, PartialEq)]
pub enum MachineInst {
    // Constants (ConstBool lowers through LoadImmI64 as 0/1)
    LoadImmI64 {
        dst: Value,
        imm: i64,
    },
    LoadImmF64 {
        dst: Value,
        bits: u64,
    },

    // Integer arithmetic -- destructive (dst must end up == lhs's location)
    IntAdd {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    IntSub {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    IntMul {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    IntDiv {
        dst: Value,
        lhs: Value,
        rhs: Value,
    }, // cqo + idiv; RAX/RDX-fixed, Phase 8's concern
    IntRem {
        dst: Value,
        lhs: Value,
        rhs: Value,
    }, // same shape, takes RDX instead of RAX
    IntNeg {
        dst: Value,
        src: Value,
    },
    And {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    Or {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    Xor {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    Not {
        dst: Value,
        src: Value,
    },
    Shl {
        dst: Value,
        lhs: Value,
        rhs: Value,
    }, // rhs must end up in CL, Phase 8's concern
    Shr {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    Sar {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },

    // Float arithmetic -- destructive (dst must end up == lhs's location)
    FloatAdd {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    FloatSub {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    FloatMul {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    FloatDiv {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    FloatSqrt {
        dst: Value,
        src: Value,
    },
    FloatMin {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    FloatMax {
        dst: Value,
        lhs: Value,
        rhs: Value,
    },
    FloatRound {
        dst: Value,
        src: Value,
        mode: crate::RoundMode,
    },

    // Abs/Neg on floats: mask_tmp is a synthetic I64 Value holding the
    // sign-mask constant, minted by the selector -- see machine_inst.rs's
    // Fma/Abs/Neg lowering for why this field exists (it lets the
    // post-Phase-8 emission step synthesize the exact movq+andpd/xorpd
    // sequence once mask_tmp's and dst's real registers are known).
    FloatAbs {
        dst: Value,
        src: Value,
        mask_tmp: Value,
    },
    FloatNeg {
        dst: Value,
        src: Value,
        mask_tmp: Value,
    },

    // Comparisons -- resolved to a specific strategy at selection time
    IntCmp {
        op: CmpOp,
        dst: Value,
        lhs: Value,
        rhs: Value,
    }, // cmp + setcc, signed codes
    FloatCmp {
        op: CmpOp,
        dst: Value,
        lhs: Value,
        rhs: Value,
    }, // ucomisd + setcc, UNSIGNED codes

    // Conversions
    IntToFloat {
        dst: Value,
        src: Value,
    },
    FloatToInt {
        dst: Value,
        src: Value,
    }, // truncating (cvttsd2si)

    // Control flow
    Jump {
        target: Block,
    },
    Branch {
        cond: Value,
        then_: Block,
        else_: Block,
    },
    Return {
        value: Value,
    },

    // Parameters
    Param {
        dst: Value,
        index: u32,
    },
}

/// The result of instruction selection: a flat MachineInst sequence plus
/// the Ty of every virtual register the selector minted that ISN'T a real
/// IR value (i.e. every synthetic temp -- Fma's mul_tmp, Abs/Neg's
/// mask_tmp). Phase 8 needs this to know GPR-vs-XMM class for registers
/// `func.types` doesn't cover; real IR values look their Ty up in
/// `func.types` directly via this module's own `ty_of` helper.
pub struct SelectedFunction {
    pub insts: Vec<MachineInst>,
    pub synthetic_types: HashMap<Value, Ty>,
    /// dst -> the Value dst should end up sharing a physical register/slot
    /// with, if Phase 8's allocator can manage it. Every entry corresponds
    /// to a 2-address-destructive x86 operation where honoring the hint
    /// lets the final MachineInst-to-bytes emission step skip an
    /// otherwise-mandatory `mov dst, lhs` copy. A hint that isn't honored
    /// is not an error -- emission falls back to inserting the copy.
    pub coalescing_hints: HashMap<Value, Value>,
}

struct Selector<'a> {
    func: &'a Function,
    insts: Vec<MachineInst>,
    synthetic_types: HashMap<Value, Ty>,
    next_value: u32,
}

impl<'a> Selector<'a> {
    /// Looks up a Value's Ty whether it's a real IR value (func.types) or
    /// a synthetic temp this selector minted (synthetic_types). Value
    /// numbering is append-only across this codebase's whole optimizer
    /// pipeline (verified: no pass compacts or renumbers `f.insts`), so
    /// `next_value` seeded from `func.insts.len()` never collides with a
    /// real Value, and this dispatch on index is safe.
    fn ty_of(&self, v: Value) -> Ty {
        if (v.0 as usize) < self.func.types.len() {
            self.func.types[v.0 as usize]
        } else {
            self.synthetic_types[&v]
        }
    }

    fn fresh(&mut self, ty: Ty) -> Value {
        let v = Value(self.next_value);
        self.next_value += 1;
        self.synthetic_types.insert(v, ty);
        v
    }

    /// Deliberately has NO wildcard (`_ =>`) arm -- same exhaustiveness
    /// rationale as forge-ir's `uses_of`/`replace_in_inst`: a new `Inst`
    /// variant must get an explicit arm here, or this fails to compile.
    /// If you hit that compile error, ADD A REAL ARM, don't silence it
    /// with `_ => {}` -- that would defeat the whole point of this match.
    fn select_inst(&mut self, dst: Value, inst: &Inst) {
        match inst {
            Inst::ConstF64(bits) => self
                .insts
                .push(MachineInst::LoadImmF64 { dst, bits: *bits }),
            Inst::ConstI64(v) => self.insts.push(MachineInst::LoadImmI64 { dst, imm: *v }),
            Inst::ConstBool(v) => self.insts.push(MachineInst::LoadImmI64 {
                dst,
                imm: *v as i64,
            }),
            Inst::Param { index, .. } => self.insts.push(MachineInst::Param { dst, index: *index }),

            Inst::Add(a, b) => match self.ty_of(*a) {
                Ty::F64 => self.insts.push(MachineInst::FloatAdd { dst, lhs: *a, rhs: *b }),
                Ty::I64 => self.insts.push(MachineInst::IntAdd { dst, lhs: *a, rhs: *b }),
                Ty::Bool => unreachable!("Add never applies to Bool"),
            },
            Inst::Sub(a, b) => match self.ty_of(*a) {
                Ty::F64 => self.insts.push(MachineInst::FloatSub { dst, lhs: *a, rhs: *b }),
                Ty::I64 => self.insts.push(MachineInst::IntSub { dst, lhs: *a, rhs: *b }),
                Ty::Bool => unreachable!("Sub never applies to Bool"),
            },
            Inst::Mul(a, b) => match self.ty_of(*a) {
                Ty::F64 => self.insts.push(MachineInst::FloatMul { dst, lhs: *a, rhs: *b }),
                Ty::I64 => self.insts.push(MachineInst::IntMul { dst, lhs: *a, rhs: *b }),
                Ty::Bool => unreachable!("Mul never applies to Bool"),
            },
            Inst::Div(a, b) => match self.ty_of(*a) {
                Ty::F64 => self.insts.push(MachineInst::FloatDiv { dst, lhs: *a, rhs: *b }),
                Ty::I64 => self.insts.push(MachineInst::IntDiv { dst, lhs: *a, rhs: *b }),
                Ty::Bool => unreachable!("Div never applies to Bool"),
            },
            Inst::Rem(a, b) => match self.ty_of(*a) {
                Ty::I64 => self.insts.push(MachineInst::IntRem { dst, lhs: *a, rhs: *b }),
                Ty::F64 => unimplemented!(
                    "float remainder (fmod) has no native x86 instruction and isn't wired to a libm call yet"
                ),
                Ty::Bool => unreachable!("Rem never applies to Bool"),
            },
            Inst::Neg(a) => match self.ty_of(*a) {
                Ty::F64 => {
                    // i64::MIN == 0x8000_0000_0000_0000: only the sign bit
                    // set. XOR-ing this into the value FLIPS the sign bit
                    // (negation) -- contrast Abs's mask below, which CLEARS
                    // it via AND instead.
                    let mask_tmp = self.fresh(Ty::I64);
                    self.insts.push(MachineInst::LoadImmI64 { dst: mask_tmp, imm: i64::MIN });
                    self.insts.push(MachineInst::FloatNeg { dst, src: *a, mask_tmp });
                }
                Ty::I64 => self.insts.push(MachineInst::IntNeg { dst, src: *a }),
                Ty::Bool => unreachable!("Neg never applies to Bool"),
            },
            Inst::And(a, b) => self.insts.push(MachineInst::And { dst, lhs: *a, rhs: *b }),
            Inst::Or(a, b) => self.insts.push(MachineInst::Or { dst, lhs: *a, rhs: *b }),
            Inst::Xor(a, b) => self.insts.push(MachineInst::Xor { dst, lhs: *a, rhs: *b }),
            Inst::Not(a) => self.insts.push(MachineInst::Not { dst, src: *a }),
            Inst::Shl(a, b) => self.insts.push(MachineInst::Shl { dst, lhs: *a, rhs: *b }),
            Inst::Shr(a, b) => self.insts.push(MachineInst::Shr { dst, lhs: *a, rhs: *b }),
            Inst::Sar(a, b) => self.insts.push(MachineInst::Sar { dst, lhs: *a, rhs: *b }),

            Inst::Min(a, b) => self.insts.push(MachineInst::FloatMin { dst, lhs: *a, rhs: *b }),
            Inst::Max(a, b) => self.insts.push(MachineInst::FloatMax { dst, lhs: *a, rhs: *b }),
            Inst::Sqrt(a) => self.insts.push(MachineInst::FloatSqrt { dst, src: *a }),
            Inst::Floor(a) => self.insts.push(MachineInst::FloatRound {
                dst,
                src: *a,
                mode: crate::RoundMode::Floor,
            }),
            Inst::Ceil(a) => self.insts.push(MachineInst::FloatRound {
                dst,
                src: *a,
                mode: crate::RoundMode::Ceil,
            }),
            Inst::Round(a) => self.insts.push(MachineInst::FloatRound {
                dst,
                src: *a,
                mode: crate::RoundMode::Nearest,
            }),
            Inst::Trunc(a) => self.insts.push(MachineInst::FloatRound {
                dst,
                src: *a,
                mode: crate::RoundMode::Truncate,
            }),

            Inst::Cmp { op, lhs, rhs } => {
                // dst's Ty is always Bool for a comparison -- dispatching on
                // dst instead of the operand would always fall into the
                // I64|Bool arm below and silently mis-lower every float
                // comparison. Dispatch on the OPERAND's type instead.
                let operand_ty = self.ty_of(*lhs);
                match operand_ty {
                    Ty::F64 => self.insts.push(MachineInst::FloatCmp {
                        op: *op,
                        dst,
                        lhs: *lhs,
                        rhs: *rhs,
                    }),
                    Ty::I64 | Ty::Bool => self.insts.push(MachineInst::IntCmp {
                        op: *op,
                        dst,
                        lhs: *lhs,
                        rhs: *rhs,
                    }),
                }
            }
            Inst::IToF(a) => self.insts.push(MachineInst::IntToFloat { dst, src: *a }),
            Inst::FToI(a) => self.insts.push(MachineInst::FloatToInt { dst, src: *a }),

            Inst::Abs(a) => {
                // 0x7FFF_FFFF_FFFF_FFFF: every bit set EXCEPT the sign bit.
                // AND-ing this into the value CLEARS the sign bit
                // (absolute value) -- contrast Neg's mask above, which
                // FLIPS it via XOR instead.
                let mask_tmp = self.fresh(Ty::I64);
                self.insts.push(MachineInst::LoadImmI64 {
                    dst: mask_tmp,
                    imm: 0x7FFF_FFFF_FFFF_FFFFi64,
                });
                self.insts.push(MachineInst::FloatAbs { dst, src: *a, mask_tmp });
            }
            // NOT bit-identical to a real hardware FMA (that's a single
            // rounding; this is two -- multiply rounds once, then add
            // rounds again). Correct interim behavior until AVX/FMA3
            // lands (Phase 6's VEX/AVX subsection, not yet built) --
            // documented loudly rather than silently approximated.
            Inst::Fma { a, b, c } => {
                let mul_tmp = self.fresh(Ty::F64);
                self.insts.push(MachineInst::FloatMul { dst: mul_tmp, lhs: *a, rhs: *b });
                self.insts.push(MachineInst::FloatAdd { dst, lhs: mul_tmp, rhs: *c });
            }

            Inst::Call { .. } => unimplemented!("libm call lowering ships in Phase 7e"),
            Inst::Phi { .. } => {
                // Deliberately emits nothing -- see the design doc's "φ
                // handling" section. This Inst's destination Value is
                // resolved entirely by Phase 8's SSA deconstruction
                // (assign the same physical register/slot where possible;
                // insert parallel-copy moves at predecessor block ends
                // otherwise). That strategy is only safe when no CFG edge
                // into a phi's block is a critical edge -- true for
                // today's if/else-only lowering, but NOT enforced or
                // checked anywhere yet. Re-verify this once loops
                // (currently a stretch goal) can introduce back-edges.
            }
        }
    }

    fn select_term(&mut self, term: &Terminator) {
        match term {
            Terminator::Return(v) => self.insts.push(MachineInst::Return { value: *v }),
            Terminator::Jump(b) => self.insts.push(MachineInst::Jump { target: *b }),
            Terminator::Branch { cond, then_, else_ } => self.insts.push(MachineInst::Branch {
                cond: *cond,
                then_: *then_,
                else_: *else_,
            }),
        }
    }
}

pub fn select(func: &Function) -> SelectedFunction {
    let mut sel = Selector {
        func,
        insts: Vec::new(),
        synthetic_types: HashMap::new(),
        next_value: func.insts.len() as u32,
    };
    for block in forge_ir::dominance::reverse_postorder(func) {
        for &v in &func.blocks[block.0 as usize].insts {
            let inst = &func.insts[v.0 as usize];
            sel.select_inst(v, inst);
        }
        if let Some(term) = &func.blocks[block.0 as usize].term {
            sel.select_term(term);
        }
    }
    let coalescing_hints = compute_coalescing_hints(&sel.insts);
    SelectedFunction {
        insts: sel.insts,
        synthetic_types: sel.synthetic_types,
        coalescing_hints,
    }
}

/// Scans a fully-selected instruction sequence and records a dst->operand
/// coalescing hint for every 2-address-destructive MachineInst. Binary ops
/// hint dst->lhs (the operand whose register `dst` needs to already hold);
/// unary ops hint dst->src. IntDiv/IntRem are deliberately excluded -- their
/// constraint is fixed RAX/RDX placement, a different (fixed-register, not
/// coalescing) hint Phase 8's allocator handles separately. Lea is
/// deliberately excluded too -- real x86 lea is non-destructive 3-operand,
/// so it has no two-address constraint to hint around at all.
pub fn compute_coalescing_hints(insts: &[MachineInst]) -> HashMap<Value, Value> {
    let mut hints = HashMap::new();
    for inst in insts {
        match inst {
            MachineInst::IntAdd { dst, lhs, .. }
            | MachineInst::IntSub { dst, lhs, .. }
            | MachineInst::IntMul { dst, lhs, .. }
            | MachineInst::And { dst, lhs, .. }
            | MachineInst::Or { dst, lhs, .. }
            | MachineInst::Xor { dst, lhs, .. }
            | MachineInst::Shl { dst, lhs, .. }
            | MachineInst::Shr { dst, lhs, .. }
            | MachineInst::Sar { dst, lhs, .. }
            | MachineInst::FloatAdd { dst, lhs, .. }
            | MachineInst::FloatSub { dst, lhs, .. }
            | MachineInst::FloatMul { dst, lhs, .. }
            | MachineInst::FloatDiv { dst, lhs, .. }
            | MachineInst::FloatMin { dst, lhs, .. }
            | MachineInst::FloatMax { dst, lhs, .. } => {
                hints.insert(*dst, *lhs);
            }
            MachineInst::IntNeg { dst, src }
            | MachineInst::Not { dst, src }
            | MachineInst::FloatNeg { dst, src, .. }
            | MachineInst::FloatAbs { dst, src, .. } => {
                hints.insert(*dst, *src);
            }
            _ => {}
        }
    }
    hints
}

#[cfg(test)]
mod tests {
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
}
