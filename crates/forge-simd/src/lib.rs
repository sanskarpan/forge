//! Runtime SIMD capability selection shared by native front ends.

use forge_ir::Ty;

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

/// Evaluates a pure expression over one column per free f64 parameter. The
/// planner partitions the input into full-width chunks and a scalar tail;
/// this implementation executes each lane through the verified runtime
/// fallback until the packed backend is available.
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
            assert!(!result.used_packed_backend);
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
}
