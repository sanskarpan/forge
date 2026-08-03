// crates/forge-syntax/src/ast.rs

use crate::span::Span;
use std::marker::PhantomData;

pub struct Idx<T> {
    raw: u32,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Idx<T> {
    pub(crate) fn new(raw: u32) -> Self {
        Self {
            raw,
            _marker: PhantomData,
        }
    }
    pub fn index(self) -> usize {
        self.raw as usize
    }
}

impl<T> Clone for Idx<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Idx<T> {}
impl<T> PartialEq for Idx<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}
impl<T> Eq for Idx<T> {}
impl<T> std::hash::Hash for Idx<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.raw.hash(state)
    }
}
impl<T> std::fmt::Debug for Idx<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Idx({})", self.raw)
    }
}

pub type ExprIdx = Idx<Expr>;

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Float(f64),
    Int(i64),
    Bool(bool),
    Ident(String),
    Unary {
        op: UnaryOp,
        operand: ExprIdx,
    },
    Binary {
        op: BinaryOp,
        lhs: ExprIdx,
        rhs: ExprIdx,
    },
    Call {
        callee: String,
        args: Vec<ExprIdx>,
    },
    If {
        cond: ExprIdx,
        then_: ExprIdx,
        else_: ExprIdx,
    },
    Let {
        name: String,
        value: ExprIdx,
        body: ExprIdx,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone)]
pub struct Ast {
    pub exprs: Vec<Expr>,
    pub spans: Vec<Span>,
    pub root: ExprIdx,
}

impl Ast {
    pub fn get(&self, idx: ExprIdx) -> &Expr {
        &self.exprs[idx.index()]
    }
    pub fn span(&self, idx: ExprIdx) -> Span {
        self.spans[idx.index()]
    }
}
