//! Runtime SIMD capability selection shared by native front ends.

use forge_ir::{Function, Inst, Terminator, Ty, Value};

/// The typed, lane-wise IR consumed by the packed evaluator. Values reuse the
/// scalar IR's SSA indices so the vector program can be inspected alongside
/// the scalar pipeline without a second value-numbering scheme.
#[derive(Clone, Debug, PartialEq)]
pub enum VectorInst {
    SplatF64(u64),
    Param {
        index: u32,
    },
    Move(Value),
    Add(Value, Value),
    Sub(Value, Value),
    Mul(Value, Value),
    Div(Value, Value),
    Neg(Value),
    Sqrt(Value),
    Abs(Value),
    Fma {
        a: Value,
        b: Value,
        c: Value,
    },
    VecLoad {
        base: Value,
        offset: i32,
        lanes: u8,
    },
    VecStore {
        base: Value,
        offset: i32,
        value: Value,
        lanes: u8,
    },
    VecReduce {
        op: ReduceOp,
        vec: Value,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReduceOp {
    Sum,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VectorFunction {
    pub lanes: u8,
    pub insts: Vec<(Value, VectorInst)>,
    pub result: Value,
}

/// Lowers the supported straight-line scalar subset into typed vector IR.
/// Control flow, calls, integer/boolean values, and memory operations are
/// rejected explicitly until their vector semantics and loop ABI exist.
pub fn lower_f64_vector(function: &Function, lanes: u8) -> Result<VectorFunction, String> {
    if !matches!(lanes, 2 | 4 | 8) {
        return Err(format!("unsupported f64 vector width: {lanes}"));
    }
    let Some(block) = (function.blocks.len() == 1).then(|| &function.blocks[0]) else {
        return Err("vector lowering requires one straight-line block".to_string());
    };
    let Some(Terminator::Return(result)) = block.term.as_ref() else {
        return Err("vector lowering requires a return terminator".to_string());
    };

    let mut insts = Vec::with_capacity(block.insts.len());
    for &value in &block.insts {
        let scalar = function
            .insts
            .get(value.0 as usize)
            .ok_or_else(|| format!("block references missing instruction {value:?}"))?;
        let vector = match scalar {
            Inst::ConstF64(bits) => VectorInst::SplatF64(*bits),
            Inst::ConstI64(number) => VectorInst::SplatF64((*number as f64).to_bits()),
            Inst::Param { index, ty: Ty::F64 } => VectorInst::Param { index: *index },
            Inst::Add(lhs, rhs) => VectorInst::Add(*lhs, *rhs),
            Inst::Sub(lhs, rhs) => VectorInst::Sub(*lhs, *rhs),
            Inst::Mul(lhs, rhs) => VectorInst::Mul(*lhs, *rhs),
            Inst::Div(lhs, rhs) => VectorInst::Div(*lhs, *rhs),
            Inst::Neg(operand) => VectorInst::Neg(*operand),
            Inst::Sqrt(operand) => VectorInst::Sqrt(*operand),
            Inst::Abs(operand) => VectorInst::Abs(*operand),
            Inst::Fma { a, b, c } => VectorInst::Fma {
                a: *a,
                b: *b,
                c: *c,
            },
            Inst::IToF(operand) => VectorInst::Move(*operand),
            Inst::Param { .. } => return Err("vector lowering requires f64 parameters".to_string()),
            _ => {
                return Err(format!(
                    "scalar instruction is not vectorizable: {scalar:?}"
                ))
            }
        };
        insts.push((value, vector));
    }
    Ok(VectorFunction {
        lanes,
        insts,
        result: *result,
    })
}

fn evaluate_scalar(source: &str, args: &[f64]) -> Result<f64, String> {
    let values = args
        .iter()
        .copied()
        .map(forge_ir::interp::RtValue::F64)
        .collect::<Vec<_>>();
    match forge_runtime::interpret_source(source, &values).map_err(|error| error.to_string())? {
        forge_ir::interp::RtValue::F64(value) => Ok(value),
        _ => Err("array evaluation expression did not return f64".to_string()),
    }
}

/// CPU capabilities sampled when a compilation starts. The JIT uses a
/// snapshot instead of compiling ISA assumptions into the portable frontend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuFeatures {
    pub sse2: bool,
    pub sse41: bool,
    pub avx: bool,
    pub avx2: bool,
    pub fma: bool,
    pub avx512f: bool,
    pub avx512dq: bool,
    pub bmi2: bool,
    pub neon: bool,
    pub sve: bool,
}

impl CpuFeatures {
    pub const fn scalar() -> Self {
        Self {
            sse2: false,
            sse41: false,
            avx: false,
            avx2: false,
            fma: false,
            avx512f: false,
            avx512dq: false,
            bmi2: false,
            neon: false,
            sve: false,
        }
    }

    pub fn detect() -> Self {
        detect_impl()
    }

    pub fn best_width(self, ty: Ty) -> u8 {
        match ty {
            Ty::F64 if self.avx512f => 8,
            Ty::F64 if self.avx2 => 4,
            Ty::F64 if self.sse2 || self.neon => 2,
            _ => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimdWidth {
    Scalar,
    F64x2,
    F64x4,
    F64x8,
}

impl SimdWidth {
    pub const fn lanes(self) -> usize {
        match self {
            Self::Scalar => 1,
            Self::F64x2 => 2,
            Self::F64x4 => 4,
            Self::F64x8 => 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArrayPlan {
    pub width: SimdWidth,
    pub elements: usize,
    pub full_chunks: usize,
    pub tail: usize,
}

impl ArrayPlan {
    pub fn for_len(elements: usize) -> Self {
        let width = best_width();
        let lanes = width.lanes();
        Self {
            width,
            elements,
            full_chunks: elements / lanes,
            tail: elements % lanes,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct ArrayResult {
    pub values: Vec<f64>,
    pub plan: ArrayPlan,
    /// True only when the typed vector IR and a host ISA implementation
    /// successfully evaluate the packed chunks, including an AVX-512 masked
    /// tail when one is present.
    pub used_packed_backend: bool,
}

#[derive(Debug, PartialEq)]
pub struct ReductionResult {
    pub value: f64,
    pub plan: ArrayPlan,
    pub used_packed_backend: bool,
}

fn prepare_array(source: &str, columns: &[&[f64]]) -> Result<(Function, ArrayPlan), String> {
    let function = forge_runtime::lower_source(source).map_err(|error| error.to_string())?;
    if function.params.len() != columns.len()
        || function.params.iter().any(|(_, ty)| *ty != Ty::F64)
        || function.types.last() != Some(&Ty::F64)
    {
        return Err(
            "array evaluation requires an all-f64 expression and one column per parameter"
                .to_string(),
        );
    }
    let elements = columns.first().map_or(0, |column| column.len());
    if columns.iter().any(|column| column.len() != elements) {
        return Err("array columns must have equal lengths".to_string());
    }
    Ok((function, ArrayPlan::for_len(elements)))
}

/// Evaluates a pure expression over one column per free f64 parameter. Full
/// chunks use the widest safe packed backend for the host when the lowered
/// function is a straight-line f64 expression. AVX-512 hosts also use a
/// k-masked packed tail; other widths use the scalar interpreter for a tail.
/// The scalar interpreter remains the correctness fallback for control flow,
/// libm calls, and operations whose hardware NaN/rounding behavior does not
/// exactly match the oracle.
pub fn evaluate_array(source: &str, columns: &[&[f64]]) -> Result<ArrayResult, String> {
    let (function, plan) = prepare_array(source, columns)?;
    let elements = plan.elements;
    if plan.width != SimdWidth::Scalar {
        let mut values = Vec::with_capacity(elements);
        let lanes = plan.width.lanes();
        let packed_chunks = (0..plan.full_chunks)
            .map(|chunk| try_evaluate_packed_chunk(&function, columns, chunk * lanes, plan.width))
            .collect::<Option<Vec<_>>>();
        if let Some(chunks) = packed_chunks {
            let mut used_packed = plan.full_chunks > 0;
            for chunk in chunks {
                values.extend(chunk);
            }
            if plan.tail > 0 {
                if let Some(tail) = try_evaluate_packed_tail(
                    &function,
                    columns,
                    plan.full_chunks * lanes,
                    plan.width,
                    plan.tail,
                ) {
                    values.extend(tail);
                    used_packed = true;
                } else {
                    for index in plan.full_chunks * lanes..elements {
                        let args = columns
                            .iter()
                            .map(|column| column[index])
                            .collect::<Vec<_>>();
                        values.push(evaluate_scalar(source, &args)?);
                    }
                }
            }
            if used_packed && values.len() == elements {
                return Ok(ArrayResult {
                    values,
                    plan,
                    used_packed_backend: true,
                });
            }
        }
    }

    let mut values = Vec::with_capacity(elements);
    for index in 0..elements {
        let args = columns
            .iter()
            .map(|column| column[index])
            .collect::<Vec<_>>();
        values.push(evaluate_scalar(source, &args)?);
    }
    Ok(ArrayResult {
        values,
        plan,
        used_packed_backend: false,
    })
}

/// Reduces the per-row results of a pure all-f64 expression in source order.
/// Packed chunks are used for the expression evaluation when available, then
/// their lanes are accumulated left-to-right so the reduction order is
/// deterministic. Control flow, libm calls, and unsupported operations use
/// the scalar interpreter for the complete reduction.
pub fn reduce_sum(source: &str, columns: &[&[f64]]) -> Result<ReductionResult, String> {
    let (function, plan) = prepare_array(source, columns)?;
    if plan.width != SimdWidth::Scalar {
        let lanes = plan.width.lanes();
        let packed_chunks = (0..plan.full_chunks)
            .map(|chunk| try_evaluate_packed_chunk(&function, columns, chunk * lanes, plan.width))
            .collect::<Option<Vec<_>>>();
        if let Some(chunks) = packed_chunks {
            let mut used_packed = plan.full_chunks > 0;
            let mut values = chunks.into_iter().flatten().collect::<Vec<_>>();
            if plan.tail > 0 {
                if let Some(tail) = try_evaluate_packed_tail(
                    &function,
                    columns,
                    plan.full_chunks * lanes,
                    plan.width,
                    plan.tail,
                ) {
                    values.extend(tail);
                    used_packed = true;
                } else {
                    for index in plan.full_chunks * lanes..plan.elements {
                        let args = columns
                            .iter()
                            .map(|column| column[index])
                            .collect::<Vec<_>>();
                        values.push(evaluate_scalar(source, &args)?);
                    }
                }
            }
            if used_packed && values.len() == plan.elements {
                let value = values.into_iter().sum::<f64>();
                return Ok(ReductionResult {
                    value,
                    plan,
                    used_packed_backend: true,
                });
            }
        }
    }

    let mut value = 0.0;
    for index in 0..plan.elements {
        let args = columns
            .iter()
            .map(|column| column[index])
            .collect::<Vec<_>>();
        value += evaluate_scalar(source, &args)?;
    }
    Ok(ReductionResult {
        value,
        plan,
        used_packed_backend: false,
    })
}

trait PackedOps {
    type Vector: Copy;
    const LANES: usize;
    const HAS_FMA: bool = false;

    unsafe fn splat(value: f64) -> Self::Vector;
    unsafe fn load(values: *const f64) -> Self::Vector;
    unsafe fn store(value: Self::Vector, values: *mut f64);
    unsafe fn load_masked(values: *const f64, active: usize) -> Result<Self::Vector, ()> {
        if active == Self::LANES {
            Ok(Self::load(values))
        } else {
            Err(())
        }
    }
    unsafe fn store_masked(value: Self::Vector, values: *mut f64, active: usize) -> Result<(), ()> {
        if active == Self::LANES {
            Self::store(value, values);
            Ok(())
        } else {
            Err(())
        }
    }
    unsafe fn add(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector;
    unsafe fn sub(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector;
    unsafe fn mul(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector;
    unsafe fn div(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector;
    unsafe fn sqrt(value: Self::Vector) -> Self::Vector;
    unsafe fn abs(value: Self::Vector) -> Self::Vector;
    unsafe fn fma(lhs: Self::Vector, rhs: Self::Vector, addend: Self::Vector) -> Self::Vector {
        Self::add(Self::mul(lhs, rhs), addend)
    }
}

fn get_packed<V: PackedOps>(values: &[Option<V::Vector>], value: Value) -> Result<V::Vector, ()> {
    values
        .get(value.0 as usize)
        .and_then(|value| *value)
        .ok_or(())
}

unsafe fn evaluate_packed<V: PackedOps>(
    function: &Function,
    columns: &[&[f64]],
    start: usize,
) -> Result<Vec<f64>, ()> {
    evaluate_packed_with_active::<V>(function, columns, start, V::LANES)
}

unsafe fn evaluate_packed_with_active<V: PackedOps>(
    function: &Function,
    columns: &[&[f64]],
    start: usize,
    active: usize,
) -> Result<Vec<f64>, ()> {
    if active == 0 || active > V::LANES {
        return Err(());
    }
    let vector = lower_f64_vector(function, V::LANES as u8).map_err(|_| ())?;
    let mut values = vec![None; function.insts.len()];
    for (value, inst) in vector.insts {
        let result = match inst {
            VectorInst::SplatF64(bits) => V::splat(f64::from_bits(bits)),
            VectorInst::Param { index } => {
                let column = columns.get(index as usize).ok_or(())?;
                if start + active > column.len() {
                    return Err(());
                }
                V::load_masked(column[start..].as_ptr(), active)?
            }
            VectorInst::Move(value) => get_packed::<V>(&values, value)?,
            VectorInst::Add(lhs, rhs) => V::add(
                get_packed::<V>(&values, lhs)?,
                get_packed::<V>(&values, rhs)?,
            ),
            VectorInst::Sub(lhs, rhs) => V::sub(
                get_packed::<V>(&values, lhs)?,
                get_packed::<V>(&values, rhs)?,
            ),
            VectorInst::Mul(lhs, rhs) => V::mul(
                get_packed::<V>(&values, lhs)?,
                get_packed::<V>(&values, rhs)?,
            ),
            VectorInst::Div(lhs, rhs) => V::div(
                get_packed::<V>(&values, lhs)?,
                get_packed::<V>(&values, rhs)?,
            ),
            VectorInst::Neg(value) => V::sub(V::splat(0.0), get_packed::<V>(&values, value)?),
            VectorInst::Sqrt(value) => V::sqrt(get_packed::<V>(&values, value)?),
            VectorInst::Abs(value) => V::abs(get_packed::<V>(&values, value)?),
            VectorInst::Fma { a, b, c } => {
                if !V::HAS_FMA {
                    return Err(());
                }
                V::fma(
                    get_packed::<V>(&values, a)?,
                    get_packed::<V>(&values, b)?,
                    get_packed::<V>(&values, c)?,
                )
            }
            VectorInst::VecLoad { .. }
            | VectorInst::VecStore { .. }
            | VectorInst::VecReduce { .. } => return Err(()),
        };
        values[value.0 as usize] = Some(result);
    }
    let result = get_packed::<V>(&values, vector.result)?;
    let mut output = vec![0.0; V::LANES];
    V::store_masked(result, output.as_mut_ptr(), active)?;
    output.truncate(active);
    Ok(output)
}

fn try_evaluate_packed_chunk(
    function: &Function,
    columns: &[&[f64]],
    start: usize,
    width: SimdWidth,
) -> Option<Vec<f64>> {
    #[cfg(target_arch = "x86_64")]
    if width == SimdWidth::F64x8 && std::is_x86_feature_detected!("avx512f") {
        // SAFETY: runtime feature detection proves AVX-512F is available.
        return unsafe { evaluate_packed::<x86_packed::Avx512>(function, columns, start) }.ok();
    }
    #[cfg(target_arch = "x86_64")]
    if width == SimdWidth::F64x4 && std::is_x86_feature_detected!("avx2") {
        // SAFETY: runtime feature detection proves AVX2 is available. FMA is
        // selected separately because fused multiply-add changes rounding.
        if function_uses_fma(function) {
            if !std::is_x86_feature_detected!("fma") {
                return None;
            }
            return unsafe { evaluate_packed::<x86_packed::Avx2Fma>(function, columns, start) }
                .ok();
        }
        return unsafe { evaluate_packed::<x86_packed::Avx2>(function, columns, start) }.ok();
    }
    #[cfg(target_arch = "x86_64")]
    if width == SimdWidth::F64x2 && std::is_x86_feature_detected!("sse2") {
        // SAFETY: SSE2 is guaranteed by the runtime check and lane ranges
        // were checked inside evaluate_packed.
        return unsafe { evaluate_packed::<x86_packed::Sse2>(function, columns, start) }.ok();
    }
    #[cfg(target_arch = "aarch64")]
    if width == SimdWidth::F64x2 {
        // AArch64 always provides the NEON register set used here.
        return unsafe { evaluate_packed::<neon_packed::Neon>(function, columns, start) }.ok();
    }
    None
}

fn try_evaluate_packed_tail(
    function: &Function,
    columns: &[&[f64]],
    start: usize,
    width: SimdWidth,
    active: usize,
) -> Option<Vec<f64>> {
    #[cfg(not(target_arch = "x86_64"))]
    let _ = (function, columns, start, width, active);

    #[cfg(target_arch = "x86_64")]
    if width == SimdWidth::F64x8 && std::is_x86_feature_detected!("avx512f") {
        // SAFETY: AVX-512F is runtime-gated and the masked load/store only
        // accesses the `active` elements that remain in each input column.
        return unsafe {
            evaluate_packed_with_active::<x86_packed::Avx512>(function, columns, start, active)
        }
        .ok();
    }
    None
}

#[cfg(target_arch = "x86_64")]
fn function_uses_fma(function: &Function) -> bool {
    function
        .insts
        .iter()
        .any(|inst| matches!(inst, Inst::Fma { .. }))
}

#[cfg(target_arch = "x86_64")]
mod x86_packed {
    use super::PackedOps;
    use std::arch::asm;
    use std::arch::x86_64::*;

    pub struct Sse2;
    pub struct Avx2;

    /// AVX-512 implementation using stable inline assembly rather than the
    /// still-unstable Rust AVX-512 intrinsics. Every operation is reached only
    /// after `is_x86_feature_detected!("avx512f")` succeeds, and the compiler
    /// is not asked to generate AVX-512 on the portable code path.
    pub struct Avx512;

    macro_rules! avx512_binary {
        ($name:ident, $instruction:literal) => {
            unsafe fn $name(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
                let mut output = [0.0; 8];
                asm!(
                    "vmovupd zmm0, [{lhs}]",
                    "vmovupd zmm1, [{rhs}]",
                    concat!($instruction, " zmm0, zmm0, zmm1"),
                    "vmovupd [{out}], zmm0",
                    lhs = in(reg) lhs.as_ptr(),
                    rhs = in(reg) rhs.as_ptr(),
                    out = in(reg) output.as_mut_ptr(),
                    out("zmm0") _,
                    out("zmm1") _,
                    options(nostack, preserves_flags),
                );
                output
            }
        };
    }

    impl PackedOps for Avx512 {
        type Vector = [f64; 8];
        const LANES: usize = 8;
        const HAS_FMA: bool = true;

        unsafe fn splat(value: f64) -> Self::Vector {
            [value; 8]
        }

        unsafe fn load(values: *const f64) -> Self::Vector {
            let mut output = [0.0; 8];
            asm!(
                "vmovupd zmm0, [{src}]",
                "vmovupd [{dst}], zmm0",
                src = in(reg) values,
                dst = in(reg) output.as_mut_ptr(),
                out("zmm0") _,
                options(nostack, preserves_flags),
            );
            output
        }

        unsafe fn store(value: Self::Vector, values: *mut f64) {
            asm!(
                "vmovupd zmm0, [{src}]",
                "vmovupd [{dst}], zmm0",
                src = in(reg) value.as_ptr(),
                dst = in(reg) values,
                out("zmm0") _,
                options(nostack, preserves_flags),
            );
        }

        unsafe fn load_masked(values: *const f64, active: usize) -> Result<Self::Vector, ()> {
            if active == 0 || active > Self::LANES {
                return Err(());
            }
            let mut output = [0.0; 8];
            asm!(
                "kmovw k1, eax",
                "vmovupd zmm0 {{k1}}{{z}}, [{src}]",
                "vmovupd [{dst}], zmm0",
                src = in(reg) values,
                dst = in(reg) output.as_mut_ptr(),
                in("eax") (1u32 << active) - 1,
                out("zmm0") _,
                out("k1") _,
                options(nostack, preserves_flags),
            );
            Ok(output)
        }

        unsafe fn store_masked(
            value: Self::Vector,
            values: *mut f64,
            active: usize,
        ) -> Result<(), ()> {
            if active == 0 || active > Self::LANES {
                return Err(());
            }
            asm!(
                "kmovw k1, eax",
                "vmovupd zmm0, [{src}]",
                "vmovupd [{dst}] {{k1}}, zmm0",
                src = in(reg) value.as_ptr(),
                dst = in(reg) values,
                in("eax") (1u32 << active) - 1,
                out("zmm0") _,
                out("k1") _,
                options(nostack, preserves_flags),
            );
            Ok(())
        }

        avx512_binary!(add, "vaddpd");
        avx512_binary!(sub, "vsubpd");
        avx512_binary!(mul, "vmulpd");
        avx512_binary!(div, "vdivpd");

        unsafe fn sqrt(value: Self::Vector) -> Self::Vector {
            let mut output = [0.0; 8];
            asm!(
                "vmovupd zmm0, [{value}]",
                "vsqrtpd zmm0, zmm0",
                "vmovupd [{out}], zmm0",
                value = in(reg) value.as_ptr(),
                out = in(reg) output.as_mut_ptr(),
                out("zmm0") _,
                options(nostack, preserves_flags),
            );
            output
        }

        unsafe fn abs(value: Self::Vector) -> Self::Vector {
            let mask = [f64::from_bits(0x7fff_ffff_ffff_ffff); 8];
            let mut output = [0.0; 8];
            asm!(
                "vmovupd zmm0, [{value}]",
                "vmovupd zmm1, [{mask}]",
                "vandpd zmm0, zmm0, zmm1",
                "vmovupd [{out}], zmm0",
                value = in(reg) value.as_ptr(),
                mask = in(reg) mask.as_ptr(),
                out = in(reg) output.as_mut_ptr(),
                out("zmm0") _,
                out("zmm1") _,
                options(nostack, preserves_flags),
            );
            output
        }

        unsafe fn fma(lhs: Self::Vector, rhs: Self::Vector, addend: Self::Vector) -> Self::Vector {
            let mut output = [0.0; 8];
            asm!(
                "vmovupd zmm0, [{addend}]",
                "vmovupd zmm1, [{lhs}]",
                "vmovupd zmm2, [{rhs}]",
                "vfmadd231pd zmm0, zmm1, zmm2",
                "vmovupd [{out}], zmm0",
                addend = in(reg) addend.as_ptr(),
                lhs = in(reg) lhs.as_ptr(),
                rhs = in(reg) rhs.as_ptr(),
                out = in(reg) output.as_mut_ptr(),
                out("zmm0") _,
                out("zmm1") _,
                out("zmm2") _,
                options(nostack, preserves_flags),
            );
            output
        }
    }

    macro_rules! impl_x86_ops {
        ($name:ident, $vector:ty, $lanes:expr, $set1:ident, $load:ident, $store:ident,
         $add:ident, $sub:ident, $mul:ident, $div:ident, $sqrt:ident, $and:ident,
         $mask:expr, $feature:literal) => {
            impl PackedOps for $name {
                type Vector = $vector;
                const LANES: usize = $lanes;
                const HAS_FMA: bool = false;

                #[target_feature(enable = $feature)]
                unsafe fn splat(value: f64) -> Self::Vector {
                    $set1(value)
                }
                #[target_feature(enable = $feature)]
                unsafe fn load(values: *const f64) -> Self::Vector {
                    $load(values)
                }
                #[target_feature(enable = $feature)]
                unsafe fn store(value: Self::Vector, values: *mut f64) {
                    $store(values, value)
                }
                #[target_feature(enable = $feature)]
                unsafe fn add(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
                    $add(lhs, rhs)
                }
                #[target_feature(enable = $feature)]
                unsafe fn sub(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
                    $sub(lhs, rhs)
                }
                #[target_feature(enable = $feature)]
                unsafe fn mul(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
                    $mul(lhs, rhs)
                }
                #[target_feature(enable = $feature)]
                unsafe fn div(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
                    $div(lhs, rhs)
                }
                #[target_feature(enable = $feature)]
                unsafe fn sqrt(value: Self::Vector) -> Self::Vector {
                    $sqrt(value)
                }
                #[target_feature(enable = $feature)]
                unsafe fn abs(value: Self::Vector) -> Self::Vector {
                    $and(value, $mask)
                }
                #[target_feature(enable = $feature)]
                unsafe fn fma(
                    lhs: Self::Vector,
                    rhs: Self::Vector,
                    addend: Self::Vector,
                ) -> Self::Vector {
                    $add($mul(lhs, rhs), addend)
                }
            }
        };
    }

    impl_x86_ops!(
        Sse2,
        __m128d,
        2,
        _mm_set1_pd,
        _mm_loadu_pd,
        _mm_storeu_pd,
        _mm_add_pd,
        _mm_sub_pd,
        _mm_mul_pd,
        _mm_div_pd,
        _mm_sqrt_pd,
        _mm_and_pd,
        _mm_set1_pd(f64::from_bits(0x7fff_ffff_ffff_ffff)),
        "sse2"
    );

    impl_x86_ops!(
        Avx2,
        __m256d,
        4,
        _mm256_set1_pd,
        _mm256_loadu_pd,
        _mm256_storeu_pd,
        _mm256_add_pd,
        _mm256_sub_pd,
        _mm256_mul_pd,
        _mm256_div_pd,
        _mm256_sqrt_pd,
        _mm256_and_pd,
        _mm256_set1_pd(f64::from_bits(0x7fff_ffff_ffff_ffff)),
        "avx2"
    );

    pub struct Avx2Fma;

    impl PackedOps for Avx2Fma {
        type Vector = __m256d;
        const LANES: usize = 4;
        const HAS_FMA: bool = true;

        #[target_feature(enable = "avx2")]
        unsafe fn splat(value: f64) -> Self::Vector {
            _mm256_set1_pd(value)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn load(values: *const f64) -> Self::Vector {
            _mm256_loadu_pd(values)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn store(value: Self::Vector, values: *mut f64) {
            _mm256_storeu_pd(values, value)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn add(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            _mm256_add_pd(lhs, rhs)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn sub(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            _mm256_sub_pd(lhs, rhs)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn mul(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            _mm256_mul_pd(lhs, rhs)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn div(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            _mm256_div_pd(lhs, rhs)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn sqrt(value: Self::Vector) -> Self::Vector {
            _mm256_sqrt_pd(value)
        }
        #[target_feature(enable = "avx2")]
        unsafe fn abs(value: Self::Vector) -> Self::Vector {
            _mm256_and_pd(value, _mm256_set1_pd(f64::from_bits(0x7fff_ffff_ffff_ffff)))
        }
        #[target_feature(enable = "avx2,fma")]
        unsafe fn fma(lhs: Self::Vector, rhs: Self::Vector, addend: Self::Vector) -> Self::Vector {
            _mm256_fmadd_pd(lhs, rhs, addend)
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod neon_packed {
    use super::PackedOps;
    use std::arch::aarch64::*;

    pub struct Neon;

    impl PackedOps for Neon {
        type Vector = float64x2_t;
        const LANES: usize = 2;

        unsafe fn splat(value: f64) -> Self::Vector {
            vdupq_n_f64(value)
        }
        unsafe fn load(values: *const f64) -> Self::Vector {
            vld1q_f64(values)
        }
        unsafe fn store(value: Self::Vector, values: *mut f64) {
            vst1q_f64(values, value)
        }
        unsafe fn add(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            vaddq_f64(lhs, rhs)
        }
        unsafe fn sub(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            vsubq_f64(lhs, rhs)
        }
        unsafe fn mul(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            vmulq_f64(lhs, rhs)
        }
        unsafe fn div(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector {
            vdivq_f64(lhs, rhs)
        }
        unsafe fn sqrt(value: Self::Vector) -> Self::Vector {
            vsqrtq_f64(value)
        }
        unsafe fn abs(value: Self::Vector) -> Self::Vector {
            vabsq_f64(value)
        }
    }
}

/// Selects the widest implementation supported by the current host for the
/// scalar f64 vector pipeline. AVX-512 selection is paired with runtime-gated
/// inline assembly, so this function never claims an ISA on targets where it
/// cannot be queried.
pub fn best_width() -> SimdWidth {
    match CpuFeatures::detect().best_width(Ty::F64) {
        8 => SimdWidth::F64x8,
        4 => SimdWidth::F64x4,
        2 => SimdWidth::F64x2,
        _ => SimdWidth::Scalar,
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn detect_impl() -> CpuFeatures {
    CpuFeatures {
        sse2: std::is_x86_feature_detected!("sse2"),
        sse41: std::is_x86_feature_detected!("sse4.1"),
        avx: std::is_x86_feature_detected!("avx"),
        avx2: std::is_x86_feature_detected!("avx2"),
        fma: std::is_x86_feature_detected!("fma"),
        avx512f: std::is_x86_feature_detected!("avx512f"),
        avx512dq: std::is_x86_feature_detected!("avx512dq"),
        bmi2: std::is_x86_feature_detected!("bmi2"),
        neon: false,
        sve: false,
    }
}

#[cfg(target_arch = "aarch64")]
fn detect_impl() -> CpuFeatures {
    CpuFeatures {
        neon: true,
        ..CpuFeatures::scalar()
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
fn detect_impl() -> CpuFeatures {
    CpuFeatures::scalar()
}

pub fn host_supports_simd() -> bool {
    best_width() != SimdWidth::Scalar
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_features_choose_the_widest_supported_width() {
        let mut features = CpuFeatures::scalar();
        assert_eq!(features.best_width(Ty::F64), 1);
        features.sse2 = true;
        assert_eq!(features.best_width(Ty::F64), 2);
        features.avx2 = true;
        assert_eq!(features.best_width(Ty::F64), 4);
        features.avx512f = true;
        assert_eq!(features.best_width(Ty::F64), 8);
        assert_eq!(features.best_width(Ty::I64), 1);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn avx512_masked_tail_matches_scalar_elements_when_available() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let left = (0..17).map(|value| value as f64 - 8.0).collect::<Vec<_>>();
        let right = (0..17)
            .map(|value| value as f64 * 0.25 + 1.0)
            .collect::<Vec<_>>();
        let addend = (0..17).map(|value| value as f64 - 2.0).collect::<Vec<_>>();
        let result = evaluate_array("fma(x, y, z) + abs(x)", &[&left, &right, &addend])
            .expect("AVX-512 array evaluation");

        let expected = left
            .iter()
            .zip(&right)
            .zip(&addend)
            .map(|((x, y), z)| x.mul_add(*y, *z) + x.abs())
            .collect::<Vec<_>>();
        assert_eq!(result.plan.width, SimdWidth::F64x8);
        assert_eq!(result.values, expected);
        assert!(result.used_packed_backend);
    }

    #[test]
    fn detected_features_are_consistent_with_the_public_width() {
        let features = CpuFeatures::detect();
        assert_eq!(
            best_width().lanes(),
            usize::from(features.best_width(Ty::F64))
        );
    }

    #[test]
    fn array_fallback_handles_every_tail_length() {
        for length in 1..=100 {
            let input = (0..length).map(|value| value as f64).collect::<Vec<_>>();
            let result = evaluate_array("x * x + 1.0", &[&input]).unwrap();
            let expected = input
                .iter()
                .map(|value| value * value + 1.0)
                .collect::<Vec<_>>();
            assert_eq!(result.values, expected);
            let avx512_masked_tail = cfg!(target_arch = "x86_64")
                && result.plan.width == SimdWidth::F64x8
                && CpuFeatures::detect().avx512f
                && result.plan.tail > 0;
            assert_eq!(
                result.used_packed_backend,
                result.plan.width != SimdWidth::Scalar
                    && (result.plan.full_chunks > 0 || avx512_masked_tail)
            );
            assert_eq!(result.plan.elements, length);
            assert_eq!(
                result.plan.full_chunks * result.plan.width.lanes() + result.plan.tail,
                length
            );
        }
    }

    #[test]
    fn array_fallback_rejects_mismatched_columns() {
        let left = [1.0, 2.0];
        let right = [3.0];
        assert!(evaluate_array("x + y", &[&left, &right]).is_err());
    }

    #[test]
    fn unsupported_control_flow_keeps_the_scalar_fallback() {
        let input = [0.0, 1.0, 2.0, 3.0];
        let result = evaluate_array("if x < 2.0 then x + 1.0 else x - 1.0", &[&input]).unwrap();
        assert_eq!(result.values, vec![1.0, 2.0, 1.0, 2.0]);
        assert!(!result.used_packed_backend);
    }

    #[test]
    fn sum_reduction_preserves_source_order_with_packed_chunks_and_tail() {
        let input = (0..11).map(|value| value as f64).collect::<Vec<_>>();
        let result = reduce_sum("x * x + 1.0", &[&input]).unwrap();
        let expected = input
            .iter()
            .fold(0.0, |sum, value| sum + value * value + 1.0);
        assert_eq!(result.value.to_bits(), expected.to_bits());
        assert_eq!(result.plan.elements, input.len());
        assert!(result.used_packed_backend == (result.plan.full_chunks > 0));
    }

    #[test]
    fn sum_reduction_uses_scalar_oracle_for_control_flow() {
        let input = [0.0, 1.0, 2.0, 3.0];
        let result = reduce_sum("if x < 2.0 then x + 1.0 else x - 1.0", &[&input]).unwrap();
        assert_eq!(result.value, 6.0);
        assert!(!result.used_packed_backend);
    }

    #[test]
    fn fma_uses_the_fused_packed_path_only_when_available() {
        let left = vec![1.0e16; 9];
        let right = vec![1.0000000000000002; 9];
        let addend = vec![-1.0e16; 9];
        let result = evaluate_array("fma(x, y, z)", &[&left, &right, &addend]).unwrap();
        let expected = left
            .iter()
            .zip(&right)
            .zip(&addend)
            .map(|((left, right), addend)| left.mul_add(*right, *addend))
            .collect::<Vec<_>>();
        assert_eq!(result.values, expected);
        let avx512_fma = cfg!(target_arch = "x86_64")
            && result.plan.width == SimdWidth::F64x8
            && CpuFeatures::detect().avx512f;
        assert_eq!(
            result.used_packed_backend,
            (result.plan.width == SimdWidth::F64x4
                && CpuFeatures::detect().avx2
                && CpuFeatures::detect().fma)
                || avx512_fma
        );
    }

    #[test]
    fn lower_f64_vector_builds_typed_lane_ir_for_the_packed_backend() {
        let function = forge_runtime::lower_source("fma(x, 2.0, y) + sqrt(abs(x))").unwrap();
        let vector = lower_f64_vector(&function, 4).unwrap();
        assert_eq!(vector.lanes, 4);
        assert!(vector
            .insts
            .iter()
            .any(|(_, inst)| matches!(inst, VectorInst::Param { index: 0 })));
        assert!(vector
            .insts
            .iter()
            .any(|(_, inst)| matches!(inst, VectorInst::Fma { .. })));
        assert!(vector
            .insts
            .iter()
            .any(|(_, inst)| matches!(inst, VectorInst::Sqrt(_))));
    }

    #[test]
    fn vector_lowering_rejects_unsupported_widths_and_control_flow() {
        let straight_line = forge_runtime::lower_source("x + 1.0").unwrap();
        assert!(lower_f64_vector(&straight_line, 3).is_err());

        let control_flow = forge_runtime::lower_source("if x < 0.0 then x else -x").unwrap();
        let error = lower_f64_vector(&control_flow, 2).unwrap_err();
        assert!(error.contains("straight-line"));
    }
}
