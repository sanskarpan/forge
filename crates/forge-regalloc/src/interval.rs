use forge_ir::{Ty, Value};
use forge_x64::PhysReg;

/// Which physical register file a value belongs in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RegClass {
    Gpr,
    Xmm,
}

impl RegClass {
    /// I64 and Bool both live in general-purpose registers (a Bool is a
    /// 0/1 GPR value per LoadImmI64's ConstBool handling, from Phase 7a);
    /// only F64 lives in XMM.
    pub fn of(ty: Ty) -> RegClass {
        match ty {
            Ty::I64 | Ty::Bool => RegClass::Gpr,
            Ty::F64 => RegClass::Xmm,
        }
    }
}

/// System V AMD64 integer/pointer argument registers, in order.
pub const SYSV_INT_ARGS: &[PhysReg] = &[
    PhysReg::Rdi,
    PhysReg::Rsi,
    PhysReg::Rdx,
    PhysReg::Rcx,
    PhysReg::R8,
    PhysReg::R9,
];

/// System V AMD64 float argument registers, in order.
pub const SYSV_FLOAT_ARGS: &[PhysReg] = &[
    PhysReg::Xmm0,
    PhysReg::Xmm1,
    PhysReg::Xmm2,
    PhysReg::Xmm3,
    PhysReg::Xmm4,
    PhysReg::Xmm5,
    PhysReg::Xmm6,
    PhysReg::Xmm7,
];

/// A virtual register's live range: `[start, end]` INCLUSIVE positions
/// into `SelectedFunction::insts` (the Vec index IS the linear
/// instruction number -- no separate numbering pass needed). `end` is the
/// value's last read position, and the value is live AT that position --
/// two intervals `[0,2]` and `[2,4]` DO overlap. `hint` points at another
/// Value this interval should try to share a physical location with, NOT
/// a bare PhysReg -- at construction time no value has been assigned a
/// real register yet (that's Phase 8b's job), so only a Value-to-Value
/// hint is meaningful here; 8b resolves it via its own scan-time
/// assignment map. This is a deliberate divergence from SPEC.md's
/// `Option<PhysReg>` sketch -- see the design doc's Hints section.
#[derive(Clone, Debug, PartialEq)]
pub struct Interval {
    pub value: Value,
    pub start: u32,
    pub end: u32,
    pub reg_class: RegClass,
    pub hint: Option<Value>,
    /// A whole-lifetime pin: the value must occupy exactly this register
    /// for its ENTIRE `[start, end]` range. As of Phase 8a NO rule
    /// populates this -- it is always `None`, and that is deliberate, not
    /// an oversight. Every "fixed register" requirement currently known
    /// (a `Param`'s incoming ABI register, `IntDiv`'s rax, `IntRem`'s rdx)
    /// turned out to be a POINT constraint holding for exactly one
    /// instruction, and expressing a point constraint as a whole-lifetime
    /// pin produces unsatisfiable constraint sets on trivial programs
    /// (`a/b + c/d` pins two overlapping values to rax forever). Those are
    /// all resolved as emission-time copies instead -- see the design
    /// doc's corrected "Fixed registers" section. The field stays because
    /// a genuinely whole-lifetime hardware constraint may exist in some
    /// future `MachineInst` variant.
    pub fixed: Option<PhysReg>,
    /// Always 0.0 as `build_intervals` (Phase 8a) constructs it. Really
    /// populated by Phase 8c's `populate_spill_weights` (`uses / length`),
    /// which `allocate` runs over the whole interval list once, up front,
    /// before partitioning by register class.
    pub spill_weight: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reg_class_of_maps_i64_and_bool_to_gpr_f64_to_xmm() {
        assert_eq!(RegClass::of(Ty::I64), RegClass::Gpr);
        assert_eq!(RegClass::of(Ty::Bool), RegClass::Gpr);
        assert_eq!(RegClass::of(Ty::F64), RegClass::Xmm);
    }

    #[test]
    fn sysv_int_args_matches_spec() {
        assert_eq!(
            SYSV_INT_ARGS,
            &[
                PhysReg::Rdi,
                PhysReg::Rsi,
                PhysReg::Rdx,
                PhysReg::Rcx,
                PhysReg::R8,
                PhysReg::R9
            ]
        );
    }

    #[test]
    fn sysv_float_args_matches_spec() {
        assert_eq!(
            SYSV_FLOAT_ARGS,
            &[
                PhysReg::Xmm0,
                PhysReg::Xmm1,
                PhysReg::Xmm2,
                PhysReg::Xmm3,
                PhysReg::Xmm4,
                PhysReg::Xmm5,
                PhysReg::Xmm6,
                PhysReg::Xmm7
            ]
        );
    }
}
