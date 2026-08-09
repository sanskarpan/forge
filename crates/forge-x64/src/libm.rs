use forge_ir::LibFunc;

extern "C" {
    fn sin(x: f64) -> f64;
    fn cos(x: f64) -> f64;
    fn tan(x: f64) -> f64;
    fn exp(x: f64) -> f64;
    fn log(x: f64) -> f64;
    fn pow(x: f64, y: f64) -> f64;
}

/// Resolves `func`'s real, process-wide libm symbol to an absolute
/// address suitable for `Assembler::mov_reg_imm` (which already
/// auto-selects the 10-byte movabs form for any value that doesn't fit
/// i32 -- no new encoder support needed) followed by `call_reg` --
/// `call_reg`'s own doc comment in assembler.rs already documents why an
/// indirect call through a resolved absolute address is required here: a
/// direct rel32 call can't reliably reach libm from a JIT-allocated page,
/// whose distance from libc in the address space isn't bounded to
/// +/-2GiB.
///
/// C's `log` is natural log (matches forge-ir's interpreter oracle,
/// `LibFunc::Log => a.ln()` in interp.rs -- NOT base-10 log10).
pub fn libm_address(func: LibFunc) -> i64 {
    type Unary = unsafe extern "C" fn(f64) -> f64;
    type Binary = unsafe extern "C" fn(f64, f64) -> f64;
    // The extra `as usize` hop before `as i64` is required, not stylistic:
    // casting a function pointer directly to i64 trips clippy::fn_to_numeric_cast
    // (a default-warn lint under this project's -D warnings gate) -- casting
    // through usize first (the pointer-width unsigned integer type) is the
    // idiomatic way to convert a fn pointer to an integer without tripping it.
    match func {
        LibFunc::Sin => sin as Unary as usize as i64,
        LibFunc::Cos => cos as Unary as usize as i64,
        LibFunc::Tan => tan as Unary as usize as i64,
        LibFunc::Exp => exp as Unary as usize as i64,
        LibFunc::Log => log as Unary as usize as i64,
        LibFunc::Pow => pow as Binary as usize as i64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Casts `addr` back through the appropriate function-pointer type and
    /// calls it -- this is what the future call-emission step effectively
    /// does at runtime via mov_reg_imm+call_reg, just done here in-process
    /// instead of through JIT-generated machine code.
    unsafe fn call_unary(addr: i64, x: f64) -> f64 {
        let f: unsafe extern "C" fn(f64) -> f64 = std::mem::transmute(addr as usize);
        f(x)
    }

    unsafe fn call_binary(addr: i64, x: f64, y: f64) -> f64 {
        let f: unsafe extern "C" fn(f64, f64) -> f64 = std::mem::transmute(addr as usize);
        f(x, y)
    }

    #[test]
    fn all_six_addresses_are_real_and_pairwise_distinct() {
        let addrs = [
            libm_address(LibFunc::Sin),
            libm_address(LibFunc::Cos),
            libm_address(LibFunc::Tan),
            libm_address(LibFunc::Exp),
            libm_address(LibFunc::Log),
            libm_address(LibFunc::Pow),
        ];
        for &a in &addrs {
            assert_ne!(a, 0, "resolved address must not be null");
        }
        for i in 0..addrs.len() {
            for j in (i + 1)..addrs.len() {
                assert_ne!(
                    addrs[i], addrs[j],
                    "addrs[{i}] and addrs[{j}] must be distinct libm symbols"
                );
            }
        }
    }

    /// Bit-exact, not approximate: this is the SAME underlying libm
    /// implementation Rust's own f64::sin/cos/tan/exp/ln/powf call into on
    /// every platform this project's CI targets, so exact equality is the
    /// correct, achievable bar (matching this project's FMA-vs-approximation
    /// precision discipline of never silently accepting "close enough" where
    /// exact is achievable).
    ///
    /// Test inputs are deliberately NOT special-cased exponents/identities
    /// (no 0.0, 1.0, 2.0, -1.0, or 0.5 exponents for Pow) -- see
    /// crates/forge-opt/src/strength.rs's own documented
    /// `LibCallSimplifier` hazard: LLVM recognizes calls literally named
    /// `pow`/`sin`/etc (even through a hand-written `extern "C"`
    /// declaration) and rewrites special-cased inputs to fmul/fdiv/sqrt at
    /// compile time, which would make comparing against `f64::powf` for
    /// exactly those inputs circular. `std::hint::black_box` on the f64::*
    /// oracle side is extra defense-in-depth against exactly that.
    #[test]
    fn resolved_addresses_are_bit_exact_against_rust_std_math() {
        use std::hint::black_box;

        for &x in &[0.5f64, 1.0, 2.0, -1.5] {
            unsafe {
                assert_eq!(
                    call_unary(libm_address(LibFunc::Sin), x),
                    black_box(x).sin()
                );
                assert_eq!(
                    call_unary(libm_address(LibFunc::Cos), x),
                    black_box(x).cos()
                );
                assert_eq!(
                    call_unary(libm_address(LibFunc::Tan), x),
                    black_box(x).tan()
                );
                assert_eq!(
                    call_unary(libm_address(LibFunc::Exp), x),
                    black_box(x).exp()
                );
            }
        }
        // Log: positive inputs only (ln of a negative number is NaN, not a
        // meaningful bit-exact comparison target).
        for &x in &[0.5f64, 1.0, 2.0] {
            unsafe {
                assert_eq!(call_unary(libm_address(LibFunc::Log), x), black_box(x).ln());
            }
        }
        unsafe {
            assert_eq!(
                call_binary(libm_address(LibFunc::Pow), 2.0, 10.0),
                black_box(2.0f64).powf(black_box(10.0))
            );
        }
    }

    /// Confirms libc's `log` really is natural log (matching forge-ir's
    /// interpreter oracle, `LibFunc::Log => a.ln()` in interp.rs) and NOT
    /// base-10 log10 -- ln(e) ~= 1.0, log10(e) ~= 0.434.
    #[test]
    fn log_is_natural_log_not_log10() {
        unsafe {
            let result = call_unary(libm_address(LibFunc::Log), std::f64::consts::E);
            assert!(
                (result - 1.0).abs() < 1e-10,
                "expected ln(e) ~= 1.0, got {result}"
            );
        }
    }
}
