// crates/forge-opt/src/gvn.rs
//
// Dominator-tree-scoped global value numbering / common subexpression
// elimination.
//
// A flat, whole-function hash table (hash-cons every instruction once, keyed
// by canonicalized opcode+operands, in *some* linear visitation order) is
// the natural first implementation and is UNSOUND: it would CSE two
// structurally identical instructions that live in non-dominating sibling
// blocks (e.g. the `then` and `else` arms of an `if`). Redirecting the
// `else` copy's uses to the `then` copy's `Value` would produce IR where a
// use is not dominated by its def -- not merely "wrong optimization
// output", but literally invalid SSA, which `forge_ir::verify::verify`
// would (correctly) reject.
//
// The fix is to scope the hash table to the current root-to-node path in
// the dominator tree, exactly like LLVM's GVN: walk the dominator tree in
// preorder, maintaining ONE hash table that instructions get inserted into
// on the way down and removed from on the way back up (once we leave a
// subtree, nothing in it dominates anything outside it anymore). This way,
// an instruction is only ever visible to `table` while we're visiting a
// block it actually dominates.

use forge_ir::{replace_value_everywhere, Block, CmpOp, Function, Inst, Ty, Value};
use rustc_hash::FxHashMap;

pub struct Gvn;

impl crate::Pass for Gvn {
    fn name(&self) -> &'static str {
        "gvn"
    }
    fn run(&mut self, f: &mut Function) -> bool {
        let idom = forge_ir::dominance::compute_dominators(f);
        let children = dom_tree_children(&idom);

        let mut table: FxHashMap<Inst, Value> = FxHashMap::default();
        let mut changed = false;
        visit(f.entry, f, &children, &mut table, &mut changed);
        changed
    }
}

/// Inverts the `idom` array (child -> parent) into `parent -> children`,
/// which is what a preorder dominator-tree walk needs. The entry block is
/// its own idom (see `compute_dominators`), so it's explicitly excluded as
/// a "child of itself" here.
fn dom_tree_children(idom: &[Option<Block>]) -> FxHashMap<Block, Vec<Block>> {
    let mut children: FxHashMap<Block, Vec<Block>> = FxHashMap::default();
    for (i, parent) in idom.iter().enumerate() {
        let b = Block(i as u32);
        if let Some(p) = parent {
            if *p != b {
                children.entry(*p).or_default().push(b);
            }
        }
    }
    children
}

