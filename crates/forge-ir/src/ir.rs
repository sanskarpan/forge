// crates/forge-ir/src/ir.rs

use smallvec::SmallVec;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Value(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Block(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Ty {
    F64,
    I64,
    Bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LibFunc {
    Sin,
    Cos,
    Tan,
    Exp,
    Log,
    Pow,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Inst {
    ConstF64(u64),
    ConstI64(i64),
    ConstBool(bool),
    Param {
        index: u32,
        ty: Ty,
    },

    Add(Value, Value),
    Sub(Value, Value),
    Mul(Value, Value),
    Div(Value, Value),
    Rem(Value, Value),
    Neg(Value),

    // Two legitimate origins: the `fma()` intrinsic lowers here directly, and
    // (later) fast-math FMA contraction rewrites `a*b + c` into the same
    // instruction. Not constructed by contraction in this slice — no
    // optimizer yet — only by the parser via `fma()`.
    Fma {
        a: Value,
        b: Value,
        c: Value,
    },

    // Also used to lower `&&`/`||`/`!` on bool operands — a 1-bit boolean is
    // representationally identical to i64's low bit for AND/OR/NOT, so no
    // separate logical-op instruction is needed.
    And(Value, Value),
    Or(Value, Value),
    Xor(Value, Value),
    Not(Value),
    Shl(Value, Value),
    Shr(Value, Value),
    Sar(Value, Value),

    Cmp {
        op: CmpOp,
        lhs: Value,
        rhs: Value,
    },

    Sqrt(Value),
    Abs(Value),
    Min(Value, Value),
    Max(Value, Value),
    Floor(Value),
    Ceil(Value),
    Round(Value),
    Trunc(Value),

    Call {
        func: LibFunc,
        args: SmallVec<[Value; 2]>,
    },

    IToF(Value),
    FToI(Value),

    Phi {
        incoming: SmallVec<[(Block, Value); 2]>,
    },
}

#[derive(Clone, Debug)]
pub enum Terminator {
    Return(Value),
    Jump(Block),
    Branch {
        cond: Value,
        then_: Block,
        else_: Block,
    },
}

#[derive(Clone, Debug, Default)]
pub struct BlockData {
    pub insts: Vec<Value>,
    pub term: Option<Terminator>,
    pub preds: SmallVec<[Block; 2]>,
}

pub struct Function {
    pub insts: Vec<Inst>,
    pub types: Vec<Ty>,
    pub spans: Vec<forge_syntax::span::Span>,
    pub blocks: Vec<BlockData>,
    pub entry: Block,
    pub params: Vec<(String, Ty)>,
}

/// Every use-operand of an instruction, for the verifier and for
/// `replace_in_inst`. `Inst` is deliberately not `Copy`/matched elsewhere
/// with a catch-all — a new variant must be added here explicitly, or the
/// verifier silently stops checking its operands.
pub fn uses_of(inst: &Inst) -> Vec<Value> {
    match inst {
        Inst::Add(a, b)
        | Inst::Sub(a, b)
        | Inst::Mul(a, b)
        | Inst::Div(a, b)
        | Inst::Rem(a, b)
        | Inst::And(a, b)
        | Inst::Or(a, b)
        | Inst::Xor(a, b)
        | Inst::Shl(a, b)
        | Inst::Shr(a, b)
        | Inst::Sar(a, b)
        | Inst::Min(a, b)
        | Inst::Max(a, b) => vec![*a, *b],
        Inst::Neg(a)
        | Inst::Not(a)
        | Inst::Sqrt(a)
        | Inst::Abs(a)
        | Inst::Floor(a)
        | Inst::Ceil(a)
        | Inst::Round(a)
        | Inst::Trunc(a)
        | Inst::IToF(a)
        | Inst::FToI(a) => vec![*a],
        Inst::Fma { a, b, c } => vec![*a, *b, *c],
        Inst::Cmp { lhs, rhs, .. } => vec![*lhs, *rhs],
        Inst::Call { args, .. } => args.iter().copied().collect(),
        Inst::Phi { incoming } => incoming.iter().map(|(_, v)| *v).collect(),
        Inst::ConstF64(_) | Inst::ConstI64(_) | Inst::ConstBool(_) | Inst::Param { .. } => vec![],
    }
}

/// Same exhaustiveness rationale as `uses_of` above — no wildcard arm, so a
/// new `Inst` variant must be handled here explicitly.
pub fn replace_in_inst(inst: &mut Inst, old: Value, new: Value) {
    let sub = |v: &mut Value| {
        if *v == old {
            *v = new;
        }
    };
    match inst {
        Inst::Add(a, b)
        | Inst::Sub(a, b)
        | Inst::Mul(a, b)
        | Inst::Div(a, b)
        | Inst::Rem(a, b)
        | Inst::And(a, b)
        | Inst::Or(a, b)
        | Inst::Xor(a, b)
        | Inst::Shl(a, b)
        | Inst::Shr(a, b)
        | Inst::Sar(a, b)
        | Inst::Min(a, b)
        | Inst::Max(a, b) => {
            sub(a);
            sub(b);
        }
        Inst::Neg(a)
        | Inst::Not(a)
        | Inst::Sqrt(a)
        | Inst::Abs(a)
        | Inst::Floor(a)
        | Inst::Ceil(a)
        | Inst::Round(a)
        | Inst::Trunc(a)
        | Inst::IToF(a)
        | Inst::FToI(a) => sub(a),
        Inst::Fma { a, b, c } => {
            sub(a);
            sub(b);
            sub(c);
        }
        Inst::Cmp { lhs, rhs, .. } => {
            sub(lhs);
            sub(rhs);
        }
        Inst::Call { args, .. } => {
            for a in args.iter_mut() {
                sub(a);
            }
        }
        Inst::Phi { incoming } => {
            for (_, v) in incoming.iter_mut() {
                sub(v);
            }
        }
        Inst::ConstF64(_) | Inst::ConstI64(_) | Inst::ConstBool(_) | Inst::Param { .. } => {}
    }
}

/// Redirects every use of `old` to `new`, across BOTH instruction operands
/// (via `replace_in_inst`) and terminator operands (`Return`'s value,
/// `Branch`'s condition) — the gap `uses_of`/`replace_in_inst` alone don't
/// cover (flagged during a prior code review) and load-bearing for the
/// optimizer: GVN/DCE need to redirect a terminator's operand when the
/// value it referenced gets CSE'd away.
pub fn replace_value_everywhere(f: &mut Function, old: Value, new: Value) {
    for inst in &mut f.insts {
        replace_in_inst(inst, old, new);
    }
    for block in &mut f.blocks {
        match &mut block.term {
            Some(Terminator::Return(v)) => {
                if *v == old {
                    *v = new;
                }
            }
            Some(Terminator::Branch { cond, .. }) if *cond == old => {
                *cond = new;
            }
            _ => {}
        }
    }
}

/// Insert a new instruction into `block` at position `pos` (shifting
/// everything at/after `pos` — including the value that used to sit there —
/// one slot to the right) and return its freshly allocated `Value`. Unlike
/// `Vec::push`, this lets us splice helper instructions in BEFORE an
/// existing instruction whose own `Value` identity must be preserved (later
/// code may already reference it). Shared by `forge-opt`'s `strength.rs`
/// (the div/rem sign-fixup sequences: several new instructions have to be
/// defined, in program order, before the position the original `Div`/`Rem`
/// occupied) and `reassoc.rs` (rebuilding a balanced tree in place of a
/// flattened chain, at the old chain root's own position — see that
/// module's `build_balanced` for why position, not just dominance, matters
/// there).
///
/// ## Chaining several calls in a row
///
/// Each call only shifts things right by one slot, so inserting a whole
/// SEQUENCE of `n` new instructions before some original position requires
/// threading a mutable `pos` through `n` calls, incrementing it by one after
/// each: the first call inserts at the original `pos`; every following call
/// must insert at the now-updated `pos` too, since that's where the
/// still-pending original instruction (and everything after it) has been
/// pushed to. After all `n` calls, the original instruction's own new
/// position is `pos` (i.e. `old_pos + n`) — not `old_pos + n + 1`, since
/// `pos` was already advanced past the last inserted instruction; callers
/// that also rewrite the original instruction's own slot afterward, and then
/// need to know where to resume scanning the block, use `pos + 1` for that
/// (skip past the rewritten original too). See `strength.rs`'s
/// `emit_div_bias` for the reference implementation of this pattern (six
/// calls in a row, `pos` incremented after each), and `div_pow2`/`rem_pow2`
/// for how its returned `pos` is subsequently used both to insert further
/// instructions and to compute the caller's resume position.
pub fn insert_before(
    f: &mut Function,
    block: Block,
    pos: usize,
    inst: Inst,
    ty: Ty,
    span: forge_syntax::span::Span,
) -> Value {
    let v = Value(f.insts.len() as u32);
    f.insts.push(inst);
    f.types.push(ty);
    f.spans.push(span);
    f.blocks[block.0 as usize].insts.insert(pos, v);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_syntax::span::Span;

    fn hand_built_return_fn() -> Function {
        // v0 = ConstF64(1.0), v1 = ConstF64(2.0), term: Return(v0)
        Function {
            insts: vec![
                Inst::ConstF64(1.0f64.to_bits()),
                Inst::ConstF64(2.0f64.to_bits()),
            ],
            types: vec![Ty::F64, Ty::F64],
            spans: vec![Span::new(0, 0), Span::new(0, 0)],
            blocks: vec![BlockData {
                insts: vec![Value(0), Value(1)],
                term: Some(Terminator::Return(Value(0))),
                preds: Default::default(),
            }],
            entry: Block(0),
            params: vec![],
        }
    }

    #[test]
    fn replaces_return_terminator_operand() {
        let mut f = hand_built_return_fn();
        replace_value_everywhere(&mut f, Value(0), Value(1));
        assert!(matches!(f.blocks[0].term, Some(Terminator::Return(v)) if v == Value(1)));
    }

    #[test]
    fn replaces_branch_terminator_condition() {
        let mut f = hand_built_return_fn();
        f.blocks[0].term = Some(Terminator::Branch {
            cond: Value(0),
            then_: Block(0),
            else_: Block(0),
        });
        replace_value_everywhere(&mut f, Value(0), Value(1));
        assert!(
            matches!(f.blocks[0].term, Some(Terminator::Branch { cond, .. }) if cond == Value(1))
        );
    }

    #[test]
    fn still_replaces_instruction_operands_via_existing_replace_in_inst() {
        let mut f = hand_built_return_fn();
        f.insts.push(Inst::Add(Value(0), Value(1))); // v2 = v0 + v1
        replace_value_everywhere(&mut f, Value(0), Value(1));
        assert!(matches!(f.insts[2], Inst::Add(a, b) if a == Value(1) && b == Value(1)));
    }
}
