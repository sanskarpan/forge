use crate::interval::{Interval, RegClass};
use crate::liveness::{block_range_at, compute_liveness, def_of, reads_of};
use forge_ir::{Block, Function, Inst, Terminator, Value};
use forge_x64::SelectedFunction;
use std::collections::HashMap;

/// Builds one `Interval` per virtual register the selected function
/// actually needs a physical location for -- every real SSA `Value` that
/// is defined by some `MachineInst`, plus every `Inst::Phi` destination
/// (which emits no `MachineInst` at all, yet is genuinely read by its
/// users), plus every synthetic temp the selector minted (Fma's `mul_tmp`,
/// typed via `selected.synthetic_types`).
///
/// `start`/`end` come from real backward liveness dataflow, not an
/// approximation: `end` covers every position a value is live across, not
/// just its last textual use inside its own block. φ destinations and all
/// their incoming values are then merged into one shared range AND given
/// mutual hints toward the φ's own destination (see `merge_phi_intervals`),
/// and two-address hints are copied from
/// `SelectedFunction::coalescing_hints`. NOTHING populates `Interval::fixed`
/// -- see `populate_fixed_registers`' own doc comment and the design doc's
/// corrected "Fixed registers" section for why `Param`/`IntDiv`/`IntRem`
/// are emission-time copies rather than whole-lifetime register pins.
///
/// The returned `Vec` is sorted by `(start, end, value)`: construction is
/// `HashMap`-backed, whose iteration order is not stable across runs on
/// identical input, and an unsorted return would make register assignment
/// -- and therefore emitted machine code -- nondeterministic once 8b
/// consumes this.
pub fn build_intervals(func: &Function, selected: &SelectedFunction) -> Vec<Interval> {
    let liveness = compute_liveness(func, selected);

    let mut start: HashMap<Value, u32> = HashMap::new();
    let mut end: HashMap<Value, u32> = HashMap::new();

    for (pos, &(block, block_start)) in selected.block_starts.iter().enumerate() {
        let range = block_range_at(selected, pos);

        // A phi emits NO MachineInst (Phase 7a), so its destination would
        // never be "defined" anywhere in `insts` and would silently end up
        // with no interval at all -- even though its users (a Return, an
        // IntAdd, ...) genuinely need it in a register. Seed those defs
        // here, at the top of the block that owns the phi, which is exactly
        // where a phi is conceptually defined.
        for &v in &func.blocks[block.0 as usize].insts {
            if matches!(func.insts[v.0 as usize], Inst::Phi { .. }) {
                start.entry(v).or_insert(block_start as u32);
                end.entry(v).or_insert(block_start as u32);
            }
        }

        // Anything live OUT of this block stays live through the block's
        // own last instruction, even when its last textual use sits in a
        // different block entirely -- this is what makes a flat [start, end]
        // range meaningful across block boundaries. (`insts` is laid out in
        // RPO, which is a topological order for today's DAG-shaped CFGs, so
        // a successor's positions are always greater than this block's.)
        if range.end > range.start {
            let block_last = (range.end - 1) as u32;
            for &v in liveness.live_out(block) {
                end.entry(v)
                    .and_modify(|e| *e = (*e).max(block_last))
                    .or_insert(block_last);
            }
        }

        for (offset, inst) in selected.insts[range.clone()].iter().enumerate() {
            let p = (range.start + offset) as u32;
            for used in reads_of(inst) {
                end.entry(used)
                    .and_modify(|e| *e = (*e).max(p))
                    .or_insert(p);
            }
            if let Some(d) = def_of(inst) {
                start.entry(d).or_insert(p);
                end.entry(d).or_insert(p);
            }
        }
    }

    let mut intervals: HashMap<Value, Interval> = HashMap::new();
    for (&v, &s) in &start {
        let ty = if (v.0 as usize) < func.types.len() {
            func.types[v.0 as usize]
        } else {
            selected.synthetic_types[&v]
        };
        let e = end.get(&v).copied().unwrap_or(s);
        // Both of these are IR-shape invariants, not allocator choices: a
        // value read with no reaching definition, or one whose last use
        // precedes its own definition, means the caller handed us IR that
        // `forge_ir::verify` would have rejected (SSA def-dominates-use).
        // Fail loudly rather than hand 8b a nonsense range.
        assert!(
            e >= s,
            "interval for {v:?} ends at {e} but starts at {s} -- its last use precedes its \
             definition, which means the input IR violates SSA def-dominates-use"
        );
        intervals.insert(
            v,
            Interval {
                value: v,
                start: s,
                end: e,
                reg_class: RegClass::of(ty),
                hint: None,
                fixed: None,
                spill_weight: 0.0,
            },
        );
    }
    for v in end.keys() {
        assert!(
            start.contains_key(v),
            "{v:?} is live but is never defined by any MachineInst (and isn't a phi \
             destination) -- it would get no interval and therefore no register at all"
        );
    }

    merge_phi_intervals(func, &mut intervals);
    populate_two_address_hints(selected, &mut intervals);
    populate_fixed_registers(func);

    let mut result: Vec<Interval> = intervals.into_values().collect();
    result.sort_by_key(|iv| (iv.start, iv.end, iv.value.0));
    result
}

