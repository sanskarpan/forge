mod interval;
mod intervals;
mod linear_scan;
mod liveness;
mod pressure;
mod verify;

pub use interval::{Interval, RegClass, SYSV_FLOAT_ARGS, SYSV_INT_ARGS};
pub use intervals::{build_intervals, excluded_registers};
pub use linear_scan::{
    allocate, Location, ALLOCATABLE_GPR, ALLOCATABLE_XMM, SCRATCH_GPR, SCRATCH_XMM,
    SPILL_AWARE_ALLOCATABLE_GPR, SPILL_AWARE_ALLOCATABLE_XMM,
};
pub use liveness::{compute_liveness, Liveness};
pub use pressure::{register_pressure, PressurePoint};
pub use verify::verify_allocation;
