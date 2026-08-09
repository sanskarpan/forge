// TEMPORARY (Tasks 2-4 only): until Task 5 adds `pub fn allocate`, which
// `lib.rs` re-exports, nothing outside this file's own `#[cfg(test)]`
// module reaches `LinearScan`, so a non-test build correctly reports every
// item here as dead code and `cargo clippy -D warnings` fails without this.
// DELETE this attribute in Task 5, Step 3, once `allocate` makes
// everything genuinely reachable.
#![allow(dead_code)]

use crate::interval::Interval;
use forge_ir::Value;
use forge_x64::PhysReg;
use std::collections::{HashMap, HashSet};

/// A virtual register's final storage location, once Phase 8 has assigned
/// one. SPEC.md's §7 pseudocode references `Location` but never defines
/// it -- defined here, since this is the first slice needing it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Location {
    Reg(PhysReg),
    /// Stack slot index. Phase 8c's concern entirely -- this slice never
    /// constructs this variant; it exists now only so `Location`'s shape
    /// is settled before 8c needs to extend the same enum.
    Spill(u32),
}

/// System V AMD64 GPRs available for allocation: all 16 minus `Rsp`
/// (stack pointer, never a virtual register's home) and `Rbp` (frame
/// pointer, same reasoning as `prologue::SYSV_CALLEE_SAVED` already
/// excluding it).
pub const ALLOCATABLE_GPR: &[PhysReg] = &[
    PhysReg::Rax,
    PhysReg::Rcx,
    PhysReg::Rdx,
    PhysReg::Rbx,
    PhysReg::Rsi,
    PhysReg::Rdi,
    PhysReg::R8,
    PhysReg::R9,
    PhysReg::R10,
    PhysReg::R11,
    PhysReg::R12,
    PhysReg::R13,
    PhysReg::R14,
    PhysReg::R15,
];

/// XMM registers available for allocation: Xmm0-15 only. Xmm16-31 need
/// EVEX to reach and nothing in this codebase can encode an
/// EVEX-prefixed instruction yet, so handing one out would produce
/// unencodable output.
pub const ALLOCATABLE_XMM: &[PhysReg] = &[
    PhysReg::Xmm0,
    PhysReg::Xmm1,
    PhysReg::Xmm2,
    PhysReg::Xmm3,
    PhysReg::Xmm4,
    PhysReg::Xmm5,
    PhysReg::Xmm6,
    PhysReg::Xmm7,
    PhysReg::Xmm8,
    PhysReg::Xmm9,
    PhysReg::Xmm10,
    PhysReg::Xmm11,
    PhysReg::Xmm12,
    PhysReg::Xmm13,
    PhysReg::Xmm14,
    PhysReg::Xmm15,
];

/// Excludes a `Value`'s specific registers at SPECIFIC instruction
/// positions (8a's `excluded_registers`, keyed per position for IntDiv/
/// IntRem's rhs), aggregated to whole-`Interval` scope: this allocator
/// has no interval splitting, so one register serves an interval's
/// entire `[start, end]`, meaning a register excluded at ANY position
/// within that range must be excluded for the WHOLE interval. Every
/// exclusion position is guaranteed by construction to lie inside its
/// value's interval, so a plain per-Value union is both necessary and
/// sufficient -- no reference to the intervals themselves is needed here.
fn precompute_excluded(
    excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>,
) -> HashMap<Value, HashSet<PhysReg>> {
    let mut out: HashMap<Value, HashSet<PhysReg>> = HashMap::new();
    for (&(_, value), regs) in excluded_registers {
        out.entry(value).or_default().extend(regs.iter().copied());
    }
    out
}

// `HashSet::new()` is not `const fn`, so this needs a lazily-initialized
// static, not a plain `static _: HashSet<_> = HashSet::new();` (a compile
// error).
static EMPTY_EXCLUSION_SET: std::sync::LazyLock<HashSet<PhysReg>> =
    std::sync::LazyLock::new(HashSet::new);