/// A block's real successors, straight from its terminator -- the ground
/// truth of the CFG's edge set. (`BlockData::preds` also exists, but it is
/// `Builder`'s own SSA-construction bookkeeping, populated by explicit
/// `add_pred` calls a caller can forget, leave stale, or never make at all
/// for a hand-built `Function`; the terminators cannot disagree with the
/// CFG the selector actually laid out.)
///
/// A degenerate `Branch { then_: X, else_: X }` (both arms targeting the
/// same block) counts as ONE successor, not two. The critical-edge logic
/// treats successor and predecessor COUNTS as meaningful, so counting that
/// branch twice would both inflate `X`'s predecessor count and make the
/// branching block look like it has two successors -- misfiring the
/// tripwire on an edge that is not actually critical. Today's front-end
/// cannot produce this shape, but Phase 7f's diamond fusion plausibly
/// could.
fn successors_of(func: &Function, block: Block) -> Vec<Block> {
    match &func.blocks[block.0 as usize].term {
        Some(Terminator::Jump(t)) => vec![*t],
        Some(Terminator::Branch { then_, else_, .. }) => {
            if then_ == else_ {
                vec![*then_]
            } else {
                vec![*then_, *else_]
            }
        }
        Some(Terminator::Return(_)) | None => vec![],
    }
}

/// `Value -> owning Block`, for every value still present in some block's
/// instruction list. A value that has been dropped from its block (e.g. a
/// dead phi DCE removed from `block.insts` but left in `func.insts`) is
/// deliberately absent, and every caller here treats "absent" as "not part
/// of this function's real CFG, impose no constraint from it".
fn block_of_each_value(func: &Function) -> HashMap<Value, Block> {
    let mut owner = HashMap::new();
    for (i, bd) in func.blocks.iter().enumerate() {
        for &v in &bd.insts {
            owner.insert(v, Block(i as u32));
        }
    }
    owner
}

