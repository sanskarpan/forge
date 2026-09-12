// crates/forge-ir/src/verify.rs
//
//! Structural and dominance validation for the SSA IR.
//!
//! The verifier is deliberately defensive because optimized IR can be built
//! by more than [`crate::builder::Builder`]. It therefore validates indices,
//! placement, CFG edges, same-block ordering, operand types, and dominance
//! before any helper that assumes a well-formed graph is called.

use crate::dominance::{compute_dominators, dominates};
use crate::ir::*;

pub fn verify(f: &Function) -> Result<(), String> {
    if f.blocks.is_empty() {
        return Err("function has no blocks".to_string());
    }
    if f.entry.0 as usize >= f.blocks.len() {
        return Err(format!(
            "function entry block {:?} is out of range",
            f.entry
        ));
    }
    if f.types.len() != f.insts.len() {
        return Err(format!(
            "instruction/type table length mismatch: {} instruction(s), {} type(s)",
            f.insts.len(),
            f.types.len()
        ));
    }
    if f.spans.len() != f.insts.len() {
        return Err(format!(
            "instruction/span table length mismatch: {} instruction(s), {} span(s)",
            f.insts.len(),
            f.spans.len()
        ));
    }

    let mut defined_in = vec![None; f.insts.len()];
    let mut defined_at = vec![None; f.insts.len()];
    for (bi, bd) in f.blocks.iter().enumerate() {
        for (position, &v) in bd.insts.iter().enumerate() {
            let index = v.0 as usize;
            if index >= f.insts.len() {
                return Err(format!(
                    "block {:?} references instruction value {:?} outside the instruction table",
                    Block(bi as u32),
                    v
                ));
            }
            if let Some(previous) = defined_in[index] {
                return Err(format!(
                    "instruction value {:?} is listed in both {:?} and {:?}",
                    v,
                    previous,
                    Block(bi as u32)
                ));
            }
            defined_in[index] = Some(Block(bi as u32));
            defined_at[index] = Some(position);
        }
        for &pred in &bd.preds {
            if pred.0 as usize >= f.blocks.len() {
                return Err(format!(
                    "block {:?} lists predecessor {:?} outside the block table",
                    Block(bi as u32),
                    pred
                ));
            }
        }
    }
    for (index, block) in defined_in.iter().enumerate() {
        if block.is_none() {
            return Err(format!(
                "instruction value {:?} is not listed in any block",
                Value(index as u32)
            ));
        }
    }

    let mut expected_preds = vec![Vec::new(); f.blocks.len()];
    for (bi, bd) in f.blocks.iter().enumerate() {
        let block = Block(bi as u32);
        let Some(term) = &bd.term else {
            return Err(format!("block {block:?} has no terminator"));
        };
        let targets: Vec<Block> = match term {
            Terminator::Return(_) => Vec::new(),
            Terminator::Jump(target) => vec![*target],
            Terminator::Branch { then_, else_, .. } => vec![*then_, *else_],
        };
        for target in targets {
            if target.0 as usize >= f.blocks.len() {
                return Err(format!(
                    "terminator in {block:?} targets block {target:?} outside the block table"
                ));
            }
            expected_preds[target.0 as usize].push(block);
        }
    }
    for (index, bd) in f.blocks.iter().enumerate() {
        let mut actual = bd.preds.to_vec();
        let mut expected = expected_preds[index].clone();
        actual.sort_by_key(|block| block.0);
        expected.sort_by_key(|block| block.0);
        if actual != expected {
            return Err(format!(
                "predecessor list for {:?} is {:?}, but terminators provide {:?}",
                Block(index as u32),
                bd.preds,
                expected_preds[index]
            ));
        }
    }

    let idom = compute_dominators(f);

    for (bi, bd) in f.blocks.iter().enumerate() {
        let block = Block(bi as u32);

        for (position, &v) in bd.insts.iter().enumerate() {
            let inst = &f.insts[v.0 as usize];
            validate_instruction_type(f, v, inst)?;
            match inst {
                // A phi operand must be dominated by its def AT THE
                // PREDECESSOR EDGE, not dominate the phi's own block — a
                // value from the `then` branch legitimately doesn't
                // dominate `merge` as a block, but it does reach merge via
                // exactly the `then` edge the phi records it against.
                Inst::Phi { incoming } => {
                    for (pred, val) in incoming {
                        let def_block = defined_in
                            .get(val.0 as usize)
                            .and_then(|block| *block)
                            .ok_or_else(|| format!("value {val:?} used but never defined"))?;
                        if !dominates(&idom, def_block, *pred) {
                            return Err(format!(
                                "phi operand {val:?} (defined in {def_block:?}) does not dominate predecessor {pred:?}"
                            ));
                        }
                    }
                }
                other => {
                    for used in uses_of(other) {
                        let def_block = defined_in
                            .get(used.0 as usize)
                            .and_then(|block| *block)
                            .ok_or_else(|| format!("value {used:?} used but never defined"))?;
                        if !dominates(&idom, def_block, block) {
                            return Err(format!(
                                "value {used:?} (defined in {def_block:?}) does not dominate its use in {block:?}"
                            ));
                        }
                        if def_block == block
                            && defined_at[used.0 as usize]
                                .expect("definition position was checked above")
                                >= position
                        {
                            return Err(format!(
                                "value {used:?} is used before its definition in {block:?}"
                            ));
                        }
                    }
                }
            }
        }

        match &bd.term {
            Some(Terminator::Return(v)) => {
                let def_block = defined_in
                    .get(v.0 as usize)
                    .and_then(|block| *block)
                    .ok_or_else(|| format!("return of undefined value {v:?}"))?;
                if !dominates(&idom, def_block, block) {
                    return Err(format!("returned value {v:?} does not dominate {block:?}"));
                }
                if def_block == block
                    && defined_at
                        .get(v.0 as usize)
                        .and_then(|position| *position)
                        .map_or(true, |position| position >= bd.insts.len())
                {
                    return Err(format!("returned value {v:?} is not defined in {block:?}"));
                }
            }
            Some(Terminator::Branch { cond, .. }) => {
                let def_block = defined_in
                    .get(cond.0 as usize)
                    .and_then(|block| *block)
                    .ok_or_else(|| format!("branch on undefined value {cond:?}"))?;
                if !dominates(&idom, def_block, block) {
                    return Err(format!(
                        "branch condition {cond:?} does not dominate {block:?}"
                    ));
                }
            }
            Some(Terminator::Jump(_)) => {}
            None => return Err(format!("block {block:?} has no terminator")),
        }

        for &v in &bd.insts {
            if let Inst::Phi { incoming } = &f.insts[v.0 as usize] {
                if incoming.len() != bd.preds.len() {
                    return Err(format!(
                        "phi {v:?} in {block:?} has {} operand(s) but block has {} predecessor(s)",
                        incoming.len(),
                        bd.preds.len()
                    ));
                }
            }
        }
    }

    Ok(())
}

