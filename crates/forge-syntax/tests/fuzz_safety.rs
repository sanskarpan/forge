//! Panic-safety coverage for the parser's untrusted text boundary.
//!
//! The parser reports malformed syntax through diagnostics. It must not turn
//! arbitrary bytes supplied by an editor, file, or fuzzing harness into a
//! process panic, even when the bytes are not valid UTF-8.

use forge_syntax::{lexer::lex, parser::parse};
use proptest::prelude::*;
use std::panic::{catch_unwind, AssertUnwindSafe};

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 512,
        max_shrink_iters: 256,
        .. ProptestConfig::default()
    })]

    #[test]
    fn arbitrary_bytes_never_panic_during_frontend(bytes in prop::collection::vec(any::<u8>(), 0..=512)) {
        let source = String::from_utf8_lossy(&bytes);
        let result = catch_unwind(AssertUnwindSafe(|| {
            let (tokens, lex_diagnostics) = lex(&source);
            let (ast, parse_diagnostics) = parse(&tokens);

            // Exercise the complete parser result so this remains a useful
            // regression test even when the input happens to be valid.
            assert!(ast.spans.len() == ast.exprs.len());
            let _ = (lex_diagnostics, parse_diagnostics);
        }));

        prop_assert!(
            result.is_ok(),
            "frontend panicked for arbitrary bytes: {bytes:?}"
        );
    }
}
