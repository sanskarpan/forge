// crates/forge-ir/src/lower.rs

use smallvec::smallvec;

use forge_syntax::ast::{BinaryOp, Expr, ExprIdx, UnaryOp};
use forge_syntax::typeck::{Ty as AstTy, TypedAst};

use crate::builder::Builder;
use crate::ir::*;

pub fn lower(typed: &TypedAst) -> Function {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.f.entry = entry;
    b.cur_block = entry;
    b.seal_block(entry);

    let root_span = typed.ast.span(typed.ast.root);
    for (i, (name, ty)) in typed.params.iter().enumerate() {
        let ty = lower_ty(*ty);
        let v = b.emit(entry, Inst::Param { index: i as u32, ty }, ty, root_span);
        b.f.params.push((name.clone(), ty));
        b.write_variable(name, entry, v);
    }

    let (result, exit_block) = lower_expr(&mut b, typed, typed.ast.root);
    b.f.blocks[exit_block.0 as usize].term = Some(Terminator::Return(result));
    b.f
}

fn lower_ty(t: AstTy) -> Ty {
    match t {
        AstTy::F64 => Ty::F64,
        AstTy::I64 => Ty::I64,
        AstTy::Bool => Ty::Bool,
    }
}

/// Returns the value produced and the block that now holds it. `if` creates
/// new blocks, so every caller threads the returned block forward instead of
/// assuming `b.cur_block` is still what it was before the recursive call.
fn lower_expr(b: &mut Builder, typed: &TypedAst, idx: ExprIdx) -> (Value, Block) {
    let span = typed.ast.span(idx);
    let ty = lower_ty(typed.types[idx.index()]);
    let block = b.cur_block;

    match typed.ast.get(idx).clone() {
        Expr::Float(v) => (b.emit(block, Inst::ConstF64(v.to_bits()), ty, span), block),
        Expr::Int(n) => (b.emit(block, Inst::ConstI64(n), ty, span), block),
        Expr::Bool(v) => (b.emit(block, Inst::ConstBool(v), ty, span), block),
        Expr::Ident(name) => (b.read_variable(&name, block, ty), block),

        Expr::Unary { op, operand } => {
            let (v, block) = lower_expr(b, typed, operand);
            b.cur_block = block;
            let inst = match op {
                UnaryOp::Neg => Inst::Neg(v),
                UnaryOp::Not | UnaryOp::BitNot => Inst::Not(v),
            };
            (b.emit(block, inst, ty, span), block)
        }

        Expr::Binary { op, lhs, rhs } => {
            let (l, block) = lower_expr(b, typed, lhs);
            b.cur_block = block;
            let (r, block) = lower_expr(b, typed, rhs);
            b.cur_block = block;
            let inst = lower_binary(op, l, r);
            (b.emit(block, inst, ty, span), block)
        }

        Expr::Call { callee, args } => {
            let mut vals = Vec::new();
            let mut block = block;
            for a in &args {
                let (v, blk) = lower_expr(b, typed, *a);
                vals.push(v);
                block = blk;
                b.cur_block = block;
            }
            let inst = lower_call(&callee, &vals);
            (b.emit(block, inst, ty, span), block)
        }

        Expr::If { cond, then_, else_ } => {
            let (c, block) = lower_expr(b, typed, cond);
            let then_block = b.create_block();
            let else_block = b.create_block();
            let merge_block = b.create_block();

            b.f.blocks[block.0 as usize].term =
                Some(Terminator::Branch { cond: c, then_: then_block, else_: else_block });
            b.add_pred(then_block, block);
            b.add_pred(else_block, block);
            b.seal_block(then_block);
            b.seal_block(else_block);

            b.cur_block = then_block;
            let (then_val, then_exit) = lower_expr(b, typed, then_);
            b.f.blocks[then_exit.0 as usize].term = Some(Terminator::Jump(merge_block));
            b.add_pred(merge_block, then_exit);

            b.cur_block = else_block;
            let (else_val, else_exit) = lower_expr(b, typed, else_);
            b.f.blocks[else_exit.0 as usize].term = Some(Terminator::Jump(merge_block));
            b.add_pred(merge_block, else_exit);

            b.seal_block(merge_block);
            b.cur_block = merge_block;
            let incoming = smallvec![(then_exit, then_val), (else_exit, else_val)];
            (b.emit(merge_block, Inst::Phi { incoming }, ty, span), merge_block)
        }

        // `name` was already alpha-renamed by forge_syntax::resolve to be
        // globally unique, so writing it into this block's SSA variable map
        // can never collide with (or need restoring after) any other
        // binding — see the design doc's "Resolved ambiguities".
        Expr::Let { name, value, body } => {
            let (v, block) = lower_expr(b, typed, value);
            b.cur_block = block;
            b.write_variable(&name, block, v);
            lower_expr(b, typed, body)
        }
    }
}

