// crates/forge-ir/src/ir.rs

use smallvec::SmallVec;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Value(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Block(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ty {
    F64,
    I64,
    Bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LibFunc {
    Sin,
    Cos,
    Tan,
    Exp,
    Log,
    Pow,
}

#[derive(Clone, Debug)]
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
