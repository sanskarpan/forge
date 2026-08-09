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

            // Remaining variants are filled in by later tasks in this plan.
            _ => todo!("filled in by Tasks 4-6 of the Phase 7a plan"),
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
    SelectedFunction {
        insts: sel.insts,
        synthetic_types: sel.synthetic_types,
    }
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
}
