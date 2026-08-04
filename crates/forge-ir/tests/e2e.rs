// crates/forge-ir/tests/e2e.rs

//! Exit criterion #3 from the design doc: the real
//! source → lex → parse → resolve → typecheck → lower → interpret path,
//! for one representative expression per language feature this slice
//! supports.

use forge_ir::interp::{interpret, RtValue};
use forge_ir::lower::lower;
use forge_ir::verify::verify;
use forge_syntax::lexer::lex;
use forge_syntax::parser::parse;
use forge_syntax::resolve::resolve;
use forge_syntax::typeck::typecheck;

fn eval(src: &str, args: &[RtValue]) -> RtValue {
    let (tokens, diags) = lex(src);
    assert!(diags.is_empty(), "lex errors for {src:?}: {diags:?}");
    let (ast, diags) = parse(&tokens);
    assert!(diags.is_empty(), "parse errors for {src:?}: {diags:?}");
    let typed =
        typecheck(resolve(ast)).unwrap_or_else(|e| panic!("type errors for {src:?}: {e:?}"));
    let f = lower(&typed);
    verify(&f).unwrap_or_else(|e| panic!("verifier rejected {src:?}: {e}"));
    interpret(&f, args)
}

#[test]
#[allow(clippy::approx_constant)] // spec test data (SPEC §3's own example), not a Pi typo
fn straight_line_arithmetic() {
    assert_eq!(
        eval("3.14159 * r * r", &[RtValue::F64(2.0)]),
        RtValue::F64(3.14159 * 2.0 * 2.0)
    );
}

#[test]
fn if_and_let() {
    let r = eval(
        "let t = a - b in if t > 0.0 then t else -t",
        &[RtValue::F64(3.0), RtValue::F64(5.0)],
    );
    assert_eq!(r, RtValue::F64(2.0)); // |3 - 5|
}

#[test]
fn intrinsic_sqrt() {
    assert_eq!(
        eval(
            "sqrt(x * x + y * y)",
            &[RtValue::F64(3.0), RtValue::F64(4.0)]
        ),
        RtValue::F64(5.0)
    );
}

#[test]
fn libm_call() {
    let r = eval("sin(x) + cos(y)", &[RtValue::F64(0.0), RtValue::F64(0.0)]);
    assert_eq!(r, RtValue::F64(0.0f64.sin() + 0.0f64.cos()));
}

#[test]
fn integer_and_bitwise_expression() {
    let r = eval("(n * 2654435761) >> 16", &[RtValue::I64(999)]);
    assert_eq!(r, RtValue::I64((999i64.wrapping_mul(2654435761)) >> 16));
}

#[test]
fn nan_producing_expression() {
    let r = eval("x / y", &[RtValue::F64(0.0), RtValue::F64(0.0)]);
    assert!(matches!(r, RtValue::F64(v) if v.is_nan()));
}

#[test]
fn mixed_i64_f64_widening_end_to_end() {
    // x is f64 (default inference); `1` is an i64 literal that must widen.
    let r = eval("x + 1", &[RtValue::F64(2.5)]);
    assert_eq!(r, RtValue::F64(3.5));
}