fn validate_instruction_type(f: &Function, value: Value, inst: &Inst) -> Result<(), String> {
    let result = f.types[value.0 as usize];
    let operand_type = |operand: Value| {
        f.types
            .get(operand.0 as usize)
            .copied()
            .ok_or_else(|| format!("instruction {value:?} uses undefined value {operand:?}"))
    };
    let require = |actual: Ty, expected: Ty, what: &str| {
        if actual == expected {
            Ok(())
        } else {
            Err(format!(
                "instruction {value:?} has {what} type {actual:?}, expected {expected:?}"
            ))
        }
    };
    match inst {
        Inst::ConstF64(_) => require(result, Ty::F64, "result"),
        Inst::ConstI64(_) => require(result, Ty::I64, "result"),
        Inst::ConstBool(_) => require(result, Ty::Bool, "result"),
        Inst::Param { index, ty } => {
            let Some((_, declared)) = f.params.get(*index as usize) else {
                return Err(format!(
                    "instruction {value:?} refers to parameter index {index} outside the parameter table"
                ));
            };
            if declared != ty {
                return Err(format!(
                    "instruction {value:?} declares parameter type {ty:?}, but parameter table uses {declared:?}"
                ));
            }
            require(result, *ty, "result")
        }
        Inst::Add(a, b)
        | Inst::Sub(a, b)
        | Inst::Mul(a, b)
        | Inst::Div(a, b)
        | Inst::Rem(a, b)
        | Inst::Min(a, b)
        | Inst::Max(a, b)
        | Inst::And(a, b)
        | Inst::Or(a, b)
        | Inst::Xor(a, b)
        | Inst::Shl(a, b)
        | Inst::Shr(a, b)
        | Inst::Sar(a, b) => {
            let lhs = operand_type(*a)?;
            let rhs = operand_type(*b)?;
            if lhs != rhs {
                return Err(format!(
                    "instruction {value:?} has mismatched operand types {lhs:?} and {rhs:?}"
                ));
            }
            match inst {
                Inst::And(..) | Inst::Or(..) | Inst::Xor(..) => {
                    if !matches!(lhs, Ty::Bool | Ty::I64) {
                        return Err(format!(
                            "instruction {value:?} bitwise operands must be bool or i64"
                        ));
                    }
                }
                Inst::Shl(..) | Inst::Shr(..) | Inst::Sar(..) => {
                    require(lhs, Ty::I64, "operand")?;
                }
                _ => {
                    if !matches!(lhs, Ty::F64 | Ty::I64) {
                        return Err(format!(
                            "instruction {value:?} arithmetic operands must be f64 or i64"
                        ));
                    }
                }
            }
            require(result, lhs, "result")
        }
        Inst::Neg(a) => {
            let ty = operand_type(*a)?;
            if !matches!(ty, Ty::F64 | Ty::I64) {
                return Err(format!(
                    "instruction {value:?} negation requires f64 or i64"
                ));
            }
            require(result, ty, "result")
        }
        Inst::Not(a) => {
            let ty = operand_type(*a)?;
            if !matches!(ty, Ty::Bool | Ty::I64) {
                return Err(format!("instruction {value:?} not requires bool or i64"));
            }
            require(result, ty, "result")
        }
        Inst::Fma { a, b, c } => {
            for operand in [*a, *b, *c] {
                require(operand_type(operand)?, Ty::F64, "operand")?;
            }
            require(result, Ty::F64, "result")
        }
        Inst::Cmp { op, lhs, rhs } => {
            let lhs_ty = operand_type(*lhs)?;
            let rhs_ty = operand_type(*rhs)?;
            if lhs_ty != rhs_ty || !matches!(lhs_ty, Ty::F64 | Ty::I64 | Ty::Bool) {
                return Err(format!(
                    "instruction {value:?} has invalid comparison operands"
                ));
            }
            if matches!(op, CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge) && lhs_ty == Ty::Bool {
                return Err(format!(
                    "instruction {value:?} uses an ordered comparison on bool operands"
                ));
            }
            require(result, Ty::Bool, "result")
        }
        Inst::Sqrt(a)
        | Inst::Abs(a)
        | Inst::Floor(a)
        | Inst::Ceil(a)
        | Inst::Round(a)
        | Inst::Trunc(a) => {
            require(operand_type(*a)?, Ty::F64, "operand")?;
            require(result, Ty::F64, "result")
        }
        Inst::Call { func, args } => {
            let expected_arity = match func {
                LibFunc::Pow | LibFunc::Fmod => 2,
                LibFunc::Sin | LibFunc::Cos | LibFunc::Tan | LibFunc::Exp | LibFunc::Log => 1,
            };
            if args.len() != expected_arity
                || args
                    .iter()
                    .any(|arg| operand_type(*arg).ok() != Some(Ty::F64))
            {
                return Err(format!(
                    "instruction {value:?} intrinsic arguments must be f64"
                ));
            }
            require(result, Ty::F64, "result")
        }
        Inst::ExternalCall { args, .. } => {
            for arg in args {
                let _ = operand_type(*arg)?;
            }
            Ok(())
        }
        Inst::IToF(a) => {
            require(operand_type(*a)?, Ty::I64, "operand")?;
            require(result, Ty::F64, "result")
        }
        Inst::FToI(a) => {
            require(operand_type(*a)?, Ty::F64, "operand")?;
            require(result, Ty::I64, "result")
        }
        Inst::Phi { incoming } => {
            for (_, operand) in incoming {
                require(operand_type(*operand)?, result, "operand")?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_syntax::lexer::lex;
    use forge_syntax::parser::parse;
    use forge_syntax::resolve::resolve;
    use forge_syntax::span::Span;
    use forge_syntax::typeck::typecheck;

    fn lowered(src: &str) -> Function {
        let (tokens, _) = lex(src);
        let (ast, _) = parse(&tokens);
        let typed = typecheck(resolve(ast)).expect("should type-check");
        crate::lower::lower(&typed)
    }

    #[test]
    fn straight_line_expression_verifies() {
        let f = lowered("sqrt(x * x + y * y)");
        assert!(verify(&f).is_ok());
    }

    #[test]
    fn if_expression_verifies() {
        let f = lowered("if x > 0.0 then x else -x");
        assert!(verify(&f).is_ok());
    }

    #[test]
    fn rejects_use_before_def() {
        let mut f = lowered("x * x");
        // Hand-corrupt: make the Mul's second operand a not-yet-defined
        // value index (one past the end of insts).
        let bad = Value(f.insts.len() as u32);
        let last = f.insts.len() - 1;
        if let Inst::Mul(_, b) = &mut f.insts[last] {
            *b = bad;
        }
        assert!(verify(&f).is_err());
    }

    #[test]
    fn rejects_same_block_use_before_definition_even_when_index_exists() {
        let mut f = lowered("x + 1.0");
        let last = f.blocks[0].insts.len() - 1;
        let future = f.blocks[0].insts[last];
        if let Inst::Add(lhs, _) = &mut f.insts[future.0 as usize] {
            *lhs = future;
        }
        let error = verify(&f).expect_err("a value cannot use its own definition");
        assert!(error.contains("used before its definition"));
    }

    #[test]
    fn rejects_duplicate_instruction_placement() {
        let mut f = lowered("x + 1.0");
        let value = f.blocks[0].insts[0];
        f.blocks[0].insts.push(value);
        let error = verify(&f).expect_err("a value must be listed in one block once");
        assert!(error.contains("listed in both"));
    }

    #[test]
    fn rejects_out_of_range_instruction_reference_without_panicking() {
        let mut f = lowered("x + 1.0");
        f.blocks[0].insts[0] = Value(u32::MAX);
        let error = verify(&f).expect_err("malformed IR should be reported");
        assert!(error.contains("outside the instruction table"));
    }

    #[test]
    fn rejects_mismatched_instruction_type_table() {
        let mut f = lowered("x + 1.0");
        f.types[0] = Ty::I64;
        let error = verify(&f).expect_err("malformed instruction types should be reported");
        assert!(error.contains("result type"));
    }

    #[test]
    fn rejects_phi_with_wrong_operand_count() {
        let mut f = lowered("if x > 0.0 then x else -x");
        let phi_idx = f
            .insts
            .iter()
            .position(|i| matches!(i, Inst::Phi { .. }))
            .unwrap();
        if let Inst::Phi { incoming } = &mut f.insts[phi_idx] {
            incoming.pop(); // now has 1 operand but its block has 2 preds
        }
        assert!(verify(&f).is_err());
    }

    #[test]
    fn dominance_rejects_a_value_used_outside_its_defining_branch() {
        // Hand-build: entry branches to then/else/merge; merge's Return
        // illegally uses a value defined only in `else` (the Neg of x),
        // which does not dominate `merge`.
        //
        // Note: the `then` branch here is a bare `x`, which the SSA builder
        // resolves to the entry block's Param value directly (a
        // single-predecessor block reads through to its predecessor without
        // emitting a local instruction) — so `then`'s block has *no* local
        // instructions to hijack. `else`'s `-x` does emit a local `Neg`,
        // which is what we corrupt the Return to use instead of the phi.
        let mut f = lowered("if x > 0.0 then x else -x");
        let else_block = f.blocks[2].insts.clone();
        let else_val = *else_block
            .first()
            .expect("else block has at least one inst");
        // Force merge's Return to use the else-only value instead of the phi.
        let merge_idx = f.blocks.len() - 1;
        f.blocks[merge_idx].term = Some(Terminator::Return(else_val));
        assert!(verify(&f).is_err());
        let _ = Span::new(0, 0); // silence unused import if Span is otherwise unused
    }

    #[test]
    fn rejects_branch_condition_that_does_not_dominate_its_block() {
        // entry's Branch.cond illegally points at a value defined only in
        // `else` (block 2) — block 2 does not dominate entry (block 0),
        // which has no predecessors at all, so this is a genuine violation,
        // not just a forward reference within the same block.
        let mut f = lowered("if x > 0.0 then x else -x");
        let else_val = *f.blocks[2]
            .insts
            .first()
            .expect("else block has at least one inst");
        let Some(Terminator::Branch { then_, else_, .. }) = f.blocks[0].term.clone() else {
            panic!("entry block must end in a Branch");
        };
        f.blocks[0].term = Some(Terminator::Branch {
            cond: else_val,
            then_,
            else_,
        });
        assert!(verify(&f).is_err());
    }
}
