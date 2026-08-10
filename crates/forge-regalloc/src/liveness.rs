use forge_ir::{Block, Function, Value};
use forge_x64::{MachineInst, SelectedFunction};
use std::collections::{HashMap, HashSet};

/// Extracts the Value operands a MachineInst READS (not its dst, if any).
/// Exhaustive over every current MachineInst variant, no wildcard --
/// mirrors select_inst's own discipline: a newly-added MachineInst
/// variant must get a real arm here, or this fails to compile.
pub(crate) fn reads_of(inst: &MachineInst) -> Vec<Value> {
    match inst {
        MachineInst::LoadImmI64 { .. } | MachineInst::LoadImmF64 { .. } => vec![],
        MachineInst::IntAdd { lhs, rhs, .. }
        | MachineInst::IntSub { lhs, rhs, .. }
        | MachineInst::IntMul { lhs, rhs, .. }
        | MachineInst::IntDiv { lhs, rhs, .. }
        | MachineInst::IntRem { lhs, rhs, .. }
        | MachineInst::And { lhs, rhs, .. }
        | MachineInst::Or { lhs, rhs, .. }
        | MachineInst::Xor { lhs, rhs, .. }
        | MachineInst::Shl { lhs, rhs, .. }
        | MachineInst::Shr { lhs, rhs, .. }
        | MachineInst::Sar { lhs, rhs, .. }
        | MachineInst::FloatAdd { lhs, rhs, .. }
        | MachineInst::FloatSub { lhs, rhs, .. }
        | MachineInst::FloatMul { lhs, rhs, .. }
        | MachineInst::FloatDiv { lhs, rhs, .. }
        | MachineInst::FloatMin { lhs, rhs, .. }
        | MachineInst::FloatMax { lhs, rhs, .. } => vec![*lhs, *rhs],
        MachineInst::IntNeg { src, .. }
        | MachineInst::Not { src, .. }
        | MachineInst::FloatSqrt { src, .. }
        | MachineInst::FloatRound { src, .. }
        | MachineInst::FloatAbs { src, .. }
        | MachineInst::FloatNeg { src, .. }
        | MachineInst::IntToFloat { src, .. }
        | MachineInst::FloatToInt { src, .. } => vec![*src],
        MachineInst::Lea { base, index, .. } => vec![*base, *index],
        MachineInst::IntCmp { lhs, rhs, .. } | MachineInst::FloatCmp { lhs, rhs, .. } => {
            vec![*lhs, *rhs]
        }
        MachineInst::IntCmov {
            cond,
            then_val,
            else_val,
            ..
        } => {
            vec![*cond, *then_val, *else_val]
        }
        MachineInst::CallLibm { args, .. } => args.iter().copied().collect(),
        MachineInst::Jump { .. } => vec![],
        MachineInst::Branch { cond, .. } => vec![*cond],
        MachineInst::Return { value } => vec![*value],
        MachineInst::Param { .. } => vec![],
    }
}

/// Extracts the Value a MachineInst DEFINES, if any. Only the three
/// terminator variants (Jump/Branch/Return) define nothing -- everything
/// else, `Param` included, has a real `dst`. Same no-wildcard
/// exhaustiveness discipline as `reads_of` above.
pub(crate) fn def_of(inst: &MachineInst) -> Option<Value> {
    match inst {
        MachineInst::Jump { .. } | MachineInst::Branch { .. } | MachineInst::Return { .. } => None,
        MachineInst::LoadImmI64 { dst, .. }
        | MachineInst::LoadImmF64 { dst, .. }
        | MachineInst::IntAdd { dst, .. }
        | MachineInst::IntSub { dst, .. }
        | MachineInst::IntMul { dst, .. }
        | MachineInst::IntDiv { dst, .. }
        | MachineInst::IntRem { dst, .. }
        | MachineInst::IntNeg { dst, .. }
        | MachineInst::And { dst, .. }
        | MachineInst::Or { dst, .. }
        | MachineInst::Xor { dst, .. }
        | MachineInst::Not { dst, .. }
        | MachineInst::Shl { dst, .. }
        | MachineInst::Shr { dst, .. }
        | MachineInst::Sar { dst, .. }
        | MachineInst::Lea { dst, .. }
        | MachineInst::IntCmov { dst, .. }
        | MachineInst::FloatAdd { dst, .. }
        | MachineInst::FloatSub { dst, .. }
        | MachineInst::FloatMul { dst, .. }
        | MachineInst::FloatDiv { dst, .. }
        | MachineInst::FloatSqrt { dst, .. }
        | MachineInst::FloatMin { dst, .. }
        | MachineInst::FloatMax { dst, .. }
        | MachineInst::FloatRound { dst, .. }
        | MachineInst::FloatAbs { dst, .. }
        | MachineInst::FloatNeg { dst, .. }
        | MachineInst::IntCmp { dst, .. }
        | MachineInst::FloatCmp { dst, .. }
        | MachineInst::IntToFloat { dst, .. }
        | MachineInst::FloatToInt { dst, .. }
        | MachineInst::CallLibm { dst, .. }
        | MachineInst::Param { dst, .. } => Some(*dst),
    }
}

