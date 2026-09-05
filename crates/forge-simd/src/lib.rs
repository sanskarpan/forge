//! Runtime SIMD capability selection shared by native front ends.

use forge_ir::{Function, Inst, Terminator, Ty, Value};

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
    /// False until packed vector IR and encoders land. Keeping this explicit
    /// prevents callers from mistaking the correct scalar fallback for SIMD.
    pub used_packed_backend: bool,
}

/// Evaluates a pure expression over one column per free f64 parameter. Full
/// chunks use the widest safe packed backend for the host when the lowered
/// function is a straight-line f64 expression; the scalar interpreter remains
/// the correctness fallback for control flow, libm calls, and operations whose
/// hardware NaN/rounding behavior does not exactly match the oracle.
pub fn evaluate_array(source: &str, columns: &[&[f64]]) -> Result<ArrayResult, String> {
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
    let plan = ArrayPlan::for_len(elements);
    if plan.full_chunks > 0 && plan.width != SimdWidth::Scalar {
        let mut values = Vec::with_capacity(elements);
        let lanes = plan.width.lanes();
        let packed_chunks = (0..plan.full_chunks)
            .map(|chunk| try_evaluate_packed_chunk(&function, columns, chunk * lanes, plan.width))
            .collect::<Option<Vec<_>>>();
        if let Some(chunks) = packed_chunks {
            for chunk in chunks {
                values.extend(chunk);
            }
            for index in plan.full_chunks * lanes..elements {
                let args = columns
                    .iter()
                    .map(|column| column[index])
                    .collect::<Vec<_>>();
                values.push(
                    forge_runtime::evaluate(source, &args).map_err(|error| error.to_string())?,
                );
            }
            return Ok(ArrayResult {
                values,
                plan,
                used_packed_backend: true,
            });
        }
    }

    let mut values = Vec::with_capacity(elements);
    for index in 0..elements {
        let args = columns
            .iter()
            .map(|column| column[index])
            .collect::<Vec<_>>();
        values.push(forge_runtime::evaluate(source, &args).map_err(|error| error.to_string())?);
    }
    Ok(ArrayResult {
        values,
        plan,
        used_packed_backend: false,
    })
}

trait PackedOps {
    type Vector: Copy;
    const LANES: usize;