/// Recursive preorder walk of the dominator tree, maintaining `table` as a
/// hash table scoped to the current root-to-node path.
///
/// A CSE hit does more than redirect the redundant instruction's uses: it
/// also overwrites its own `f.insts` slot with an inert, type-matched
/// constant, AND splices it out of its block's `insts` list. These two
/// steps are not equally load-bearing:
///
/// - Overwriting the `f.insts` slot is REQUIRED. Value indices are
///   permanent (they double as `Vec` indices throughout the IR -- see
///   `Builder::emit`), so the redundant instruction's slot can never be
///   physically removed; merely redirecting its uses (the pattern
///   `simplify.rs`/`fold.rs` use for their `UseExisting` rewrites) would
///   leave a structurally-identical, now-argument-only duplicate sitting in
///   `f.insts` forever -- undetectable by `verify()`/`interp()` (which only
///   follow live block membership) but very much still counted by anything
///   that scans the raw instruction array, including this pass's own test
///   helper (`count_op`, which does exactly that).
/// - Splicing it out of `block.insts` is EXTRA hygiene, not strictly needed
///   for `verify()` to pass (verify only ever looks at values reachable
///   from a block's `insts`, and this Value has none left pointing to it
///   either way). It's done anyway because it's cheap and it forecloses a
///   real, if non-unsound, footgun: without it, the dead slot stays visible
///   to a *later* preorder visit of the *same* block (this pass re-runs
///   every fixed-point round), where it could wrongly resurface as a
///   "canonical" definition for some later CSE hit -- correct per `verify()`
///   but a wasteful, confusing one to debug. Verified empirically (dumped
///   IR before/after on several cases, including this file's
///   multi-level-dominator-chain test) rather than assumed.
fn visit(
    block: Block,
    f: &mut Function,
    children: &FxHashMap<Block, Vec<Block>>,
    table: &mut FxHashMap<Inst, Value>,
    changed: &mut bool,
) {
    // Keys this call inserted into `table` -- removed again before
    // returning, so that once we're done with this block's dominator
    // subtree, none of its instructions remain visible to blocks outside
    // it (siblings, or anything below a sibling).
    let mut inserted: Vec<Inst> = Vec::new();

    let insts = f.blocks[block.0 as usize].insts.clone();
    let mut live_insts: Vec<Value> = Vec::with_capacity(insts.len());
    for v in insts {
        let key = canonicalize(&f.insts[v.0 as usize]);
        if let Some(&existing) = table.get(&key) {
            replace_value_everywhere(f, v, existing);
            f.insts[v.0 as usize] = dead_placeholder(f.types[v.0 as usize]);
            *changed = true;
            // Not pushed to `live_insts` -- this Value no longer belongs to
            // any block, so nothing will ever visit it (in this pass, in a
            // later round, or in verify/interp/print) again.
        } else {
            table.insert(key.clone(), v);
            inserted.push(key);
            live_insts.push(v);
        }
    }
    f.blocks[block.0 as usize].insts = live_insts;

    if let Some(kids) = children.get(&block) {
        // Cloning the children list up front decouples the borrow of
        // `children` from the recursive calls below (which need `f`, and
        // `children` was itself computed from an `f` snapshot that's now
        // stale but still structurally valid -- blocks are never added or
        // removed by this pass).
        for &child in kids.clone().iter() {
            visit(child, f, children, table, changed);
        }
    }

    for key in inserted {
        table.remove(&key);
    }
}

/// A type-matched, operand-free constant to leave behind in a spliced-out
/// instruction's own `f.insts` slot. Never read by anything (the Value no
/// longer appears in any block's `insts`, so verify/interp/print all skip
/// it), but keeping it type-consistent with the slot's `f.types` entry
/// (rather than e.g. always emitting `ConstI64(0)`) costs nothing and avoids
/// planting a nonsensical `Ty::Bool` / `Inst::ConstF64` mismatch for anyone
/// debugging with a raw dump of `f.insts`.
fn dead_placeholder(ty: Ty) -> Inst {
    match ty {
        Ty::F64 => Inst::ConstF64(0.0f64.to_bits()),
        Ty::I64 => Inst::ConstI64(0),
        Ty::Bool => Inst::ConstBool(false),
    }
}

/// For fully-commutative ops (Add/Mul/And/Or/Xor, and Cmp{Eq,Ne}), sort the
/// two operands by `Value` index so `a+b` and `b+a` hash identically. Every
/// other op -- including Cmp{Lt,Le,Gt,Ge}, which are NOT commutative --
/// keeps its operand order as-is.
fn canonicalize(inst: &Inst) -> Inst {
    match inst {
        Inst::Add(a, b) => Inst::Add(min2(*a, *b), max2(*a, *b)),
        Inst::Mul(a, b) => Inst::Mul(min2(*a, *b), max2(*a, *b)),
        Inst::And(a, b) => Inst::And(min2(*a, *b), max2(*a, *b)),
        Inst::Or(a, b) => Inst::Or(min2(*a, *b), max2(*a, *b)),
        Inst::Xor(a, b) => Inst::Xor(min2(*a, *b), max2(*a, *b)),
        Inst::Cmp {
            op: op @ (CmpOp::Eq | CmpOp::Ne),
            lhs,
            rhs,
        } => Inst::Cmp {
            op: *op,
            lhs: min2(*lhs, *rhs),
            rhs: max2(*lhs, *rhs),
        },
        other => other.clone(),
    }
}