pub struct LinearScan<'a> {
    intervals: Vec<Interval>,
    active: Vec<usize>,
    free_regs: HashSet<PhysReg>,
    assignment: HashMap<Value, Location>,
    excluded: HashMap<Value, HashSet<PhysReg>>,
    allocatable: &'a [PhysReg],
}

impl<'a> LinearScan<'a> {
    fn new(
        intervals: Vec<Interval>,
        excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>,
        allocatable: &'a [PhysReg],
    ) -> Self {
        LinearScan {
            intervals,
            active: Vec::new(),
            free_regs: allocatable.iter().copied().collect(),
            assignment: HashMap::new(),
            excluded: precompute_excluded(excluded_registers),
            allocatable,
        }
    }

    fn assign(&mut self, i: usize, loc: Location) {
        self.assignment.insert(self.intervals[i].value, loc);
    }

    fn location_of(&self, i: usize) -> Option<Location> {
        self.assignment.get(&self.intervals[i].value).copied()
    }

    /// Returns an EMPTY set (not a missing-key panic) for any `Value`
    /// with no exclusion entry -- the overwhelming common case.
    fn excluded_at(&self, value: Value) -> &HashSet<PhysReg> {
        self.excluded.get(&value).unwrap_or(&EMPTY_EXCLUSION_SET)
    }

    /// An active interval `j` expires (frees its register) once the new
    /// interval's `start` has moved PAST `j`'s `end` -- under 8a's
    /// INCLUSIVE `[start, end]` convention, `j.end == current_start`
    /// means the two intervals touch at exactly one shared position,
    /// which IS an overlap, so `j` must stay active (its register must
    /// NOT be freed yet). This is the inclusive-range-correct boundary --
    /// PROMPT.md's original sketch (`end > current_start`) assumes
    /// half-open ranges and would free `j` one position too early.
    fn expire_old_intervals(&mut self, current_start: u32) {
        while let Some(&j) = self.active.first() {
            if self.intervals[j].end >= current_start {
                break;
            }
            self.active.remove(0);
            // Only the Reg variant ever occupies a slot in free_regs --
            // Spill never does (this slice never produces it anyway).
            if let Some(Location::Reg(r)) = self.location_of(j) {
                self.free_regs.insert(r);
            }
        }
    }

