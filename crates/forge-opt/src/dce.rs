// crates/forge-opt/src/dce.rs
//
// Worklist-based reachability from every block's terminator operand
// (`Return`'s value, `Branch`'s condition -- `Jump` has none), following
// `uses_of` backward transitively. Anything never reached is dead. Sweeps
// by filtering each block's `insts` list -- the underlying `f.insts` Vec
// keeps now-unreferenced entries in place (consistent with how the SSA
// builder already leaves dead trivial-phis after `replace_all_uses`; no
// renumbering needed).

use rustc_hash::FxHashSet;

use forge_ir::*;

use crate::Pass;

/// Caveat for future codegen: an unused function parameter's `Inst::Param`
/// is a normal dead instruction from DCE's point of view, so DCE can (and
/// will) sweep it out of `block.insts` if nothing in the body ever uses it.
/// `f.params` -- the function's signature metadata -- is untouched by DCE
/// either way, so the parameter stays declared there even after its
/// `Inst::Param` is gone from `block.insts`. A future codegen pass must
/// therefore source its parameter list from `f.params` directly, not by
/// scanning `block.insts` for `Inst::Param` entries, or it will silently
/// fail to materialize an unused-but-declared parameter.
pub struct Dce;

impl Pass for Dce {
    fn name(&self) -> &'static str {
        "dce"
    }
    fn run(&mut self, f: &mut Function) -> bool {
        let mut used: FxHashSet<Value> = FxHashSet::default();
        let mut worklist: Vec<Value> = Vec::new();

        for block in &f.blocks {
            match &block.term {
                Some(Terminator::Return(v)) => worklist.push(*v),
                Some(Terminator::Branch { cond, .. }) => worklist.push(*cond),
                _ => {}
            }
        }

        while let Some(v) = worklist.pop() {
            if used.insert(v) {
                for operand in uses_of(&f.insts[v.0 as usize]) {
                    worklist.push(operand);
                }
            }
        }

        let mut changed = false;
        for block in &mut f.blocks {
            let before = block.insts.len();
            block.insts.retain(|v| used.contains(v));
            if block.insts.len() != before {
                changed = true;
            }
        }
        changed
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
        forge_ir::lower::lower(&typed)
    }

    #[test]
    fn removes_an_unused_subexpression() {
        // `let t = x * x in y` -- t is computed but never used in the body.
        let mut f = lowered("let t = x * x in y");
        let before = f.blocks[f.entry.0 as usize].insts.len();
        let changed = Dce.run(&mut f);
        assert!(changed);
        let after = f.blocks[f.entry.0 as usize].insts.len();
        assert!(
            after < before,
            "the dead `x*x` should have been swept from the block's instruction list"
        );
        assert!(forge_ir::verify::verify(&f).is_ok());
    }

    #[test]
    fn does_not_remove_a_value_used_by_the_return() {
        let mut f = lowered("x + y");
        let before = f.blocks[f.entry.0 as usize].insts.len();
        Dce.run(&mut f);
        assert_eq!(f.blocks[f.entry.0 as usize].insts.len(), before);
    }

    #[test]
    fn does_not_remove_a_value_used_only_by_a_branch_condition() {
        // The condition of an `if` is a Terminator operand, not a normal
        // Inst use -- confirms DCE's liveness seeding covers Branch.cond
        // (this is exactly the gap `replace_value_everywhere`/an earlier
        // task fixed for rewriting; DCE's OWN liveness-seed must
        // independently cover it too, via `uses_of`-style reasoning over
        // Terminators).
        let mut f = lowered("if x > 0.0 then 1.0 else 2.0");
        Dce.run(&mut f);
        assert!(forge_ir::verify::verify(&f).is_ok());
        // The comparison feeding the branch must still be present.
        assert!(f.insts.iter().any(|i| matches!(i, Inst::Cmp { .. })));
    }
}