fn lower_binary(op: BinaryOp, l: Value, r: Value) -> Inst {
    use BinaryOp::*;
    match op {
        Add => Inst::Add(l, r), Sub => Inst::Sub(l, r), Mul => Inst::Mul(l, r),
        Div => Inst::Div(l, r), Rem => Inst::Rem(l, r),
        BitAnd | And => Inst::And(l, r),
        BitOr | Or => Inst::Or(l, r),
        BitXor => Inst::Xor(l, r),
        Shl => Inst::Shl(l, r), Shr => Inst::Shr(l, r),
        Eq => Inst::Cmp { op: CmpOp::Eq, lhs: l, rhs: r },
        Ne => Inst::Cmp { op: CmpOp::Ne, lhs: l, rhs: r },
        Lt => Inst::Cmp { op: CmpOp::Lt, lhs: l, rhs: r },
        Le => Inst::Cmp { op: CmpOp::Le, lhs: l, rhs: r },
        Gt => Inst::Cmp { op: CmpOp::Gt, lhs: l, rhs: r },
        Ge => Inst::Cmp { op: CmpOp::Ge, lhs: l, rhs: r },
    }
}

fn lower_call(callee: &str, args: &[Value]) -> Inst {
    match callee {
        "sqrt" => Inst::Sqrt(args[0]),
        "abs" => Inst::Abs(args[0]),
        "floor" => Inst::Floor(args[0]),
        "ceil" => Inst::Ceil(args[0]),
        "round" => Inst::Round(args[0]),
        "trunc" => Inst::Trunc(args[0]),
        "min" => Inst::Min(args[0], args[1]),
        "max" => Inst::Max(args[0], args[1]),
        "fma" => Inst::Fma { a: args[0], b: args[1], c: args[2] },
        "sin" => Inst::Call { func: LibFunc::Sin, args: args.iter().copied().collect() },
        "cos" => Inst::Call { func: LibFunc::Cos, args: args.iter().copied().collect() },
        "tan" => Inst::Call { func: LibFunc::Tan, args: args.iter().copied().collect() },
        "exp" => Inst::Call { func: LibFunc::Exp, args: args.iter().copied().collect() },
        "log" => Inst::Call { func: LibFunc::Log, args: args.iter().copied().collect() },
        "pow" => Inst::Call { func: LibFunc::Pow, args: args.iter().copied().collect() },
        other => unreachable!("type checker already rejected unknown intrinsic `{other}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_syntax::lexer::lex;
    use forge_syntax::parser::parse;
    use forge_syntax::resolve::resolve;
    use forge_syntax::typeck::typecheck;

    fn lowered(src: &str) -> Function {
        let (tokens, _) = lex(src);
        let (ast, _) = parse(&tokens);
        let typed = typecheck(resolve(ast)).expect("should type-check");
        lower(&typed)
    }

    #[test]
    fn sqrt_of_sum_of_squares_is_exactly_six_instructions() {
        // param x, param y, mul, mul, add, sqrt — matches SPEC §15's example.
        let f = lowered("sqrt(x * x + y * y)");
        assert_eq!(f.insts.len(), 6);
    }

    #[test]
    fn if_produces_four_blocks_with_a_phi() {
        let f = lowered("if x > 0.0 then x else -x");
        assert_eq!(f.blocks.len(), 4); // entry, then, else, merge
        let merge = &f.blocks[3];
        let phi_count = merge.insts.iter().filter(|&&v| matches!(f.insts[v.0 as usize], Inst::Phi { .. })).count();
        assert_eq!(phi_count, 1);
    }

    #[test]
    fn outer_reference_after_shadowing_let_resolves_to_the_parameter() {
        // (let x = 1.0 in x) + x must add the let's value to the PARAMETER x,
        // not to itself — this is the shadowing bug the resolve pass fixes.
        let f = lowered("(let x = 1.0 in x) + x");
        let add = f.insts.iter().find(|i| matches!(i, Inst::Add(_, _))).expect("an Add exists");
        let Inst::Add(l, r) = add else { unreachable!() };
        // Both operands trace back to the single Param instruction — if the
        // shadow leaked, one operand would instead be the ConstF64(1.0).
        let param_idx = f.insts.iter().position(|i| matches!(i, Inst::Param { .. })).unwrap() as u32;
        assert_eq!(r.0, param_idx, "the trailing `x` must be the parameter");
        let _ = l; // lhs is the let's inner (shadowed) value — not asserted here
    }

    #[test]
    fn every_block_has_a_terminator() {
        let f = lowered("if x > 0.0 then x else -x");
        for b in &f.blocks {
            assert!(b.term.is_some());
        }
    }
}