    /// Picks a register for interval `i`, honoring its hint where safe.
    /// Case 1: the hint target's register is already free (it expired
    /// normally). Case 2: the hint target is STILL active but its
    /// interval ends exactly where this one starts -- the legitimate
    /// same-instruction-reuse case (x86's own 2-address destructive
    /// instructions read-then-overwrite one register atomically). When
    /// Case 2 fires, ownership transfers directly: the hint target is
    /// removed from `active` WITHOUT ever touching `free_regs` -- the
    /// register never becomes "free" in the general sense, it goes
    /// straight from one owner to the next. Falls back to any free,
    /// non-excluded register (in `allocatable`'s declared order, for
    /// deterministic output) if neither case applies.
    fn pick_register(&mut self, i: usize, allocatable: &[PhysReg]) -> Option<PhysReg> {
        let iv = self.intervals[i].clone();
        let excluded = self.excluded_at(iv.value);

        if let Some(hinted_value) = iv.hint {
            if let Some(Location::Reg(reg)) = self.assignment.get(&hinted_value) {
                if self.free_regs.contains(reg) && !excluded.contains(reg) {
                    return Some(*reg);
                }
            }
            if let Some(pos) = self
                .active
                .iter()
                .position(|&j| self.intervals[j].value == hinted_value)
            {
                let target_end = self.intervals[self.active[pos]].end;
                if target_end == iv.start {
                    if let Some(Location::Reg(reg)) = self.assignment.get(&hinted_value).copied() {
                        if !excluded.contains(&reg) {
                            self.active.remove(pos);
                            return Some(reg);
                        }
                    }
                }
            }
        }

        allocatable
            .iter()
            .find(|r| self.free_regs.contains(r) && !excluded.contains(r))
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phys_reg_hash_derive_works() {
        // Confirms the derive actually compiles and behaves correctly --
        // cheap, but real, since a missing/broken derive would otherwise
        // only surface as a confusing downstream compile error far from
        // its cause.
        let set: std::collections::HashSet<PhysReg> =
            [PhysReg::Rax, PhysReg::Rax].into_iter().collect();
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn allocatable_gpr_excludes_rsp_and_rbp() {
        assert_eq!(ALLOCATABLE_GPR.len(), 14);
        assert!(!ALLOCATABLE_GPR.contains(&PhysReg::Rsp));
        assert!(!ALLOCATABLE_GPR.contains(&PhysReg::Rbp));
    }

    #[test]
    fn allocatable_xmm_excludes_xmm16_through_31() {
        assert_eq!(ALLOCATABLE_XMM.len(), 16);
        for r in ALLOCATABLE_XMM {
            assert!(
                r.encoding() < 16,
                "{r:?} has encoding >= 16, unencodable without EVEX"
            );
        }
    }

    #[test]
    fn location_reg_and_spill_are_distinct_and_comparable() {
        assert_ne!(Location::Reg(PhysReg::Rax), Location::Spill(0));
        assert_eq!(Location::Reg(PhysReg::Rax), Location::Reg(PhysReg::Rax));
    }

    fn iv(
        value: u32,
        start: u32,
        end: u32,
        class: crate::interval::RegClass,
    ) -> crate::interval::Interval {
        crate::interval::Interval {
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
    fn precompute_excluded_unions_per_value_across_positions() {
        let mut raw: HashMap<(usize, Value), Vec<PhysReg>> = HashMap::new();
        raw.insert((2, Value(1)), vec![PhysReg::Rax, PhysReg::Rdx]);
        raw.insert((5, Value(1)), vec![PhysReg::Rdx]); // same Value, different position -- must union
        raw.insert((3, Value(2)), vec![PhysReg::Rcx]);

        let excluded = precompute_excluded(&raw);

        let v1: HashSet<PhysReg> = excluded[&Value(1)].clone();
        assert_eq!(v1, [PhysReg::Rax, PhysReg::Rdx].into_iter().collect());
        assert_eq!(excluded[&Value(2)], [PhysReg::Rcx].into_iter().collect());
    }

    #[test]
    fn excluded_at_returns_empty_set_for_unlisted_value() {
        let scan = LinearScan::new(vec![], &HashMap::new(), ALLOCATABLE_GPR);
        assert!(scan.excluded_at(Value(999)).is_empty());
    }

    #[test]
    fn expire_old_intervals_keeps_touching_intervals_active() {
        // [0,2] and [2,4] TOUCH at position 2 -- under 8a's inclusive
        // convention this IS an overlap, so [0,2]'s register must NOT be
        // freed when processing the interval starting at 2.
        let a = iv(0, 0, 2, crate::interval::RegClass::Gpr);
        let b = iv(1, 2, 4, crate::interval::RegClass::Gpr);
        let mut scan = LinearScan::new(vec![a.clone(), b], &HashMap::new(), ALLOCATABLE_GPR);
        scan.assign(0, Location::Reg(PhysReg::Rax));
        scan.active.push(0);
        scan.free_regs.remove(&PhysReg::Rax); // a HOLDS Rax -- it is not free

        scan.expire_old_intervals(2); // processing b, which starts at 2

        assert_eq!(
            scan.active,
            vec![0],
            "a must still be active -- it touches at position 2"
        );
        assert!(!scan.free_regs.contains(&PhysReg::Rax));
    }

    #[test]
    fn expire_old_intervals_frees_genuinely_disjoint_intervals() {
        let a = iv(0, 0, 2, crate::interval::RegClass::Gpr);
        let b = iv(1, 3, 4, crate::interval::RegClass::Gpr); // starts at 3, strictly after a.end=2
        let mut scan = LinearScan::new(vec![a.clone(), b], &HashMap::new(), ALLOCATABLE_GPR);
        scan.assign(0, Location::Reg(PhysReg::Rax));
        scan.active.push(0);
        scan.free_regs.remove(&PhysReg::Rax); // a HOLDS Rax -- it is not free

        scan.expire_old_intervals(3);

        assert!(scan.active.is_empty());
        assert!(scan.free_regs.contains(&PhysReg::Rax));
    }

    #[test]
    fn pick_register_case2_transfers_ownership_on_same_instruction_reuse() {
        // lhs.end == dst.start == 2, dst.hint == Some(lhs.value) -- the
        // structural signature of a legitimate two-address handoff.
        let lhs = iv(0, 0, 2, crate::interval::RegClass::Gpr);
        let mut dst = iv(1, 2, 4, crate::interval::RegClass::Gpr);
        dst.hint = Some(Value(0));
        let mut scan = LinearScan::new(vec![lhs.clone(), dst], &HashMap::new(), ALLOCATABLE_GPR);
        scan.assign(0, Location::Reg(PhysReg::Rax));
        scan.active.push(0);
        // lhs's register is NOT in free_regs (still "active") -- Case 2 must
        // transfer it directly, not require it to be free first.
        scan.free_regs.remove(&PhysReg::Rax);

        let picked = scan.pick_register(1, ALLOCATABLE_GPR);

        assert_eq!(picked, Some(PhysReg::Rax));
        assert!(
            scan.active.is_empty(),
            "lhs must be removed from active by the transfer"
        );
        assert!(
            !scan.free_regs.contains(&PhysReg::Rax),
            "the register must NEVER appear in free_regs during a Case 2 transfer"
        );
    }

    #[test]
    fn pick_register_case1_honors_a_hint_whose_target_already_expired() {
        // A hand-built fixture for the structurally-dead-against-real-data
        // Case 1 path: the hint target's interval has ALREADY expired, so
        // its register is genuinely back in `free_regs` and the target is
        // NOT in `active`. `build_intervals` provably cannot produce this
        // shape (a hint target is always either a two-address operand read
        // at the hinting interval's own start, or a phi anchor whose range
        // is identical -- both still active), so this MUST be hand-built
        // rather than corpus-derived.
        let mut dst = iv(1, 5, 7, crate::interval::RegClass::Gpr);
        dst.hint = Some(Value(0));
        let mut scan = LinearScan::new(vec![dst], &HashMap::new(), ALLOCATABLE_GPR);
        // Value(0) held Rcx and has since expired: `LinearScan::new` seeds
        // `free_regs` with the whole pool, so Rcx is already free, and
        // Value(0) never enters `active`.
        scan.assignment
            .insert(Value(0), Location::Reg(PhysReg::Rcx));
        assert!(scan.free_regs.contains(&PhysReg::Rcx));
        assert!(scan.active.is_empty());

        let picked = scan.pick_register(0, ALLOCATABLE_GPR);

        // Rcx, and specifically NOT ALLOCATABLE_GPR[0] (Rax) -- proving
        // Case 1 fired rather than the plain free-register fallback.
        assert_eq!(picked, Some(PhysReg::Rcx));
        assert_ne!(picked, Some(ALLOCATABLE_GPR[0]));
    }

    #[test]
    fn pick_register_falls_back_to_free_register_when_hint_unusable() {
        // Hint target's interval extends PAST this interval's start -- not a
        // legitimate handoff (shouldn't happen per 8a's own invariants, but
        // confirm the fallback path is taken safely, not a panic/wrong reg).
        let target = iv(0, 0, 10, crate::interval::RegClass::Gpr);
        let mut dst = iv(1, 2, 4, crate::interval::RegClass::Gpr);
        dst.hint = Some(Value(0));
        let mut scan = LinearScan::new(vec![target.clone(), dst], &HashMap::new(), ALLOCATABLE_GPR);
        scan.assign(0, Location::Reg(PhysReg::Rax));
        scan.active.push(0);
        scan.free_regs.remove(&PhysReg::Rax); // target HOLDS Rax -- it is not free

        let picked = scan.pick_register(1, ALLOCATABLE_GPR);

        // Rax is NOT returned (target.end=10 != dst.start=2, no transfer);
        // falls back to the first free register in ALLOCATABLE_GPR's order.
        assert_ne!(picked, Some(PhysReg::Rax));
        assert_eq!(picked, Some(ALLOCATABLE_GPR[1])); // Rax is index 0 and still occupied
    }

    #[test]
    fn pick_register_respects_exclusions_even_for_a_legitimate_handoff() {
        let lhs = iv(0, 0, 2, crate::interval::RegClass::Gpr);
        let mut dst = iv(1, 2, 4, crate::interval::RegClass::Gpr);
        dst.hint = Some(Value(0));
        let mut raw: HashMap<(usize, Value), Vec<PhysReg>> = HashMap::new();
        raw.insert((2, Value(1)), vec![PhysReg::Rax]); // dst itself excluded from Rax
        let mut scan = LinearScan::new(vec![lhs, dst], &raw, ALLOCATABLE_GPR);
        scan.assign(0, Location::Reg(PhysReg::Rax));
        scan.active.push(0);
        scan.free_regs.remove(&PhysReg::Rax);

        let picked = scan.pick_register(1, ALLOCATABLE_GPR);

        assert_ne!(
            picked,
            Some(PhysReg::Rax),
            "excluded even though it's the hint target's register"
        );
    }

    #[test]
    fn pick_register_case2_when_active_position_differs_from_interval_index() {
        // DISCRIMINATION FIXTURE: `pos` (from `active.iter().position(...)`)
        // indexes `active`, NOT `intervals` directly. Every OTHER Case 2
        // fixture in this file has `active == vec![0]`, where the two
        // coincide by coincidence -- so a bug conflating `intervals[pos]`
        // with `intervals[active[pos]]`, or `active.remove(pos)` with
        // `active.remove(active[pos])`, would pass every other test in this
        // file. Here `pos == 0` but `active[pos] == 1`, so the conflation
        // is genuinely caught: `other` (index 0) is long-lived and holds
        // Rbx, `target` (index 1) is the real hint target and holds Rcx.
        let other = iv(0, 0, 100, crate::interval::RegClass::Gpr);
        let target = iv(1, 1, 3, crate::interval::RegClass::Gpr);
        let mut dst = iv(2, 3, 5, crate::interval::RegClass::Gpr);
        dst.hint = Some(Value(1));
        let mut scan = LinearScan::new(vec![other, target, dst], &HashMap::new(), ALLOCATABLE_GPR);
        scan.assign(0, Location::Reg(PhysReg::Rbx));
        scan.assign(1, Location::Reg(PhysReg::Rcx));
        scan.free_regs.remove(&PhysReg::Rbx);
        scan.free_regs.remove(&PhysReg::Rcx);
        // `active` is sorted by `end`: target (end=3) before other (end=100).
        scan.active = vec![1, 0];

        let picked = scan.pick_register(2, ALLOCATABLE_GPR);

        assert_eq!(
            picked,
            Some(PhysReg::Rcx),
            "must transfer the hint TARGET's register, not some other active interval's"
        );
        assert_eq!(
            scan.active,
            vec![0],
            "only the target leaves active, and active stays sorted by end"
        );
        assert!(!scan.free_regs.contains(&PhysReg::Rcx));
    }
}
