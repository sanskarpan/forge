// crates/forge-opt/tests/differential.rs

//! The core correctness invariant for the whole optimizer: `-O0 == -O2`.
//! Runs a broad random corpus of expressions through the real
//! source -> lex -> parse -> resolve -> typecheck -> lower pipeline, then
//! compares `interpret` on the unoptimized IR against `interpret` on the
//! IR after the full `forge_opt::optimize` pipeline (const-fold, algebraic
//! simplify, strength reduction, GVN/CSE, reassociation, DCE). Any
//! mismatch is a real optimizer bug -- see Tasks 2-8's individual pass
//! test suites for the per-pass unit coverage this is meant to complement,
//! not replace.

use forge_ir::interp::{interpret, RtValue};
use forge_syntax::lexer::lex;
use forge_syntax::parser::parse;
use forge_syntax::resolve::resolve;
use forge_syntax::typeck::typecheck;
use proptest::prelude::*;

/// A tiny well-formed-expression generator, extended from
/// `forge-syntax/tests/roundtrip.rs`'s pattern to cover every op this
/// optimizer actually touches: arithmetic, comparisons, `if`, intrinsics,
/// and -- importantly -- integer literals and bitwise/shift ops, since
/// strength reduction only fires on i64.
fn arb_expr() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        (0.1f64..1000.0).prop_map(|f| format!("{f:.3}")),
        (1i64..1000).prop_map(|n| n.to_string()),
        Just("x".to_string()),
        Just("y".to_string()),
        Just("n".to_string()),
    ];
    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} + {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} - {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} * {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} / {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} == {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} < {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} & {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} | {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} ^ {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} << {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} >> {b})")),
            (inner.clone(), inner.clone(), inner.clone())
                .prop_map(|(c, t, e)| format!("(if {c} > 0.0 then {t} else {e})")),
            inner.clone().prop_map(|a| format!("sqrt({a} * {a})")),
            inner.clone().prop_map(|a| format!("abs({a})")),
            inner.clone().prop_map(|a| format!("-{a}")),
        ]
    })
}

fn params_for(f: &forge_ir::Function) -> Vec<RtValue> {
    f.params
        .iter()
        .map(|(_, ty)| match ty {
            forge_ir::Ty::F64 => RtValue::F64(2.5),
            forge_ir::Ty::I64 => RtValue::I64(7),
            forge_ir::Ty::Bool => RtValue::Bool(true),
        })
        .collect()
}

/// Runs `interpret`, catching the one panic it can legitimately raise: i64
/// division/remainder by zero. Per SPEC.md's rule table (the `x / x -> 1`
/// entry) and `fold.rs`'s `does_not_panic_on_i64_div_by_zero_even_in_dead_code`
/// test, this is a genuine runtime trap that both `-O0` and `-O2` must hit
/// identically -- the optimizer must never erase it (that would silently
/// change program behavior) nor introduce one that wasn't there
/// unoptimized. `Err(())` here means "trapped"; callers compare trap-for-
/// trap rather than treating a trap as an interpreter bug.
fn try_interpret(f: &forge_ir::Function, args: &[RtValue]) -> Result<RtValue, ()> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| interpret(f, args))).map_err(|_| ())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn optimized_matches_unoptimized(src in arb_expr()) {
        let (tokens, diags) = lex(&src);
        prop_assume!(diags.is_empty());
        let (ast, diags) = parse(&tokens);
        prop_assume!(diags.is_empty());
        let Ok(typed) = typecheck(resolve(ast)) else { return Ok(()); };
        let unoptimized = forge_ir::lower::lower(&typed);
        let mut optimized = forge_ir::lower::lower(&typed);
        forge_opt::optimize(&mut optimized);

        prop_assert!(forge_ir::verify::verify(&optimized).is_ok(), "optimized IR failed verification for {src:?}");

        let args = params_for(&unoptimized);

        // i64 div/rem by zero is a genuine runtime trap (see try_interpret's
        // doc comment) -- silence the default panic-hook printout while we
        // deliberately trigger and catch it up to twice per case.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let expected = try_interpret(&unoptimized, &args);
        let actual = try_interpret(&optimized, &args);
        std::panic::set_hook(prev_hook);

        match (expected, actual) {
            // Both sides trapped (i64 div/rem by zero): the optimizer
            // preserved the trap rather than erasing or introducing it.
            (Err(()), Err(())) => {}
            (Err(()), Ok(a)) => prop_assert!(
                false,
                "unoptimized trapped (i64 div/rem by zero) but optimized returned {a:?} for {src:?} -- optimizer erased a trap"
            ),
            (Ok(e), Err(())) => prop_assert!(
                false,
                "unoptimized returned {e:?} but optimized trapped for {src:?} -- optimizer introduced a trap"
            ),
            (Ok(RtValue::F64(e)), Ok(RtValue::F64(a))) => {
                if e.is_nan() {
                    prop_assert!(a.is_nan(), "NaN-ness mismatch for {src:?}");
                } else {
                    prop_assert_eq!(e.to_bits(), a.to_bits(), "mismatch for {}: {} vs {}", src, e, a);
                }
            }
            (Ok(e), Ok(a)) => prop_assert_eq!(e, a, "mismatch for {}", src),
        }
    }
}