/// A block's instruction-index range within `SelectedFunction::insts`, by
/// its `block_starts` INDEX (every caller here already iterates
/// `block_starts`, so no by-`Block` lookup variant is needed).
///
/// Takes the NEXT ENTRY'S start as the end -- deliberately NOT "the first
/// entry with a larger start", which would mis-handle a block that
/// selects to zero MachineInsts (two adjacent entries sharing one start)
/// by handing that block its successor's instructions.
pub(crate) fn block_range_at(selected: &SelectedFunction, pos: usize) -> std::ops::Range<usize> {
    let start = selected.block_starts[pos].1;
    let end = selected
        .block_starts
        .get(pos + 1)
        .map(|(_, s)| *s)
        .unwrap_or(selected.insts.len());
    start..end
}

pub struct Liveness {
    live_in: HashMap<Block, HashSet<Value>>,
    live_out: HashMap<Block, HashSet<Value>>,
}

impl Liveness {
    pub fn live_in(&self, block: Block) -> &HashSet<Value> {
        &self.live_in[&block]
    }

    pub fn live_out(&self, block: Block) -> &HashSet<Value> {
        &self.live_out[&block]
    }
}

/// Standard backward per-block dataflow to a fixpoint:
///   live_out[B] = union of live_in[S] for each successor S of B
///   live_in[B]  = uses[B] union (live_out[B] minus defs[B])
/// `uses[B]` only counts a Value as used if it's read before any def of
/// the SAME value earlier in the same block (a value defined and used
/// within one block never needs to appear in that block's live_in).
///
/// CFG successors are derived from `func`'s own real `Terminator`, NOT by
/// re-scanning `selected.insts` for `Jump`/`Branch` -- diamond fusion
/// (Phase 7f) replaces a fused block's real Branch with a branchless
/// FloatMin/FloatMax/IntCmov in `selected.insts`, so that MachineInst
/// stream alone no longer reflects the pred -> merge edge for a fused
/// block. `func`'s IR terminator is never touched by fusion (fusion
/// operates entirely on `select()`'s MachineInst output -- see the design
/// doc), so it always has the real edge. For every non-fused block this
/// is behavior-preserving: `select_term` translates each `Terminator`
/// 1:1 into the matching `Jump`/`Branch` MachineInst, so the two sources
/// already agreed exactly before fusion existed.
pub fn compute_liveness(func: &Function, selected: &SelectedFunction) -> Liveness {
    let blocks: Vec<Block> = selected.block_starts.iter().map(|(b, _)| *b).collect();

    let mut uses: HashMap<Block, HashSet<Value>> = HashMap::new();
    let mut defs: HashMap<Block, HashSet<Value>> = HashMap::new();
    let mut successors: HashMap<Block, Vec<Block>> = HashMap::new();

    for (pos, &block) in blocks.iter().enumerate() {
        let range = block_range_at(selected, pos);
        let mut block_defs: HashSet<Value> = HashSet::new();
        let mut block_uses: HashSet<Value> = HashSet::new();
        for inst in &selected.insts[range] {
            for used in reads_of(inst) {
                if !block_defs.contains(&used) {
                    block_uses.insert(used);
                }
            }
            if let Some(d) = def_of(inst) {
                block_defs.insert(d);
            }
        }
        let succs = match &func.blocks[block.0 as usize].term {
            Some(forge_ir::Terminator::Jump(target)) => vec![*target],
            Some(forge_ir::Terminator::Branch { then_, else_, .. }) => vec![*then_, *else_],
            _ => Vec::new(),
        };
        uses.insert(block, block_uses);
        defs.insert(block, block_defs);
        successors.insert(block, succs);
    }

    let mut live_in: HashMap<Block, HashSet<Value>> =
        blocks.iter().map(|&b| (b, HashSet::new())).collect();
    let mut live_out: HashMap<Block, HashSet<Value>> =
        blocks.iter().map(|&b| (b, HashSet::new())).collect();

    let mut changed = true;
    while changed {
        changed = false;
        for &block in blocks.iter().rev() {
            let mut new_out = HashSet::new();
            for &succ in &successors[&block] {
                new_out.extend(live_in[&succ].iter().copied());
            }
            let mut new_in = uses[&block].clone();
            for v in new_out.difference(&defs[&block]) {
                new_in.insert(*v);
            }
            if new_in != live_in[&block] {
                live_in.insert(block, new_in);
                changed = true;
            }
            if new_out != live_out[&block] {
                live_out.insert(block, new_out);
                changed = true;
            }
        }
    }

    Liveness { live_in, live_out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_ir::builder::Builder;
    use forge_ir::{Inst, Terminator, Ty};
    use forge_syntax::span::Span;
    use forge_x64::select;

    fn dummy_span() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn straight_line_function_has_trivial_liveness() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(
            entry,
            Inst::Param {
                index: 0,
                ty: Ty::I64,
            },
            Ty::I64,
            dummy_span(),
        );
        let one = b.emit(entry, Inst::ConstI64(1), Ty::I64, dummy_span());
        let y = b.emit(entry, Inst::Add(x, one), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(y));

        let selected = select(&b.f);
        let liveness = compute_liveness(&b.f, &selected);

        assert_eq!(liveness.live_in(entry), &std::collections::HashSet::new());
        assert_eq!(liveness.live_out(entry), &std::collections::HashSet::new());
    }

    #[test]
    fn value_live_across_a_branch_appears_in_live_out_of_the_defining_block() {
        let mut b = Builder::new();
        let entry = b.create_block();
        let then_block = b.create_block();
        let else_block = b.create_block();
        b.add_pred(then_block, entry);
        b.add_pred(else_block, entry);
        b.seal_block(entry);

        let shared = b.emit(entry, Inst::ConstI64(7), Ty::I64, dummy_span());
        let cond = b.emit(entry, Inst::ConstBool(true), Ty::Bool, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
            cond,
            then_: then_block,
            else_: else_block,
        });

        b.seal_block(then_block);
        let one = b.emit(then_block, Inst::ConstI64(1), Ty::I64, dummy_span());
        let then_result = b.emit(then_block, Inst::Add(shared, one), Ty::I64, dummy_span());
        b.f.blocks[then_block.0 as usize].term = Some(Terminator::Return(then_result));

        b.seal_block(else_block);
        let two = b.emit(else_block, Inst::ConstI64(2), Ty::I64, dummy_span());
        let else_result = b.emit(else_block, Inst::Add(shared, two), Ty::I64, dummy_span());
        b.f.blocks[else_block.0 as usize].term = Some(Terminator::Return(else_result));

        let selected = select(&b.f);
        let liveness = compute_liveness(&b.f, &selected);

        assert!(liveness.live_out(entry).contains(&shared));
        assert!(liveness.live_in(then_block).contains(&shared));
        assert!(liveness.live_in(else_block).contains(&shared));
        // cond, by contrast, is used inside entry (the Branch) and never
        // escapes it.
        assert!(!liveness.live_out(entry).contains(&cond));
    }

    /// Reproduces a real bug: once select() actually fuses an eligible
    /// diamond (Phase 7f) into a branchless FloatMax/IntCmov, the fused
    /// block's real Branch terminator is GONE from `selected.insts` --
    /// only the real forge_ir `Terminator` (untouched by fusion) still
    /// records the pred -> merge edge. `compute_liveness` used to derive
    /// CFG successors purely by re-scanning `selected.insts` for
    /// `Jump`/`Branch`, so a fused block's successors came out empty,
    /// silently dropping that edge from the dataflow graph.
    ///
    /// `c` here is defined in `entry` (the diamond's own Branch block),
    /// is NOT an operand of the diamond at all (the diamond fuses `a`/`b`
    /// into a FloatMax), and is used only after the merge block via
    /// `c + max(a, b)`. Correct liveness must keep `c` live out of
    /// `entry` all the way to the merge block's use -- a value the buggy
    /// successor-scan drops entirely, since `entry`'s scanned
    /// `selected.insts` range ends in `FloatMax`, not `Branch`.
    #[test]
    fn value_live_across_a_fused_diamond_survives_in_pred_live_out() {
        let src = "c + (if a > b then a else b)";
        let (tokens, diags) = forge_syntax::lexer::lex(src);
        assert!(diags.is_empty(), "lex errors: {diags:?}");
        let (ast, diags) = forge_syntax::parser::parse(&tokens);
        assert!(diags.is_empty(), "parse errors: {diags:?}");
        let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
            .unwrap_or_else(|e| panic!("type errors: {e:?}"));
        let func = forge_ir::lower::lower(&typed);

        let selected = select(&func);
        // Confirm this string genuinely produces a fused diamond (no
        // MachineInst::Branch survives selection) -- otherwise this test
        // would pass vacuously without ever exercising the bug.
        assert!(
            !selected
                .insts
                .iter()
                .any(|i| matches!(i, MachineInst::Branch { .. })),
            "expected select() to fuse this diamond away entirely"
        );
        assert!(
            selected
                .insts
                .iter()
                .any(|i| matches!(i, MachineInst::FloatMax { .. })),
            "expected the diamond to fuse into a FloatMax"
        );

        // `c` is the pass-through: the FloatAdd combining it with the
        // fused max's dst is the only FloatAdd in this program.
        let pass_through = selected
            .insts
            .iter()
            .find_map(|i| match i {
                MachineInst::FloatAdd { lhs, .. } => Some(*lhs),
                _ => None,
            })
            .expect("expected a FloatAdd combining c with the fused max");

        let liveness = compute_liveness(&func, &selected);

        assert!(
            liveness.live_out(func.entry).contains(&pass_through),
            "c is defined before the diamond and used after the merge block -- \
             it must stay live out of the diamond's own Branch block, but the \
             pred -> merge edge was silently dropped"
        );
    }
}
