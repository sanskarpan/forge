// crates/forge-opt/src/reassoc.rs
//
// i64-only reassociation: flattens a maximal chain of the same associative
// op (Add or Mul) into its leaf operands, then rebuilds a balanced tree from
// that flat leaf list instead of the original (left-leaning, from how the
// parser/lowerer naturally build a chain of binary ops) linear-depth chain.
// Shortens the longest dependency chain from O(n) to O(log n) for an n-leaf
// chain -- more instruction-level parallelism for a codegen backend /
// superscalar CPU to exploit, at zero cost in instruction count (same number
// of Add/Mul ops, just regrouped).
//
// ## Why i64 only
//
// Integer add/mul under two's-complement wraparound (this IR's `Add`/`Mul`
// are `wrapping_add`/`wrapping_mul` -- see `interp.rs`) are EXACTLY
// associative: `(a +_w b) +_w c == a +_w (b +_w c)` for every `a,b,c: i64`,
// no exceptions, because wrapping arithmetic is arithmetic mod 2^64, and
// modular addition/multiplication are associative regardless of
// intermediate overflow. Floating-point add/mul are NOT associative under
// rounding: `(a + b) + c` and `a + (b + c)` can round to different results
// (a textbook example: `(1e16 + 1.0) + -1e16 == 0.0` but
// `1e16 + (1.0 + -1e16) == 1.0`). Reassociating f64 without an explicit
// `--fast-math` opt-in would silently change a program's answer -- a real
// correctness bug, not just a missed optimization -- so this pass gates
// strictly on `Ty::I64`, checked explicitly (never assumed) at both the
// chain root and every node the flatten walk descends into.
//
// ## Multi-use intermediate values
//
// A chain like `Add(Add(Add(a,b),c),d)` might have its INTERMEDIATE nodes
// (the inner `Add(a,b)`, or `Add(Add(a,b),c)`) referenced from somewhere
// else too -- not just as the next link in the chain -- e.g.
// `let t = a + b in (t + c) + d * t`, where `t` is both folded into the sum
// AND used again separately. Flattening through such a node and discarding
// it would silently break that other reference. The fix: only flatten
// THROUGH a node if it has EXACTLY ONE use in the whole function (computed
// once, up front, over the function's original -- pre-mutation -- LIVE
// instructions only, i.e. `f.blocks[*].insts`, not the raw `f.insts` backing
// array, which can still hold entries DCE has already swept from every
// block -- see `analyze_uses`'s doc comment for why that distinction is
// load-bearing, not cosmetic). A node with more than one use is treated as
// an opaque
// LEAF of the flattened chain instead -- its own instruction is left
// completely untouched (never rewritten, never removed; this pass, like
// GVN, never deletes an instruction outright, only redirects uses of the
// chain's own root and lets DCE sweep what's left unreferenced), so every
// other reference to it stays valid no matter how the rest of the chain
// gets rebuilt.

use forge_ir::{
    insert_before, replace_value_everywhere, uses_of, Block, Function, Inst, Terminator, Ty, Value,
};
use forge_syntax::span::Span;
use rustc_hash::FxHashMap;

pub struct Reassociate;

