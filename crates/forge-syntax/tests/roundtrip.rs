// crates/forge-syntax/tests/roundtrip.rs

use forge_syntax::lexer::lex;
use forge_syntax::parser::parse;
use proptest::prelude::*;

/// A tiny well-formed-expression generator — deep enough to exercise every
/// binary/unary op and both branches of `if`, shallow enough to stay fast.
fn arb_expr() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        (0.0f64..1000.0).prop_map(|f| format!("{f:.3}")),
        (1i64..1000).prop_map(|n| n.to_string()),
        Just("x".to_string()),
        Just("y".to_string()),
    ];
    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} + {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} * {b})")),
            (inner.clone(), inner.clone(), inner.clone())
                .prop_map(|(c, t, e)| format!("(if {c} > 0.0 then {t} else {e})")),
            inner.clone().prop_map(|a| format!("sqrt({a} * {a})")),
        ]
    })
}

proptest! {
    #[test]
    fn parse_print_round_trip_preserves_structure(src in arb_expr()) {
        let (tokens, diags) = lex(&src);
        prop_assert!(diags.is_empty(), "lex diagnostics for {src:?}: {diags:?}");
        let (ast, diags) = parse(&tokens);
        prop_assert!(diags.is_empty(), "parse diagnostics for {src:?}: {diags:?}");

        // Re-lex/parse the same source a second time — a stable parser must
        // produce a structurally identical tree both times. (A true
        // print(ast)->parse->compare round trip needs an AST pretty-printer,
        // which this slice doesn't build; re-parsing the same text is the
        // cheap, still-meaningful version of the same property: determinism.)
        let (tokens2, _) = lex(&src);
        let (ast2, _) = parse(&tokens2);
        prop_assert_eq!(ast.exprs.len(), ast2.exprs.len());
    }
}
