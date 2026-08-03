// crates/forge-ir/src/verify.rs

use rustc_hash::FxHashMap;

use crate::dominance::{compute_dominators, dominates};
use crate::ir::*;

pub fn verify(f: &Function) -> Result<(), String> {
    let idom = compute_dominators(f);

    let mut defined_in: FxHashMap<Value, Block> = FxHashMap::default();
    for (bi, bd) in f.blocks.iter().enumerate() {
        for &v in &bd.insts {
            defined_in.insert(v, Block(bi as u32));
        }
    }

    for (bi, bd) in f.blocks.iter().enumerate() {
        let block = Block(bi as u32);

        for &v in &bd.insts {
            match &f.insts[v.0 as usize] {
                // A phi operand must be dominated by its def AT THE
                // PREDECESSOR EDGE, not dominate the phi's own block — a
                // value from the `then` branch legitimately doesn't
                // dominate `merge` as a block, but it does reach merge via
                // exactly the `then` edge the phi records it against.
                Inst::Phi { incoming } => {
                    for (pred, val) in incoming {
                        let def_block = *defined_in.get(val)
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
                        let def_block = *defined_in.get(&used)
                            .ok_or_else(|| format!("value {used:?} used but never defined"))?;
                        if !dominates(&idom, def_block, block) {
                            return Err(format!(
                                "value {used:?} (defined in {def_block:?}) does not dominate its use in {block:?}"
                            ));
                        }
                    }
                }
            }
        }

        match &bd.term {
            Some(Terminator::Return(v)) => {
                let def_block = *defined_in.get(v)
                    .ok_or_else(|| format!("return of undefined value {v:?}"))?;
                if !dominates(&idom, def_block, block) {
                    return Err(format!("returned value {v:?} does not dominate {block:?}"));
                }
            }
            Some(_) => {}
            None => return Err(format!("block {block:?} has no terminator")),
        }

        for &v in &bd.insts {
            if let Inst::Phi { incoming } = &f.insts[v.0 as usize] {
                if incoming.len() != bd.preds.len() {
                    return Err(format!(
                        "phi {v:?} in {block:?} has {} operand(s) but block has {} predecessor(s)",
                        incoming.len(), bd.preds.len()
                    ));
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_syntax::lexer::lex;
    use forge_syntax::parser::parse;
    use forge_syntax::resolve::resolve;
    use forge_syntax::typeck::typecheck;
    use forge_syntax::span::Span;

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
        if let Inst::Mul(_, b) = &mut f.insts[last] { *b = bad; }
        assert!(verify(&f).is_err());
    }

    #[test]
    fn rejects_phi_with_wrong_operand_count() {
        let mut f = lowered("if x > 0.0 then x else -x");
        let phi_idx = f.insts.iter().position(|i| matches!(i, Inst::Phi { .. })).unwrap();
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
        let else_val = *else_block.first().expect("else block has at least one inst");
        // Force merge's Return to use the else-only value instead of the phi.
        let merge_idx = f.blocks.len() - 1;
        f.blocks[merge_idx].term = Some(Terminator::Return(else_val));
        assert!(verify(&f).is_err());
        let _ = Span::new(0, 0); // silence unused import if Span is otherwise unused
    }
}
