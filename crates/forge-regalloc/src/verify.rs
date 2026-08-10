use crate::interval::Interval;
use crate::linear_scan::Location;
use forge_ir::Value;
use std::collections::HashMap;

/// An INDEPENDENT verifier (PROMPT.md), deliberately written without
/// reference to `LinearScan`'s internals so it cannot share a bug with the
/// allocator it checks. Re-derives the overlap check from the raw
/// `Interval`/assignment data via a plain pairwise scan -- the same shape
/// PROMPT.md's own sketch uses, corrected for two defects that sketch has
/// (see this file's design doc): the INCLUSIVE range convention, and the
/// legitimate-handoff exemption that makes "no two overlapping intervals
/// share a register" false of the real allocator's own correct output.
///
/// Also checks spill slots: `Location::Spill(n)` values sharing the same
/// slot number must be genuinely, STRICTLY disjoint -- unlike registers,
/// a stack slot has no same-instruction handoff mechanism, so no
/// exemption applies on this side. Not reachable from the current
/// `spill()` (its strict `slot_end[s] < start` reuse condition already
/// guarantees same-slot pairs are disjoint) but kept anyway: an
/// independent verifier must not lean on the allocator's own invariant,
/// and this is defense-in-depth against other future slot-assigning code
/// (`evict_and_assign`'s still-deferred victim path, a future coalescer).
///
/// Deliberately does NOT check for a `Value` missing from `assignment`
/// entirely, or a `RegClass`/register-class mismatch -- both are already
/// checked by `linear_scan.rs`'s own tests, and bullet 17's literal scope
/// is the overlap property specifically. See this file's design doc for
/// the full reasoning.
///
/// **`Ok(())` here means "no two overlapping intervals share a location",
/// NOT "this allocation is correct."** The gap is currently real, not
/// theoretical: nothing in this allocator models CALL CLOBBERS yet
/// (`excluded_registers` covers only `IntDiv`/`IntRem`'s Rax/Rdx), and
/// every XMM register is caller-saved under System V, so a float value
/// living across a `CallLibm` is destroyed by it. Measured on
/// `sin(x) + cos(y)`: `Value(1)` is live across the `CallLibm` at
/// position 2 and is assigned `Xmm1`, which that call destroys -- and
/// this function returns `Ok(())` on it, correctly by its own contract.
/// That is CHECKLIST bullet 22's job (Phase 8e) to make the allocator
/// handle and to test; recorded here so nobody reads a green
/// `verify_allocation` as a soundness proof it does not claim to be.
pub fn verify_allocation(
    intervals: &[Interval],
    assignment: &HashMap<Value, Location>,
) -> Result<(), String> {
    for (i, a) in intervals.iter().enumerate() {
        for b in &intervals[i + 1..] {
            let overlaps = a.start <= b.end && b.start <= a.end;
            if !overlaps {
                continue;
            }
            match (assignment.get(&a.value), assignment.get(&b.value)) {
                (Some(Location::Reg(ra)), Some(Location::Reg(rb))) if ra == rb => {
                    let legit_handoff = (a.end == b.start && b.hint == Some(a.value))
                        || (b.end == a.start && a.hint == Some(b.value));
                    if !legit_handoff {
                        return Err(format!(
                            "overlapping values {:?} [{},{}] and {:?} [{},{}] both assigned \
                             {:?}, and neither end touches the other's start as a hinted \
                             handoff",
                            a.value, a.start, a.end, b.value, b.start, b.end, ra
                        ));
                    }
                }
                (Some(Location::Spill(sa)), Some(Location::Spill(sb))) if sa == sb => {
                    return Err(format!(
                        "overlapping values {:?} [{},{}] and {:?} [{},{}] both assigned spill \
                         slot {sa} -- spill slots have no legitimate-handoff exemption",
                        a.value, a.start, a.end, b.value, b.start, b.end
                    ));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interval::RegClass;

    fn iv(value: u32, start: u32, end: u32, class: RegClass) -> Interval {
        Interval {
            value: Value(value),
            start,
            end,
            reg_class: class,
            hint: None,
            fixed: None,
            spill_weight: 0.0,
        }
    }

    #[test]
    fn accepts_a_valid_disjoint_allocation() {
        let a = iv(0, 0, 2, RegClass::Gpr);
        let b = iv(1, 3, 5, RegClass::Gpr);
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Reg(forge_x64::PhysReg::Rax));
        assignment.insert(b.value, Location::Reg(forge_x64::PhysReg::Rbx));

        assert!(verify_allocation(&[a, b], &assignment).is_ok());
    }

    #[test]
    fn rejects_overlapping_same_register_without_handoff() {
        let a = iv(0, 0, 5, RegClass::Gpr);
        let b = iv(1, 2, 8, RegClass::Gpr); // genuinely overlaps a, no hint relationship
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Reg(forge_x64::PhysReg::Rax));
        assignment.insert(b.value, Location::Reg(forge_x64::PhysReg::Rax));

        let err = verify_allocation(&[a, b], &assignment).unwrap_err();
        assert!(
            err.contains("Value(0)"),
            "error must name the first value: {err}"
        );
        assert!(
            err.contains("Value(1)"),
            "error must name the second value: {err}"
        );
    }

    #[test]
    fn accepts_a_legitimate_handoff() {
        let a = iv(0, 0, 2, RegClass::Gpr);
        let mut b = iv(1, 2, 4, RegClass::Gpr);
        b.hint = Some(a.value); // a.end == b.start, b hints at a -- a real two-address handoff
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Reg(forge_x64::PhysReg::Rax));
        assignment.insert(b.value, Location::Reg(forge_x64::PhysReg::Rax));

        assert!(verify_allocation(&[a, b], &assignment).is_ok());
    }

    #[test]
    fn accepts_the_symmetric_handoff_direction() {
        // The second disjunct (`b.end == a.start && a.hint == Some(b.value)`)
        // needs b's range to come BEFORE a's, not merely a flipped hint on
        // the same slice order as the test above -- confirmed by execution
        // during design review. Naively keeping a=[0,2],b=[2,4] and just
        // setting a.hint = Some(b.value) does NOT exercise this branch and
        // produces Err, not Ok (the design doc's Testing section explains
        // why). This test genuinely swaps which interval has the earlier
        // range instead.
        let mut a = iv(0, 2, 4, RegClass::Gpr); // now the LATER range
        let b = iv(1, 0, 2, RegClass::Gpr); // now the EARLIER range
        a.hint = Some(b.value); // b.end == a.start, a hints at b
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Reg(forge_x64::PhysReg::Rax));
        assignment.insert(b.value, Location::Reg(forge_x64::PhysReg::Rax));

        assert!(verify_allocation(&[a, b], &assignment).is_ok());
    }

    #[test]
    fn rejects_the_symmetric_touching_case_without_a_matching_hint() {
        // Mirrors accepts_the_symmetric_handoff_direction's geometry (b's
        // range earlier, a's range later, touching at one point) but with
        // no hint relationship -- must be rejected. Closes a real gap: an
        // asymmetric weakening of the overlap predicate's first `<=` to
        // `<` (verify_allocation's `a.start <= b.end && b.start <= a.end`)
        // would make this pair look non-overlapping instead of correctly
        // rejected, and nothing else in this file's tests would catch it.
        let a = iv(0, 2, 4, RegClass::Gpr); // later range
        let b = iv(1, 0, 2, RegClass::Gpr); // earlier range, no hint at all
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Reg(forge_x64::PhysReg::Rax));
        assignment.insert(b.value, Location::Reg(forge_x64::PhysReg::Rax));

        assert!(
            verify_allocation(&[a, b], &assignment).is_err(),
            "touching positions alone must not be enough in either direction -- the hint must \
             actually match"
        );
    }

    #[test]
    fn rejects_touching_intervals_sharing_a_register_without_a_matching_hint() {
        let a = iv(0, 0, 2, RegClass::Gpr);
        let b = iv(1, 2, 4, RegClass::Gpr); // touches a, but no hint at all
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Reg(forge_x64::PhysReg::Rax));
        assignment.insert(b.value, Location::Reg(forge_x64::PhysReg::Rax));

        assert!(
            verify_allocation(&[a, b], &assignment).is_err(),
            "touching positions alone must not be enough -- the hint must actually match"
        );
    }

    #[test]
    fn rejects_overlapping_spill_slots() {
        let a = iv(0, 0, 5, RegClass::Gpr);
        let b = iv(1, 2, 8, RegClass::Gpr);
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Spill(0));
        assignment.insert(b.value, Location::Spill(0));

        assert!(verify_allocation(&[a, b], &assignment).is_err());
    }

    #[test]
    fn rejects_touching_spill_slots_even_though_touching_registers_can_be_exempt() {
        // Spill slots have NO handoff exemption, unlike registers -- this
        // is the test that would fail if someone copy-pasted the register
        // exemption onto the spill branch.
        let a = iv(0, 0, 2, RegClass::Gpr);
        let mut b = iv(1, 2, 4, RegClass::Gpr);
        b.hint = Some(a.value); // even WITH a matching hint --
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Spill(0));
        assignment.insert(b.value, Location::Spill(0));

        assert!(
            verify_allocation(&[a, b], &assignment).is_err(),
            "spill slots must never be exempted, even when the hint would exempt a register"
        );
    }

    #[test]
    fn accepts_disjoint_spill_slots_sharing_a_slot_number() {
        let a = iv(0, 0, 2, RegClass::Gpr);
        let b = iv(1, 3, 5, RegClass::Gpr); // genuinely disjoint from a
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Spill(0));
        assignment.insert(b.value, Location::Spill(0));

        assert!(verify_allocation(&[a, b], &assignment).is_ok());
    }

    #[test]
    fn accepts_overlapping_values_in_different_spill_slots() {
        // Distinguishes the `sa == sb` check from "any two overlapping
        // Spill locations are an error regardless of slot number" -- the
        // disjoint-same-slot test above short-circuits on the `!overlaps`
        // guard before ever reaching the slot-number comparison, so it
        // can't tell these two behaviors apart on its own; this test can.
        let a = iv(0, 0, 5, RegClass::Gpr);
        let b = iv(1, 2, 8, RegClass::Gpr); // genuinely OVERLAPS a
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Spill(0));
        assignment.insert(b.value, Location::Spill(1)); // DIFFERENT slots -- fine

        assert!(verify_allocation(&[a, b], &assignment).is_ok());
    }

    #[test]
    fn accepts_every_real_corpus_allocation() {
        for src in test_corpus() {
            let func = front_end(src);
            let selected = forge_x64::select(&func);
            let intervals = crate::intervals::build_intervals(&func, &selected);
            let excluded = crate::intervals::excluded_registers(&func, &selected);
            let (assignment, _bytes) =
                crate::linear_scan::allocate(intervals.clone(), &excluded, &selected);

            assert!(
                verify_allocation(&intervals, &assignment).is_ok(),
                "{src:?}: the real, shipped allocator's own output must pass this independent \
                 check -- if it doesn't, either the allocator or this verifier has a bug"
            );
        }
    }

    #[test]
    fn catches_a_deliberately_broken_allocation() {
        let mut checked = 0;
        for src in test_corpus() {
            let func = front_end(src);
            let selected = forge_x64::select(&func);
            let intervals = crate::intervals::build_intervals(&func, &selected);
            let excluded = crate::intervals::excluded_registers(&func, &selected);
            let (mut assignment, _bytes) =
                crate::linear_scan::allocate(intervals.clone(), &excluded, &selected);

            // Find a genuinely-overlapping, non-handoff, same-class pair in
            // this program's REAL intervals (phi-merged ranges, real hints
            // included) -- not every corpus program has one.
            let mut broken = None;
            'outer: for i in 0..intervals.len() {
                for j in (i + 1)..intervals.len() {
                    let (a, b) = (&intervals[i], &intervals[j]);
                    if a.reg_class != b.reg_class {
                        continue;
                    }
                    let overlaps = a.start <= b.end && b.start <= a.end;
                    let legit_handoff = (a.end == b.start && b.hint == Some(a.value))
                        || (b.end == a.start && a.hint == Some(b.value));
                    if overlaps && !legit_handoff {
                        broken = Some((a.value, b.value));
                        break 'outer;
                    }
                }
            }
            let Some((va, vb)) = broken else { continue };
            checked += 1;

            // Force vb onto va's real location, creating a genuine conflict.
            let loc_a = assignment[&va];
            assignment.insert(vb, loc_a);

            assert!(
                verify_allocation(&intervals, &assignment).is_err(),
                "{src:?}: {va:?} and {vb:?} genuinely overlap without a legitimate handoff but \
                 were forced onto the same location -- verify_allocation must catch this"
            );
        }
        assert!(
            checked > 0,
            "corpus must contain at least one genuinely-overlapping non-handoff pair to \
             exercise this test -- if this fails, the corpus changed and needs a hand-built \
             fallback fixture instead"
        );
    }

    /// Shared corpus, copied VERBATIM from `crates/forge-regalloc/src/linear_scan.rs`'s
    /// `test_corpus()` -- this project's established convention (already used between
    /// `intervals.rs` and `linear_scan.rs`) is to duplicate this exact list into each new
    /// test module that needs it, rather than thread `pub(crate)` plumbing across files.
    fn test_corpus() -> Vec<&'static str> {
        vec![
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
            "if a > b then (a * c) + (b * c) else a - b",
            "if a > b then (a - b) - (a + b) else c - a",
            "if a > b then fma(a, b, c) else a * c",
            "if x > y then (x * y) + (x - y) else x / y",
            "if x > y then fma(x, y, z) * x else fma(y, x, z) - y",
        ]
    }

    /// Same front_end helper shape as linear_scan.rs's own test module (lex ->
    /// parse -> resolve -> typecheck -> lower).
    fn front_end(src: &str) -> forge_ir::Function {
        let (tokens, diags) = forge_syntax::lexer::lex(src);
        assert!(diags.is_empty(), "lex errors for {src:?}: {diags:?}");
        let (ast, diags) = forge_syntax::parser::parse(&tokens);
        assert!(diags.is_empty(), "parse errors for {src:?}: {diags:?}");
        let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
            .unwrap_or_else(|e| panic!("type errors for {src:?}: {e:?}"));
        forge_ir::lower::lower(&typed)
    }
}