/// Re-verifies the critical-edge-free invariant Phase 7a's φ-lowering
/// depends on, then unions every φ destination with all of its incoming
/// values into ONE shared `[min start, max end]` range -- which is what
/// makes "φ emits nothing; its dst and its incoming values end up sharing
/// one physical location" real at the interval level.
///
/// ## The critical-edge tripwire
///
/// An edge `pred -> phi_block` is CRITICAL iff `pred` has more than one
/// successor AND `phi_block` has more than one predecessor, counting ALL
/// edges into it, not just this one. Across a critical edge the shared-
/// interval strategy is unsound (two predecessors reaching the φ block
/// would carry different values for the same φ, yet be forced into one
/// location with nowhere to insert the resolving copy), so this is an
/// `assert!`, not a `debug_assert!` -- matching this project's "invariant
/// bugs must fail loudly in release too" precedent (Phase 6a's `bind()`,
/// Phase 7d's `Rbp`-in-`callee_saved` guard).
///
/// Both counts come from real terminators (see `successors_of`): the
/// successor count of `pred`, and the total number of edges targeting
/// `phi_block` from anywhere in the function. Every program today's
/// front-end can produce is an if/else DAG whose φ blocks are only ever
/// reached by single-successor `Jump`s, so this can never fire yet; it
/// exists as a tripwire for whenever the front-end grows a construct that
/// could introduce one.
///
/// The merge itself uses union-find (not independent per-φ unions): a φ
/// can feed another φ, or two φs can share an incoming value, and merging
/// each φ independently in `func.insts` order would give order-dependent,
/// possibly-inconsistent ranges. Union-find collapses each connected
/// component into one shared range regardless of listing order.
fn merge_phi_intervals(func: &Function, intervals: &mut HashMap<Value, Interval>) {
    let owner = block_of_each_value(func);

    let mut pred_edge_counts = vec![0usize; func.blocks.len()];
    for i in 0..func.blocks.len() {
        for succ in successors_of(func, Block(i as u32)) {
            pred_edge_counts[succ.0 as usize] += 1;
        }
    }

    let mut parent: HashMap<Value, Value> = HashMap::new();
    let mut members: Vec<Value> = Vec::new();

    for (i, inst) in func.insts.iter().enumerate() {
        let Inst::Phi { incoming } = inst else {
            continue;
        };
        let phi_value = Value(i as u32);
        // A φ no longer present in any block imposes no constraint: it is
        // dead, contributes no MachineInst, and has no interval to merge.
        let Some(&phi_block) = owner.get(&phi_value) else {
            continue;
        };

        for &(pred_block, incoming_value) in incoming.iter() {
            let pred_successors = successors_of(func, pred_block).len();
            let phi_block_predecessors = pred_edge_counts[phi_block.0 as usize];
            assert!(
                pred_successors <= 1 || phi_block_predecessors <= 1,
                "critical edge {pred_block:?} -> {phi_block:?} feeding phi {phi_value:?} \
                 ({pred_block:?} has {pred_successors} successors, {phi_block:?} has \
                 {phi_block_predecessors} predecessors): Phase 7a's phi-lowering strategy \
                 (a phi and its incoming values sharing one interval) is unsound across a \
                 critical edge, and critical-edge splitting does not exist yet"
            );

            union(&mut parent, &mut members, phi_value, incoming_value);
        }
    }

    // One pass to collect each class's merged bounds, one to write them
    // back -- a member with no interval (a φ whose block never selected,
    // say) simply contributes nothing and receives nothing.
    let mut bounds: HashMap<Value, (u32, u32)> = HashMap::new();
    for &m in &members {
        let Some(iv) = intervals.get(&m) else {
            continue;
        };
        let root = find(&mut parent, m);
        let entry = bounds.entry(root).or_insert((iv.start, iv.end));
        entry.0 = entry.0.min(iv.start);
        entry.1 = entry.1.max(iv.end);
    }
    for &m in &members {
        let root = find(&mut parent, m);
        let Some(&(s, e)) = bounds.get(&root) else {
            continue;
        };
        if let Some(iv) = intervals.get_mut(&m) {
            iv.start = s;
            iv.end = e;
        }
    }

    // Beyond the range merge, every group member ALSO needs a mutual hint
    // toward one canonical anchor (the phi's own destination) -- N
    // intervals with an identical range and no hint look like N mutually
    // interfering values to 8b, not one coalescing group. The hint is
    // soft (per this project's established "not honoring a hint is not
    // an error" convention); the deferred final-emission task is
    // responsible for inserting a real parallel copy for any group
    // member 8b/8c didn't manage to co-locate -- the fallback Phase 7a's
    // own design doc already anticipated ("insert parallel-copy moves at
    // predecessor block ends otherwise") but which doesn't exist yet.
    //
    // This is a genuinely separate pass over `func.insts` rather than a
    // fold into the bounds loop above: that loop iterates `members`, a
    // flat list of every value touched by ANY phi across the whole
    // function, so it cannot cleanly derive which phi is a given value's
    // anchor -- re-walking `Inst::Phi` directly can.
    for (i, inst) in func.insts.iter().enumerate() {
        let Inst::Phi { incoming } = inst else {
            continue;
        };
        let phi_value = Value(i as u32);
        if !intervals.contains_key(&phi_value) {
            continue;
        }
        for &(_, incoming_value) in incoming.iter() {
            if let Some(iv) = intervals.get_mut(&incoming_value) {
                if iv.hint.is_none() {
                    iv.hint = Some(phi_value);
                }
            }
        }
    }
}

fn find(parent: &mut HashMap<Value, Value>, mut v: Value) -> Value {
    loop {
        let p = parent.get(&v).copied().unwrap_or(v);
        if p == v {
            return v;
        }
        let grandparent = parent.get(&p).copied().unwrap_or(p);
        parent.insert(v, grandparent); // path halving
        v = grandparent;
    }
}

fn union(parent: &mut HashMap<Value, Value>, members: &mut Vec<Value>, a: Value, b: Value) {
    for v in [a, b] {
        // `entry` rather than `contains_key` + `insert`: clippy::map_entry
        // is a default-on lint and rejects the two-lookup form under
        // `-D warnings`.
        if let std::collections::hash_map::Entry::Vacant(slot) = parent.entry(v) {
            slot.insert(v);
            members.push(v);
        }
    }
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        parent.insert(rb, ra);
    }
}