impl crate::Pass for Reassociate {
    fn name(&self) -> &'static str {
        "reassoc"
    }

    fn run(&mut self, f: &mut Function) -> bool {
        let (use_counts, sole_consumer) = analyze_uses(f);
        let mut changed = false;

        for block_idx in 0..f.blocks.len() {
            let block = Block(block_idx as u32);
            let snapshot = f.blocks[block.0 as usize].insts.clone();
            for v in snapshot {
                let Some(op) = assoc_i64_op(f, v) else {
                    continue;
                };

                // `v` is an interior link of a larger chain if its ONLY use
                // is as an operand of a matching-op parent -- that parent
                // (strictly higher `Value` index than `v`, since this IR's
                // SSA values are numbered in def order) will itself be
                // visited later in this same scan and flatten all the way
                // through `v` then. Processing `v` here too would at best
                // duplicate that work and at worst rebuild a shallower,
                // already-superseded subtree the parent's later pass would
                // then (harmlessly, but wastefully) treat as an opaque leaf.
                if let Some(&parent) = sole_consumer.get(&v) {
                    if assoc_i64_op(f, parent) == Some(op) {
                        continue;
                    }
                }

                let (leaves, orig_depth) = flatten(f, v, op, &use_counts);
                if leaves.len() < 4 {
                    // A 2- or 3-leaf chain is already as shallow as any
                    // regrouping could make it (depth 1 or 2 either way) --
                    // rebuilding would just churn out an identical-shape
                    // duplicate for no benefit.
                    continue;
                }
                let balanced_depth = leaves.len().next_power_of_two().trailing_zeros();
                if balanced_depth >= orig_depth {
                    continue;
                }

                let ty = f.types[v.0 as usize];
                let span = f.spans[v.0 as usize];
                // Insert the new tree at `v`'s CURRENT position in
                // `block.insts` (not appended at the end -- see
                // `build_balanced`'s doc comment for why position, not just
                // dominance, matters here).
                let mut pos = f.blocks[block.0 as usize]
                    .insts
                    .iter()
                    .position(|&x| x == v)
                    .expect("root value must still be present in its own block's inst list");
                let new_root = build_balanced(f, block, &leaves, op, ty, span, &mut pos);
                replace_value_everywhere(f, v, new_root);
                changed = true;
            }
        }
        changed
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AssocOp {
    Add,
    Mul,
}

impl AssocOp {
    fn operands(self, inst: &Inst) -> Option<(Value, Value)> {
        match (self, inst) {
            (AssocOp::Add, Inst::Add(a, b)) => Some((*a, *b)),
            (AssocOp::Mul, Inst::Mul(a, b)) => Some((*a, *b)),
            _ => None,
        }
    }
    fn make(self, a: Value, b: Value) -> Inst {
        match self {
            AssocOp::Add => Inst::Add(a, b),
            AssocOp::Mul => Inst::Mul(a, b),
        }
    }
}

/// `Some(op)` if `v` is an `Add`/`Mul` instruction whose RESULT type is
/// `Ty::I64` -- checked explicitly rather than assumed, since silently
/// reassociating an f64 chain would be the real correctness bug this pass
/// exists to avoid (see module doc comment).
fn assoc_i64_op(f: &Function, v: Value) -> Option<AssocOp> {
    if f.types[v.0 as usize] != Ty::I64 {
        return None;
    }
    match f.insts[v.0 as usize] {
        Inst::Add(..) => Some(AssocOp::Add),
        Inst::Mul(..) => Some(AssocOp::Mul),
        _ => None,
    }
}

/// One pass over the function's ORIGINAL (pre-mutation) LIVE instructions
/// and terminators: `counts[v]` is the total number of operand-uses of `v`
/// anywhere (instructions + terminators), and `sole_consumer[v]` is `Some(w)`
/// only when `v` has EXACTLY one use in the whole function and that use is
/// an instruction operand (not a terminator) -- `w` is the instruction that
/// uses it. Computed once, up front, and reused for the rest of `run`: this
/// pass only ever ADDS instructions and redirects uses of a chain's own
/// (now-dead) root, so no existing instruction's operands change in a way
/// that would invalidate these counts for any value other than the
/// just-processed root itself (which is never queried again afterward).
///
/// MUST scan only instructions actually reachable via `f.blocks[*].insts`
/// (walked below), never the raw `f.insts` backing array directly. DCE
/// (`dce.rs`) deliberately leaves a swept instruction's `Inst` sitting in
/// `f.insts` forever -- only its entry in `block.insts` is removed -- so a
/// naive `f.insts.iter().enumerate()` scan would still "see" a dead
/// instruction's operand references as real uses. Concretely, that bug (a
/// real one, found by review, not hypothetical) permanently blocks
/// reassociation from treating a value as safely flattenable even after the
/// ONLY thing holding its use count above one has become provably
/// unreachable: `reassoc` runs before `dce` in the pipeline (see `lib.rs`),
/// so a since-dead binding's `Inst` gets swept from `block.insts` by the
/// SAME round's `dce`, but a raw-`f.insts` scan would still count it in
/// EVERY subsequent round too, since that backing-array entry never goes
/// away -- capping the achievable rebalancing depth forever instead of
/// letting a later round finish the job once DCE has caught up.
fn analyze_uses(f: &Function) -> (FxHashMap<Value, u32>, FxHashMap<Value, Value>) {
    let mut counts: FxHashMap<Value, u32> = FxHashMap::default();
    let mut consumers: FxHashMap<Value, Vec<Value>> = FxHashMap::default();

    for block in &f.blocks {
        for &user in &block.insts {
            for operand in uses_of(&f.insts[user.0 as usize]) {
                *counts.entry(operand).or_insert(0) += 1;
                consumers.entry(operand).or_default().push(user);
            }
        }
        match &block.term {
            Some(Terminator::Return(v)) => *counts.entry(*v).or_insert(0) += 1,
            Some(Terminator::Branch { cond, .. }) => *counts.entry(*cond).or_insert(0) += 1,
            _ => {}
        }
    }

    let sole_consumer = consumers
        .into_iter()
        .filter(|(v, cs)| cs.len() == 1 && counts.get(v) == Some(&1))
        .map(|(v, cs)| (v, cs[0]))
        .collect();

    (counts, sole_consumer)
}

/// Flattens the maximal same-op chain rooted at `root` into its leaf operand
/// list (left-to-right, associativity-only regrouping -- leaf ORDER is
/// preserved, so no commutativity is needed for correctness) plus the
/// ORIGINAL chain's true dependency depth (computed by the same walk, not
/// assumed to be `leaves.len() - 1` -- a multi-use boundary can make the
/// real depth shallower than that guess). `root` itself is always descended
/// into regardless of its own use count -- how many places reference `root`
/// doesn't matter, since every one of them gets redirected to the rebuilt
/// tree's new root via `replace_value_everywhere`.
fn flatten(
    f: &Function,
    root: Value,
    op: AssocOp,
    use_counts: &FxHashMap<Value, u32>,
) -> (Vec<Value>, u32) {
    let (a, b) = op
        .operands(&f.insts[root.0 as usize])
        .expect("caller already confirmed `root` matches `op`");
    let (mut leaves, depth_a) = flatten_operand(f, a, op, use_counts);
    let (b_leaves, depth_b) = flatten_operand(f, b, op, use_counts);
    leaves.extend(b_leaves);
    (leaves, 1 + depth_a.max(depth_b))
}

/// Continues flattening through `v` only if it's itself a matching-op I64
/// node AND has exactly one use in the whole function -- otherwise `v` is an
/// opaque leaf (see module doc comment's "multi-use intermediate values"
/// section for why the use-count gate is load-bearing here, not just an
/// optimization).
fn flatten_operand(
    f: &Function,
    v: Value,
    op: AssocOp,
    use_counts: &FxHashMap<Value, u32>,
) -> (Vec<Value>, u32) {
    let single_use = use_counts.get(&v).copied().unwrap_or(0) == 1;
    if assoc_i64_op(f, v) == Some(op) && single_use {
        flatten(f, v, op, use_counts)
    } else {
        (vec![v], 0)
    }
}

/// Rebuilds a balanced tree over `leaves` (split-in-half, recursively) --
/// depth `ceil(log2(leaves.len()))` instead of the flattened chain's
/// original (up to) `leaves.len() - 1`. Leaf order is preserved exactly, so
/// this is pure associativity regrouping (no operand reordering), which is
/// all wrapping i64 add/mul needs to stay exact -- see module doc comment.
///
/// Every new instruction is inserted at `*pos` (post-order: both children
/// before the node combining them), which starts as the OLD root's own
/// position in `block.insts` and advances by one after each insertion --
/// `forge_ir::insert_before`'s chaining pattern (see its doc comment),
/// shared with `strength.rs`'s div/rem sign-fixup sequences. This is NOT
/// just a dominance nicety: `verify()` only checks block-granularity
/// dominance, but `interp.rs` evaluates a block's instructions by walking
/// `block.insts` strictly IN ORDER, filling each `Value`'s slot as it goes
/// (see `interp.rs`'s `get`, which panics "undefined value" if a use is
/// evaluated before its def). Appending new instructions at the END of
/// `block.insts` instead of at the root's own position was tried first and
/// FAILED exactly this way: some existing instruction defined AFTER the
/// original root but BEFORE the end of the block (e.g. this pass's own test
/// harness's `& -1` forcing-wrapper, which the parser always places right
/// after the expression it wraps) could already depend on the root, and
/// after `replace_value_everywhere` redirects that dependency to the new
/// tree's root, the new tree must be defined no LATER than where the old
/// root used to sit, not merely "somewhere before its own uses" -- hence
/// inserting at the old position, not appending at the end.
fn build_balanced(
    f: &mut Function,
    block: Block,
    leaves: &[Value],
    op: AssocOp,
    ty: Ty,
    span: Span,
    pos: &mut usize,
) -> Value {
    if leaves.len() == 1 {
        return leaves[0];
    }
    let mid = leaves.len() / 2;
    let left = build_balanced(f, block, &leaves[..mid], op, ty, span, pos);
    let right = build_balanced(f, block, &leaves[mid..], op, ty, span, pos);
    let v = insert_before(f, block, *pos, op.make(left, right), ty, span);
    *pos += 1;
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pass;
    use forge_ir::interp::{interpret, RtValue};
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

    /// See `simplify.rs`/`strength.rs`'s identically-named helper for the
    /// full explanation: bare identifiers default to F64 under this
    /// project's type inference unless something forces I64 top-down. `&
    /// -1` is a bitwise no-op that forces I64 through the whole wrapped
    /// expression (via `infer_expect`'s `BitAnd => Some(Ty::I64)` arm
    /// propagating down through `Add`/`Mul`'s pass-through inference)
    /// without disturbing which SSA `Value` each identifier resolves to.
    fn lowered_i64(src: &str) -> Function {
        lowered(&format!("({src}) & -1"))
    }

    /// Longest chain of Add/Mul instructions ending at the function's
    /// return value, counted by walking operand-of-operand as far as it
    /// stays within the same associative op.
    fn dependency_depth(f: &Function, v: Value) -> u32 {
        match &f.insts[v.0 as usize] {
            Inst::Add(a, b) | Inst::Mul(a, b) => {
                1 + dependency_depth(f, *a).max(dependency_depth(f, *b))
            }
            _ => 0,
        }
    }

    /// Unwraps the `& -1` I64-forcing wrapper `lowered_i64` adds around the
    /// real expression, returning the wrapped expression's own root
    /// `Value`. Needed here (unlike other passes' tests) because this test
    /// inspects the chain's actual SHAPE via `dependency_depth`, and the
    /// wrapper's own `Inst::And` node -- not an `Add`/`Mul` -- would
    /// otherwise measure as depth 0, masking the real chain underneath it.
    fn unwrap_forced_i64(f: &Function, wrapped: Value) -> Value {
        match f.insts[wrapped.0 as usize].clone() {
            Inst::And(root, _) => root,
            other => panic!("expected the `& -1` wrapper, found {other:?}"),
        }
    }

    #[test]
    fn reduces_dependency_depth_on_an_i64_sum_chain() {
        // NOTE: the bare identifiers here default to F64 under this
        // project's type inference (see `lowered_i64`'s doc comment) -- the
        // plan's literal source string does NOT type-check as an i64 chain
        // on its own, confirmed by running this test against plain
        // `lowered(...)` first and observing `assoc_i64_op` reject every
        // node (F64-typed). `lowered_i64` fixes that.
        let mut f = lowered_i64("((((((a + b) + c) + d) + e) + g) + h) + i");
        let Terminator::Return(wrapped) = f.blocks[f.entry.0 as usize].term.clone().unwrap() else {
            panic!()
        };
        let before = dependency_depth(&f, unwrap_forced_i64(&f, wrapped));
        assert_eq!(
            before, 7,
            "sanity: a fully left-leaning 8-term chain has depth 7"
        );
        Reassociate.run(&mut f);
        let Terminator::Return(wrapped2) = f.blocks[f.entry.0 as usize].term.clone().unwrap()
        else {
            panic!()
        };
        let after = dependency_depth(&f, unwrap_forced_i64(&f, wrapped2));
        assert!(
            after < before,
            "reassociation should reduce dependency depth (got {after}, was {before})"
        );
    }

    #[test]
    fn does_not_reassociate_f64_chains() {
        // Floating-point addition is not associative under rounding --
        // reassociating without --fast-math would be a real correctness
        // bug (a different, if very close, answer). Confirm this pass
        // leaves an f64 chain's shape untouched.
        let mut f = lowered("((((a + b) + c) + d) + e)");
        let before_insts = f.insts.len();
        let changed = Reassociate.run(&mut f);
        assert!(
            !changed,
            "f64 chains must not be reassociated without --fast-math"
        );
        assert_eq!(f.insts.len(), before_insts);
    }

    #[test]
    fn reassociation_never_changes_the_i64_answer() {
        // Bare `n` needs forcing to I64 -- see `lowered_i64`'s doc comment
        // -- otherwise this would panic in `interpret` feeding an
        // `RtValue::I64` to an F64-typed parameter.
        let src = "((((((n + n) + n) + n) + n) + n) + n) + n";
        let unreassoc = lowered_i64(src);
        let expected = interpret(&unreassoc, &[RtValue::I64(7)]);
        let mut reassoc = lowered_i64(src);
        Reassociate.run(&mut reassoc);
        let actual = interpret(&reassoc, &[RtValue::I64(7)]);
        assert_eq!(expected, actual);
    }

    #[test]
    fn multi_use_intermediate_value_is_preserved_not_discarded() {
        // `t` is used TWICE: once folded into the Add chain (`t + c`), and
        // once again directly as the OTHER operand of the outer multiply --
        // a genuine "used elsewhere, not just as the next link" case. A
        // naive flatten that blindly discarded `t`'s own instruction while
        // flattening through it would leave the multiply's second operand
        // dangling (or silently wrong). `t`'s value comes from a literal
        // `3 + 4` (not identifiers) specifically to sidestep a DIFFERENT
        // gotcha: `typeck`'s `Expr::Let` arm infers the bound value with
        // `expected = None`, NOT the surrounding context's expected type
        // (see `typeck.rs`), so `let t = a + b in ...` would leave `a`/`b`
        // defaulted to F64 regardless of any outer `& -1`. A literal
        // `3 + 4` types as I64 on its own, sidestepping that entirely.
        let src = "(let t = 3 + 4 in (((t + c) + d) + e) * t) & -1";
        let unreassoc = lowered(src);
        let args = &[RtValue::I64(2), RtValue::I64(3), RtValue::I64(5)]; // c, d, e
        let expected = interpret(&unreassoc, args);

        let mut f = lowered(src);
        let changed = Reassociate.run(&mut f);
        assert!(
            changed,
            "sanity: the 4-leaf add-chain (t,c,d,e) should actually get rebalanced"
        );
        assert!(forge_ir::verify::verify(&f).is_ok());

        let actual = interpret(&f, args);
        assert_eq!(
            expected, actual,
            "t's second, non-chain use (the outer multiply) must still see the right value"
        );
    }

    #[test]
    fn verifies_after_rewrite() {
        let mut f = lowered_i64("((((((a + b) + c) + d) + e) + g) + h) + i");
        Reassociate.run(&mut f);
        assert!(forge_ir::verify::verify(&f).is_ok());
    }

    #[test]
    fn does_not_fire_on_a_short_i64_chain() {
        // Only 3 leaves -- already as shallow as any regrouping could make
        // it (depth 2 either way), so nothing should change.
        let mut f = lowered_i64("(a + b) + c");
        let changed = Reassociate.run(&mut f);
        assert!(!changed);
    }

    #[test]
    fn stale_use_counts_across_dce_do_not_permanently_cap_rebalancing_depth() {
        // Regression test (found by code review) for a real bug:
        // `analyze_uses` used to scan the raw `f.insts` backing array
        // instead of each block's LIVE `insts` list. Since DCE (`dce.rs`)
        // deliberately leaves a swept instruction's `Inst` sitting in
        // `f.insts` forever (only removing it from `block.insts`), a
        // since-dead multi-use reference kept "counting" as a real use
        // FOREVER, even in pipeline rounds run long after DCE had already
        // removed it from live code -- permanently capping how far a chain
        // could be rebalanced, not just missing one round's worth of
        // optimization.
        //
        // `x` is defined as a 4-leaf chain and used TWICE: once by `dead`
        // (never read by the returned expression, so DCE removes it) and
        // once as the first leaf of a further, otherwise-unrelated 3-leaf
        // chain. With the bug, `x` stays permanently walled off as an
        // opaque leaf of the outer chain (its use count never drops back to
        // 1), capping the achievable depth at 4 (2 for the outer 4-leaf
        // split `[x, q1, q2, q3]` + 2 for x's own independently-rebalanced
        // 4-leaf subtree) even after `dead` is long gone -- confirmed
        // empirically by temporarily reverting the fix and re-running this
        // exact case, not just derived on paper. Fixed, `reassoc`
        // eventually (a later round, once DCE has caught up) sees `x` drop
        // to a single use and fully merges its 4 leaves into the outer
        // chain for one flat 7-leaf balanced tree, depth 3.
        //
        // Each leaf is forced to I64 individually via `& -1` (rather than
        // wrapping the whole expression once, as `lowered_i64` does) so the
        // function's return value IS the chain root directly -- no wrapper
        // node to unwrap before measuring `dependency_depth`.
        let src = "let x = (((p1 & -1) + (p2 & -1)) + (p3 & -1)) + (p4 & -1) in \
                   let dead = x + 99 in \
                   (((x + (q1 & -1)) + (q2 & -1)) + (q3 & -1))";
        let mut f = lowered(src);

        // Run the FULL pipeline (multiple rounds), not `Reassociate.run()`
        // standalone -- the bug only manifests ACROSS rounds, once DCE has
        // had a chance to remove `dead` between two `Reassociate`
        // invocations.
        crate::optimize(&mut f);

        let Terminator::Return(root) = f.blocks[f.entry.0 as usize].term.clone().unwrap() else {
            panic!()
        };
        let depth = dependency_depth(&f, root);
        assert_eq!(
            depth, 3,
            "chain should converge to the fully-balanced depth (3) for its 7 total leaves \
             (p1..p4, q1..q3) once `dead` is swept and `x` is no longer wrongly seen as \
             multi-use; got {depth} -- a regression to 4 means `analyze_uses` is counting \
             stale/dead uses again"
        );
        assert!(forge_ir::verify::verify(&f).is_ok());
    }
}
