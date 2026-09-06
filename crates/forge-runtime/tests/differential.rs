//! End-to-end native-backend differential coverage.
//!
//! The optimizer differential test compares two interpreter executions. This
//! suite crosses the remaining boundary: the same source and inputs are run
//! through the active native pipeline (x86 JIT or the supported AArch64 path)
//! and the reference interpreter. A non-NaN result must match by its IEEE-754
//! bit pattern; NaNs are compared by class because a foreign libm call may
//! choose a different payload.

use forge_ir::interp::RtValue;
use forge_runtime::{evaluate, interpret_source, lower_source};
use proptest::prelude::*;

fn assert_same(expected: RtValue, actual: f64, source: &str, args: &[f64]) {
    let RtValue::F64(expected) = expected else {
        panic!("differential source returned a non-f64 value: {source:?}");
    };
    if expected.is_nan() || actual.is_nan() {
        assert!(
            expected.is_nan() && actual.is_nan(),
            "NaN mismatch for {source:?} with args={args:?}: interpreter={expected:?}, jit={actual:?}"
        );
    } else {
        assert_eq!(
            expected.to_bits(),
            actual.to_bits(),
            "bit mismatch for {source:?} with args={args:?}: interpreter={expected:?}, jit={actual:?}"
        );
    }
}

fn args_for(source: &str) -> Vec<RtValue> {
    let function = lower_source(source).expect("generated source must lower");
    function
        .params
        .iter()
        .enumerate()
        .map(|(index, (_, ty))| {
            assert_eq!(*ty, forge_ir::Ty::F64);
            RtValue::F64(1.25 + index as f64 * 0.75)
        })
        .collect()
}

fn arb_f64_expr() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        Just("x".to_string()),
        Just("y".to_string()),
        Just("0.5".to_string()),
        Just("1.0".to_string()),
        Just("2.0".to_string()),
        Just("3.0".to_string()),
    ];
    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} + {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} - {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} * {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} / (abs({b}) + 1.0))")),
            inner.clone().prop_map(|a| format!("abs({a})")),
            inner.clone().prop_map(|a| format!("sqrt(abs({a}))")),
            inner.clone().prop_map(|a| format!("log(abs({a}) + 1.0)")),
            inner
                .clone()
                .prop_map(|a| { format!("(if ({a} < 0.0) then ({a} * {a}) else ({a} + 1.0))") }),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("min({a}, {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("max({a}, {b})")),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn jit_matches_interpreter_for_generated_f64_expressions(source in arb_f64_expr()) {
        let args = args_for(&source);
        let expected = interpret_source(&source, &args).unwrap();
        let raw_args = args.iter().map(|value| value.as_f64()).collect::<Vec<_>>();
        let actual = evaluate(&source, &raw_args).unwrap();
        assert_same(expected, actual, &source, &raw_args);
    }
}

#[test]
fn jit_preserves_special_values_and_signed_zeroes() {
    let inputs = [
        0.0,
        -0.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        f64::MAX,
        f64::MIN,
    ];
    let sources = [
        "x + 0.0",
        "x * 1.0",
        "x / (abs(x) + 1.0)",
        "sqrt(abs(x))",
        "min(x, 1.0)",
        "max(x, -1.0)",
        "if x < 0.0 then abs(x) else x",
    ];

    for source in sources {
        for input in inputs {
            let expected = interpret_source(source, &[RtValue::F64(input)]).unwrap();
            let actual = evaluate(source, &[input]).unwrap();
            assert_same(expected, actual, source, &[input]);
        }
    }
}