/// Two-address hints: for each `dst -> preferred_same_as` entry in
/// `SelectedFunction::coalescing_hints` (fully computed already, Phase 7b),
/// record `dst`'s interval hint as pointing at `preferred_same_as`. This is
/// a direct copy from an existing map, not new computation -- 8b resolves
/// the hinted `Value` to a real register via its own scan-time assignment
/// map.
fn populate_two_address_hints(
    selected: &SelectedFunction,
    intervals: &mut HashMap<Value, Interval>,
) {
    for (&dst, &preferred) in &selected.coalescing_hints {
        if let Some(iv) = intervals.get_mut(&dst) {
            iv.hint = Some(preferred);
        }
    }
}

/// Validates `func.params` fits within SysV's ABI argument-register counts.
/// Does NOT populate `Interval::fixed` for Param/IntDiv/IntRem's dst --
/// see the design doc's corrected "Fixed registers" section: none of
/// these are genuinely whole-lifetime register requirements (the ABI/
/// hardware register only matters for a single instant -- the Param's
/// own position, or the IntDiv/IntRem's own position), so representing
/// them as a whole-`Interval` pin produces unsatisfiable constraint sets
/// whenever two such values have overlapping lifetimes (e.g. `a/b + c/d`,
/// or a 3rd int param used alongside an `%` in the same function --
/// both confirmed to actually collide by an earlier version of this
/// function). Both are instead resolved as pure emission-time copies
/// (mirroring the established two-address-hint and dividend-into-rax
/// fixups): the deferred final-emission task recomputes the ABI/hardware
/// register directly from the MachineInst itself (no Interval data
/// needed) and inserts a copy into the value's real assigned location if
/// they don't already coincide.
fn populate_fixed_registers(func: &Function) {
    let mut gpr_seen = 0usize;
    let mut xmm_seen = 0usize;
    for &(_, ty) in &func.params {
        match RegClass::of(ty) {
            RegClass::Gpr => {
                assert!(
                    gpr_seen < crate::interval::SYSV_INT_ARGS.len(),
                    "function has more than {} integer/bool parameters -- exceeds SysV's \
                     integer argument register count; this needs to become a real Diagnostic \
                     before any user-facing CLI surface ships (tracked in the Phase 8a design doc)",
                    crate::interval::SYSV_INT_ARGS.len()
                );
                gpr_seen += 1;
            }
            RegClass::Xmm => {
                assert!(
                    xmm_seen < crate::interval::SYSV_FLOAT_ARGS.len(),
                    "function has more than {} float parameters -- exceeds SysV's float \
                     argument register count; this needs to become a real Diagnostic before \
                     any user-facing CLI surface ships (tracked in the Phase 8a design doc)",
                    crate::interval::SYSV_FLOAT_ARGS.len()
                );
                xmm_seen += 1;
            }
        }
    }
    // IntDiv/IntRem's dst deliberately gets NO fixed marking either -- see
    // this function's own doc comment above.
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_ir::builder::Builder;
    use forge_ir::{Function, Inst, Terminator, Ty};
    use forge_syntax::span::Span;
    use forge_x64::select;

    fn dummy_span() -> Span {
        Span::new(0, 0)
    }

    /// The real front-end pipeline, per `crates/forge-ir/tests/e2e.rs`:
    /// `lex` returns `(tokens, diags)`, `parse` takes TOKENS (not a &str)
    /// and returns `(ast, diags)`, `typecheck` takes an OWNED resolved
    /// `Ast` and returns `Result<TypedAst, Vec<Diagnostic>>`, and `lower`
    /// lives in the `forge_ir::lower` MODULE (`forge_ir::lower::lower`).
    fn front_end(src: &str) -> Function {
        let (tokens, diags) = forge_syntax::lexer::lex(src);
        assert!(diags.is_empty(), "lex errors for {src:?}: {diags:?}");
        let (ast, diags) = forge_syntax::parser::parse(&tokens);
        assert!(diags.is_empty(), "parse errors for {src:?}: {diags:?}");
        let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
            .unwrap_or_else(|e| panic!("type errors for {src:?}: {e:?}"));
        forge_ir::lower::lower(&typed)
    }

    #[test]
    fn straight_line_interval_starts_at_def_ends_at_last_use() {
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
        let intervals = build_intervals(&b.f, &selected);

        // Selected layout: 0 Param(x), 1 LoadImmI64(one), 2 IntAdd(y), 3 Return(y).
        let x_iv = intervals.iter().find(|iv| iv.value == x).unwrap();
        let one_iv = intervals.iter().find(|iv| iv.value == one).unwrap();
        let y_iv = intervals.iter().find(|iv| iv.value == y).unwrap();
        assert_eq!((x_iv.start, x_iv.end), (0, 2));
        assert_eq!((one_iv.start, one_iv.end), (1, 2));
        // y is defined at the IntAdd and dies at the Return that reads it.
        assert_eq!((y_iv.start, y_iv.end), (2, 3));
        assert_eq!(x_iv.reg_class, RegClass::Gpr);
        assert_eq!(intervals.len(), 3);
    }

    #[test]
    fn value_live_across_a_branch_gets_an_interval_extending_into_the_successor() {
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
        // Each block returns ITS OWN result: returning `one` (defined in
        // then_block) from else_block would violate SSA def-dominates-use
        // and is not legal IR.
        b.f.blocks[else_block.0 as usize].term = Some(Terminator::Return(else_result));

        let selected = select(&b.f);
        let intervals = build_intervals(&b.f, &selected);

        // RPO is entry, else_block, then_block (dominance::reverse_postorder
        // DFSes then_ first, then reverses), so the layout is:
        //   0 LoadImmI64(shared)  1 LoadImmI64(cond)  2 Branch
        //   3 LoadImmI64(two)     4 IntAdd(else_result) 5 Return
        //   6 LoadImmI64(one)     7 IntAdd(then_result) 8 Return
        assert_eq!(
            selected.block_starts,
            vec![(entry, 0), (else_block, 3), (then_block, 6)]
        );
        let shared_iv = intervals.iter().find(|iv| iv.value == shared).unwrap();
        // shared is used in BOTH successors; its interval must run from its
        // def all the way to its LAST use anywhere, well past entry's own
        // block boundary -- that is the whole point of running real
        // liveness instead of a per-block approximation.
        let entry_block_end = selected.block_starts[1].1;
        assert_eq!((shared_iv.start, shared_iv.end), (0, 7));
        assert!(shared_iv.end as usize > entry_block_end);
        // cond, by contrast, dies at the Branch inside entry.
        let cond_iv = intervals.iter().find(|iv| iv.value == cond).unwrap();
        assert_eq!((cond_iv.start, cond_iv.end), (1, 2));
    }

    #[test]
    fn phi_interval_merges_with_all_incoming_values() {
        let mut b = Builder::new();
        let entry = b.create_block();
        let then_block = b.create_block();
        let else_block = b.create_block();
        let join = b.create_block();
        b.add_pred(then_block, entry);
        b.add_pred(else_block, entry);
        b.add_pred(join, then_block);
        b.add_pred(join, else_block);
        b.seal_block(entry);

        let cond = b.emit(entry, Inst::ConstBool(true), Ty::Bool, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
            cond,
            then_: then_block,
            else_: else_block,
        });

        b.seal_block(then_block);
        let then_val = b.emit(then_block, Inst::ConstI64(1), Ty::I64, dummy_span());
        b.f.blocks[then_block.0 as usize].term = Some(Terminator::Jump(join));

        b.seal_block(else_block);
        let else_val = b.emit(else_block, Inst::ConstI64(2), Ty::I64, dummy_span());
        b.f.blocks[else_block.0 as usize].term = Some(Terminator::Jump(join));

        b.seal_block(join);
        let phi = b.emit(
            join,
            Inst::Phi {
                incoming: smallvec::smallvec![(then_block, then_val), (else_block, else_val)],
            },
            Ty::I64,
            dummy_span(),
        );
        b.f.blocks[join.0 as usize].term = Some(Terminator::Return(phi));

        let selected = select(&b.f);
        let intervals = build_intervals(&b.f, &selected);

        // Layout (RPO entry, else_block, then_block, join):
        //   0 LoadImmI64(cond)  1 Branch
        //   2 LoadImmI64(else_val)  3 Jump
        //   4 LoadImmI64(then_val)  5 Jump
        //   6 Return(phi)        <- the Phi itself emits NOTHING
        let then_iv = intervals.iter().find(|iv| iv.value == then_val).unwrap();
        let else_iv = intervals.iter().find(|iv| iv.value == else_val).unwrap();
        // The phi destination must HAVE an interval even though no
        // MachineInst defines it -- the Return genuinely reads it.
        let phi_iv = intervals
            .iter()
            .find(|iv| iv.value == phi)
            .expect("phi destination must get an interval of its own");
        // All three collapse to ONE range: earliest incoming def (else_val
        // at 2) through the phi's last use (the Return at 6).
        assert_eq!((then_iv.start, then_iv.end), (2, 6));
        assert_eq!((else_iv.start, else_iv.end), (2, 6));
        assert_eq!((phi_iv.start, phi_iv.end), (2, 6));

        // Beyond the range merge, both incoming values must ALSO hint toward
        // the phi's own destination -- the merge alone isn't enough signal
        // for an allocator to treat these as one coalescing group rather
        // than N mutually interfering values with identical ranges.
        assert_eq!(then_iv.hint, Some(phi));
        assert_eq!(else_iv.hint, Some(phi));
    }

    #[test]
    fn critical_edge_tripwire_never_fires_on_realistic_if_else_programs() {
        // Real front-end output for every if/else shape this project can
        // currently produce -- confirms build_intervals' critical-edge
        // assertion never fires on anything actually reachable today.
        for src in [
            "if x > 0.0 then x else 0.0 - x",
            "if a > b then a + b else a - b",
            "let t = a - b in if t > 0.0 then t else -t",
            "if a > b then (if a > c then a else c) else b",
            "(if a > b then a else b) + a",
            "if a > b then (if b > c then b else c) else (if a > c then a else c)",
            "let m = (if a > b then a else b) in m * m + sqrt(m)",
        ] {
            let func = front_end(src);
            let selected = select(&func);
            let _ = build_intervals(&func, &selected); // must not panic
        }
    }

    #[test]
    #[should_panic(expected = "critical edge")]
    fn critical_edge_tripwire_fires_on_a_hand_built_critical_edge() {
        // entry --Branch--> {a_block, join} and a_block --Jump--> join:
        // the entry->join edge is CRITICAL (entry has 2 successors, join has
        // 2 predecessors). No construct the front-end can produce today has
        // this shape -- it has to be built by hand -- but the tripwire must
        // genuinely detect it, not merely appear to.
        let mut b = Builder::new();
        let entry = b.create_block();
        let a_block = b.create_block();
        let join = b.create_block();
        b.add_pred(a_block, entry);
        b.add_pred(join, entry);
        b.add_pred(join, a_block);
        b.seal_block(entry);

        let c0 = b.emit(entry, Inst::ConstI64(1), Ty::I64, dummy_span());
        let cond = b.emit(entry, Inst::ConstBool(true), Ty::Bool, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
            cond,
            then_: a_block,
            else_: join,
        });

        b.seal_block(a_block);
        let c1 = b.emit(a_block, Inst::ConstI64(2), Ty::I64, dummy_span());
        b.f.blocks[a_block.0 as usize].term = Some(Terminator::Jump(join));

        b.seal_block(join);
        let phi = b.emit(
            join,
            Inst::Phi {
                incoming: smallvec::smallvec![(entry, c0), (a_block, c1)],
            },
            Ty::I64,
            dummy_span(),
        );
        b.f.blocks[join.0 as usize].term = Some(Terminator::Return(phi));

        let selected = select(&b.f);
        let _ = build_intervals(&b.f, &selected); // must panic
    }

    #[test]
    fn build_intervals_holds_its_invariants_across_the_whole_language_corpus() {
        // Every feature the front-end supports, incl. the shapes that make
        // selection non-1:1 with the IR: fma (2 MachineInsts + a synthetic
        // temp), libm calls, lea fusion (suppressed Mul/Shl), and phis
        // (0 MachineInsts).
        for src in [
            "3.14159 * r * r",
            "sin(x) + cos(y)",
            "(n * 2654435761) >> 16",
            "x / y",
            "x + 1",
            "fma(a, b, c)",
            "base + i * 8",
            "let t = a - b in if t > 0.0 then t else -t",
            "if a > b then (if a > c then a else c) else b",
            "(if a > b then a else b) + a",
            "sqrt(x * x + y * y)",
            "abs(x) + floor(y) + ceil(z)",
            "(n >> 1) % 7 + (n >> 1) / 7",
        ] {
            let func = front_end(src);
            let selected = select(&func);
            let intervals = build_intervals(&func, &selected);

            for iv in &intervals {
                assert!(
                    iv.start <= iv.end,
                    "{src:?}: interval {iv:?} has end before start"
                );
                assert!(
                    (iv.end as usize) < selected.insts.len(),
                    "{src:?}: interval {iv:?} runs past the end of insts"
                );
            }
            // Every Value any MachineInst reads or writes must have exactly
            // one interval -- a missing one means 8b would allocate no
            // register for a register the code genuinely needs.
            let covered: std::collections::HashSet<forge_ir::Value> =
                intervals.iter().map(|iv| iv.value).collect();
            assert_eq!(
                covered.len(),
                intervals.len(),
                "{src:?}: duplicate intervals"
            );
            for inst in &selected.insts {
                for v in reads_of(inst).into_iter().chain(def_of(inst)) {
                    assert!(covered.contains(&v), "{src:?}: {v:?} has no interval");
                }
            }
        }
    }

    #[test]
    fn two_address_op_dst_gets_hint_pointing_at_lhs() {
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
        let intervals = build_intervals(&b.f, &selected);

        let y_iv = intervals.iter().find(|iv| iv.value == y).unwrap();
        assert_eq!(y_iv.hint, Some(x));
        // An op with no two-address constraint gets no hint at all.
        let x_iv = intervals.iter().find(|iv| iv.value == x).unwrap();
        let one_iv = intervals.iter().find(|iv| iv.value == one).unwrap();
        assert_eq!(x_iv.hint, None);
        assert_eq!(one_iv.hint, None);
    }

    #[test]
    fn params_never_get_a_fixed_register_pin_from_build_intervals() {
        // Corrected behavior: Param's ABI register is no longer represented
        // as a whole-lifetime Interval::fixed pin (an earlier version of this
        // rule made overlapping params/idiv results unsatisfiable-by-
        // construction -- see the design doc's corrected "Fixed registers"
        // section). The ABI register is recomputed independently by the
        // deferred final-emission task directly from func.params + index,
        // with zero Interval involvement.
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(
            entry,
            Inst::Param {
                index: 0,
                ty: Ty::F64,
            },
            Ty::F64,
            dummy_span(),
        );
        let n = b.emit(
            entry,
            Inst::Param {
                index: 1,
                ty: Ty::I64,
            },
            Ty::I64,
            dummy_span(),
        );
        let y = b.emit(
            entry,
            Inst::Param {
                index: 2,
                ty: Ty::F64,
            },
            Ty::F64,
            dummy_span(),
        );
        b.f.params = vec![
            ("x".to_string(), Ty::F64),
            ("n".to_string(), Ty::I64),
            ("y".to_string(), Ty::F64),
        ];
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(n));

        let selected = select(&b.f);
        let intervals = build_intervals(&b.f, &selected);

        let x_iv = intervals.iter().find(|iv| iv.value == x).unwrap();
        let n_iv = intervals.iter().find(|iv| iv.value == n).unwrap();
        let y_iv = intervals.iter().find(|iv| iv.value == y).unwrap();
        assert_eq!(x_iv.fixed, None);
        assert_eq!(n_iv.fixed, None);
        assert_eq!(y_iv.fixed, None);
        assert_eq!(x_iv.reg_class, RegClass::Xmm);
        assert_eq!(n_iv.reg_class, RegClass::Gpr);
    }

    #[test]
    #[should_panic(expected = "more than 6 integer/bool parameters")]
    fn seventh_int_param_panics() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let mut last = None;
        let mut params = Vec::new();
        for i in 0..7u32 {
            let v = b.emit(
                entry,
                Inst::Param {
                    index: i,
                    ty: Ty::I64,
                },
                Ty::I64,
                dummy_span(),
            );
            params.push((format!("p{i}"), Ty::I64));
            last = Some(v);
        }
        b.f.params = params;
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(last.unwrap()));

        let selected = select(&b.f);
        let _ = build_intervals(&b.f, &selected); // must panic
    }

    #[test]
    fn int_div_and_int_rem_dst_get_no_fixed_register_pin() {
        // Corrected behavior: dst no longer gets fixed = Some(Rax)/Some(Rdx)
        // (an earlier version made two overlapping idiv results, e.g.
        // `a/b + c/d`, unsatisfiable-by-construction). The Rax/Rdx placement
        // is now a pure emission-time copy, symmetric with the dividend-side
        // fixup this file already didn't touch at the Interval level.
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let a = b.emit(entry, Inst::ConstI64(100), Ty::I64, dummy_span());
        let c = b.emit(entry, Inst::ConstI64(3), Ty::I64, dummy_span());
        let q = b.emit(entry, Inst::Div(a, c), Ty::I64, dummy_span());
        let r = b.emit(entry, Inst::Rem(a, c), Ty::I64, dummy_span());
        let sum = b.emit(entry, Inst::Add(q, r), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(sum));

        let selected = select(&b.f);
        let intervals = build_intervals(&b.f, &selected);

        let q_iv = intervals.iter().find(|iv| iv.value == q).unwrap();
        let r_iv = intervals.iter().find(|iv| iv.value == r).unwrap();
        let a_iv = intervals.iter().find(|iv| iv.value == a).unwrap();
        assert_eq!(q_iv.fixed, None);
        assert_eq!(r_iv.fixed, None);
        // lhs (a) gets NO special treatment at the Interval level -- neither a
        // fixed register nor a hint -- confirming the design's "pure
        // emission-time fixup, no allocator-level hint" resolution.
        assert_eq!(a_iv.fixed, None);
        assert_eq!(a_iv.hint, None);
        // ...and IntDiv/IntRem contribute no coalescing hint for their dst
        // either (compute_coalescing_hints deliberately excludes them).
        assert_eq!(q_iv.hint, None);
        assert_eq!(r_iv.hint, None);
    }

    #[test]
    fn two_overlapping_int_divs_no_longer_produce_conflicting_fixed_registers() {
        // The exact shape that exposed the original bug: two independent
        // divisions whose results are both needed by a later Add. Under the
        // old (incorrect) design both dsts got fixed = Some(Rax) for their
        // whole, overlapping ranges -- unsatisfiable. Now neither gets fixed
        // at all, so there's no conflict for 8b to even encounter.
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let a = b.emit(
            entry,
            Inst::Param {
                index: 0,
                ty: Ty::I64,
            },
            Ty::I64,
            dummy_span(),
        );
        let bb = b.emit(
            entry,
            Inst::Param {
                index: 1,
                ty: Ty::I64,
            },
            Ty::I64,
            dummy_span(),
        );
        let c = b.emit(
            entry,
            Inst::Param {
                index: 2,
                ty: Ty::I64,
            },
            Ty::I64,
            dummy_span(),
        );
        let d = b.emit(
            entry,
            Inst::Param {
                index: 3,
                ty: Ty::I64,
            },
            Ty::I64,
            dummy_span(),
        );
        b.f.params = vec![
            ("a".to_string(), Ty::I64),
            ("b".to_string(), Ty::I64),
            ("c".to_string(), Ty::I64),
            ("d".to_string(), Ty::I64),
        ];
        let q1 = b.emit(entry, Inst::Div(a, bb), Ty::I64, dummy_span());
        let q2 = b.emit(entry, Inst::Div(c, d), Ty::I64, dummy_span());
        let sum = b.emit(entry, Inst::Add(q1, q2), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(sum));

        let selected = select(&b.f);
        let intervals = build_intervals(&b.f, &selected);

        let q1_iv = intervals.iter().find(|iv| iv.value == q1).unwrap();
        let q2_iv = intervals.iter().find(|iv| iv.value == q2).unwrap();
        // Both dsts overlap (both survive to the final Add) -- confirm
        // neither carries a fixed register that would conflict.
        assert_eq!(q1_iv.fixed, None);
        assert_eq!(q2_iv.fixed, None);
    }

    #[test]
    fn build_intervals_returns_a_deterministically_sorted_vec() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let a = b.emit(
            entry,
            Inst::Param {
                index: 0,
                ty: Ty::I64,
            },
            Ty::I64,
            dummy_span(),
        );
        let one = b.emit(entry, Inst::ConstI64(1), Ty::I64, dummy_span());
        let two = b.emit(entry, Inst::ConstI64(2), Ty::I64, dummy_span());
        let x = b.emit(entry, Inst::Add(a, one), Ty::I64, dummy_span());
        let y = b.emit(entry, Inst::Add(x, two), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(y));

        let selected = select(&b.f);
        let intervals = build_intervals(&b.f, &selected);

        let sorted: Vec<_> = {
            let mut v = intervals.clone();
            v.sort_by_key(|iv| (iv.start, iv.end, iv.value.0));
            v
        };
        assert_eq!(
            intervals, sorted,
            "build_intervals' output must already be in sorted order"
        );
    }
}
