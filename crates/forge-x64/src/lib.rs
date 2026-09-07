mod assembler;
mod libm;
mod machine_inst;
mod prologue;
mod reg;

pub use assembler::{AluOp, Assembler, ConditionCode, Label, RoundMode, ShiftOp, SseOp};
pub use libm::libm_address;
pub use machine_inst::{
    find_fusable_diamonds, select, ConstantPool, DiamondFusion, MachineInst, MinMaxOp, PoolIndex,
    SelectedFunction,
};
pub use prologue::{emit_epilogue, emit_prologue, SYSV_CALLEE_SAVED};
pub use reg::PhysReg;

/// The callee-saved register set for the active x86-64 ABI. Windows adds
/// nonvolatile XMM6-XMM15 to its GPR set; the prologue emitter stores those
/// registers in the frame instead of treating them like pushable GPRs.
#[cfg(not(windows))]
pub const CALLEE_SAVED: &[PhysReg] = SYSV_CALLEE_SAVED;

#[cfg(windows)]
pub const CALLEE_SAVED: &[PhysReg] = &[
    PhysReg::Rbx,
    PhysReg::Rsi,
    PhysReg::Rdi,
    PhysReg::R12,
    PhysReg::R13,
    PhysReg::R14,
    PhysReg::R15,
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
