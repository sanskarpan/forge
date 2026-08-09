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
    // `func` and `next_value` are unread until Task 2 wires up `ty_of`'s
    // and `fresh`'s call sites (Ty-dispatching arithmetic arms, synthetic
    // mask/mul temps) -- allowed here since Task 1 intentionally leaves
    // most of `select_inst`'s match as `todo!()`, per the plan.
    #[allow(dead_code)]
    func: &'a Function,
    insts: Vec<MachineInst>,
    synthetic_types: HashMap<Value, Ty>,
    #[allow(dead_code)]
    next_value: u32,
}

impl<'a> Selector<'a> {
    /// Looks up a Value's Ty whether it's a real IR value (func.types) or
    /// a synthetic temp this selector minted (synthetic_types). Value
    /// numbering is append-only across this codebase's whole optimizer
    /// pipeline (verified: no pass compacts or renumbers `f.insts`), so
    /// `next_value` seeded from `func.insts.len()` never collides with a
    /// real Value, and this dispatch on index is safe.
    #[allow(dead_code)]
    fn ty_of(&self, v: Value) -> Ty {
        if (v.0 as usize) < self.func.types.len() {
            self.func.types[v.0 as usize]
        } else {
            self.synthetic_types[&v]
        }
    }

    #[allow(dead_code)]
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

            // Remaining variants are filled in by later tasks in this plan.
            _ => todo!("filled in by Tasks 2-6 of the Phase 7a plan"),
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
}