fn min2(a: Value, b: Value) -> Value {
    if a.0 <= b.0 {
        a
    } else {
        b
    }
}
fn max2(a: Value, b: Value) -> Value {
    if a.0 <= b.0 {
        b
    } else {
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pass;
    use forge_ir::Function;
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

    fn count_op<F: Fn(&Inst) -> bool>(f: &Function, pred: F) -> usize {
        f.insts.iter().filter(|i| pred(i)).count()
    }

    #[test]
    fn repeated_subexpression_cses_to_one_add() {
        // (a+b)*(a+b): the two (a+b) subexpressions must become one Add.
        let mut f = lowered("(a + b) * (a + b)");
        let before = count_op(&f, |i| matches!(i, Inst::Add(_, _)));
        assert_eq!(before, 2, "sanity: two separate Adds before GVN");
        Gvn.run(&mut f);
        let after = count_op(&f, |i| matches!(i, Inst::Add(_, _)));
        assert_eq!(
            after, 1,
            "GVN should have merged the two identical Adds into one"
        );
    }

    #[test]
    fn commutative_operands_cse_together() {
        // a+b and b+a must be recognized as the same value.
        let mut f = lowered("(a + b) + (b + a)");
        Gvn.run(&mut f);
        let after = count_op(&f, |i| matches!(i, Inst::Add(_, _)));
        // (a+b) appears twice (once as a+b, once as b+a) plus the outer add
        // = 3 Adds total before CSE; after CSE the two inner ones merge, so
        // 2 remain (the shared a+b, and the outer add of it with itself).
        assert_eq!(
            after, 2,
            "b+a should CSE with a+b via commutative canonicalization"
        );
    }

    #[test]
    fn does_not_cse_across_non_dominating_sibling_blocks() {
        // then and else each independently compute `x * x` -- these must
        // NOT be merged into one instruction, since neither branch
        // dominates the other. A flat (non-dominator-scoped) GVN would
        // incorrectly merge these; this is the test that catches that bug.
        let mut f = lowered("if x > 0.0 then x * x else x * x");
        let before = count_op(&f, |i| matches!(i, Inst::Mul(_, _)));
        assert_eq!(before, 2, "sanity: one Mul per branch before GVN");
        Gvn.run(&mut f);
        let after = count_op(&f, |i| matches!(i, Inst::Mul(_, _)));
        assert_eq!(
            after, 2,
            "the two branch-local Muls must NOT be CSE'd across non-dominating blocks"
        );
        assert!(
            forge_ir::verify::verify(&f).is_ok(),
            "result must still be valid SSA"
        );
    }

    #[test]
    fn cse_result_still_passes_the_verifier() {
        let mut f = lowered("(a + b) * (a + b) + sqrt(a + b)");
        Gvn.run(&mut f);
        assert!(forge_ir::verify::verify(&f).is_ok());
    }

    #[test]
    fn cses_across_a_multi_level_dominator_chain() {
        // Outer x*x at entry; nested if-in-if where the innermost then-branch
        // recomputes x*x -- two dominator levels down from entry (entry ->
        // outer-then -> inner-then). All 4 tests above exercise only a
        // single block or a 2-block tree (entry+child, or two siblings);
        // none stress the "insert on the way down, remove on the way back
        // up" recursion across a genuine grandparent -> parent -> child
        // chain, which is exactly what could silently regress to a
        // shallow/flat table without a test noticing. Must CSE.
        let mut f = lowered("x * x + (if x > 0.0 then (if y > 0.0 then x * x else 1.0) else 2.0)");
        let before = count_op(&f, |i| matches!(i, Inst::Mul(_, _)));
        assert_eq!(before, 2, "sanity: two separate x*x before GVN");
        Gvn.run(&mut f);
        let after = count_op(&f, |i| matches!(i, Inst::Mul(_, _)));
        assert_eq!(
            after, 1,
            "the nested x*x should CSE against entry's, two dominator levels up"
        );
        assert!(forge_ir::verify::verify(&f).is_ok());
    }
}
