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
}
