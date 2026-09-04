//! Runtime SIMD capability selection shared by native front ends.

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

/// Selects the widest implementation supported by the current host for the
/// scalar f64 vector pipeline. The actual packed encoder remains a separate
/// backend; this function is nevertheless useful to callers and never
/// claims AVX support on targets where it cannot be queried.
pub fn best_width() -> SimdWidth {
    best_width_impl()
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn best_width_impl() -> SimdWidth {
    if std::is_x86_feature_detected!("avx512f") {
        SimdWidth::F64x8
    } else if std::is_x86_feature_detected!("avx2") {
        SimdWidth::F64x4
    } else if std::is_x86_feature_detected!("sse2") {
        SimdWidth::F64x2
    } else {
        SimdWidth::Scalar
    }
}

#[cfg(target_arch = "aarch64")]
fn best_width_impl() -> SimdWidth {
    SimdWidth::F64x2
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
fn best_width_impl() -> SimdWidth {
    SimdWidth::Scalar
}

pub fn host_supports_simd() -> bool {
    best_width() != SimdWidth::Scalar
}