    unsafe fn splat(value: f64) -> Self::Vector;
    unsafe fn load(values: *const f64) -> Self::Vector;
    unsafe fn store(value: Self::Vector, values: *mut f64);
    unsafe fn add(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector;
    unsafe fn sub(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector;
    unsafe fn mul(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector;
    unsafe fn div(lhs: Self::Vector, rhs: Self::Vector) -> Self::Vector;
    unsafe fn sqrt(value: Self::Vector) -> Self::Vector;
    unsafe fn abs(value: Self::Vector) -> Self::Vector;
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
    if function.blocks.len() != 1
        || !matches!(
            function.blocks[0].term.as_ref(),
            Some(Terminator::Return(_))
        )
    {
        return Err(());
    }
    let mut values = vec![None; function.insts.len()];
    for &value in &function.blocks[0].insts {
        let result = match &function.insts[value.0 as usize] {
            Inst::ConstF64(bits) => V::splat(f64::from_bits(*bits)),
            // An integer constant can only reach an all-f64 expression via
            // IToF. Representing it as f64 here preserves that conversion.
            Inst::ConstI64(value) => V::splat(*value as f64),
            Inst::ConstBool(value) => V::splat(if *value { 1.0 } else { 0.0 }),
            Inst::Param { index, ty: Ty::F64 } => {
                let column = columns.get(*index as usize).ok_or(())?;
                if start + V::LANES > column.len() {
                    return Err(());
                }
                V::load(column[start..].as_ptr())
            }
            Inst::Param { .. } => return Err(()),
            Inst::Add(lhs, rhs) => V::add(
                get_packed::<V>(&values, *lhs)?,
                get_packed::<V>(&values, *rhs)?,
            ),
            Inst::Sub(lhs, rhs) => V::sub(
                get_packed::<V>(&values, *lhs)?,
                get_packed::<V>(&values, *rhs)?,
            ),
            Inst::Mul(lhs, rhs) => V::mul(
                get_packed::<V>(&values, *lhs)?,
                get_packed::<V>(&values, *rhs)?,
            ),
            Inst::Div(lhs, rhs) => V::div(
                get_packed::<V>(&values, *lhs)?,
                get_packed::<V>(&values, *rhs)?,
            ),
            Inst::Neg(value) => V::sub(V::splat(0.0), get_packed::<V>(&values, *value)?),
            Inst::Sqrt(value) => V::sqrt(get_packed::<V>(&values, *value)?),
            Inst::Abs(value) => V::abs(get_packed::<V>(&values, *value)?),
            Inst::IToF(value) => get_packed::<V>(&values, *value)?,
            // These operations are deliberately rejected instead of being
            // approximated: their scalar oracle semantics are not guaranteed
            // by the corresponding packed instruction on every ISA.
            Inst::Fma { .. }
            | Inst::Rem(..)
            | Inst::And(..)
            | Inst::Or(..)
            | Inst::Xor(..)
            | Inst::Not(..)
            | Inst::Shl(..)
            | Inst::Shr(..)
            | Inst::Sar(..)
            | Inst::Cmp { .. }
            | Inst::Min(..)
            | Inst::Max(..)
            | Inst::Floor(..)
            | Inst::Ceil(..)
            | Inst::Round(..)
            | Inst::Trunc(..)
            | Inst::Call { .. }
            | Inst::FToI(..)
            | Inst::Phi { .. } => return Err(()),
        };
        values[value.0 as usize] = Some(result);
    }
    let result = match function.blocks[0].term.as_ref() {
        Some(Terminator::Return(value)) => get_packed::<V>(&values, *value)?,
        _ => return Err(()),
    };
    let mut output = vec![0.0; V::LANES];
    V::store(result, output.as_mut_ptr());
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
        // SAFETY: runtime feature detection proves AVX-512F is available;
        // evaluate_packed only loads the caller-validated lane ranges.
        return unsafe { evaluate_packed::<x86_packed::Avx512>(function, columns, start) }.ok();
    }
    #[cfg(target_arch = "x86_64")]
    if width == SimdWidth::F64x4 && std::is_x86_feature_detected!("avx2") {
        // SAFETY: runtime feature detection proves AVX2 is available.
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

#[cfg(target_arch = "x86_64")]
mod x86_packed {
    use super::PackedOps;
    use std::arch::x86_64::*;

    pub struct Sse2;
    pub struct Avx2;
    pub struct Avx512;

    macro_rules! impl_x86_ops {
        ($name:ident, $vector:ty, $lanes:expr, $set1:ident, $load:ident, $store:ident,
         $add:ident, $sub:ident, $mul:ident, $div:ident, $sqrt:ident, $and:ident,
         $mask:expr, $feature:literal) => {
            impl PackedOps for $name {
                type Vector = $vector;
                const LANES: usize = $lanes;

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

    impl_x86_ops!(
        Avx512,
        __m512d,
        8,
        _mm512_set1_pd,
        _mm512_loadu_pd,
        _mm512_storeu_pd,
        _mm512_add_pd,
        _mm512_sub_pd,
        _mm512_mul_pd,
        _mm512_div_pd,
        _mm512_sqrt_pd,
        _mm512_and_pd,
        _mm512_set1_pd(f64::from_bits(0x7fff_ffff_ffff_ffff)),
        "avx512f"
    );
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
/// scalar f64 vector pipeline. The actual packed encoder remains a separate
/// backend; this function is nevertheless useful to callers and never
/// claims AVX support on targets where it cannot be queried.
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
            assert_eq!(
                result.used_packed_backend,
                result.plan.width != SimdWidth::Scalar && result.plan.full_chunks > 0
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
}
