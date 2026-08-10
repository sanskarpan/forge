#[test]
fn fused_output_across_the_whole_corpus_still_produces_valid_allocations() {
    let corpus = [
        "3.14159 * r * r",
        "sin(x) + cos(y)",
        "(n * 2654435761) >> 16",
        "x / y",
        "x + 1",
        "fma(a, b, c)",
        "base + i * 8",
        "let t = a - b in if t > 0.0 then t else -t",
        "if a > b then (if a > c then a else c) else b",
        "(if a > b then a else b) + a",
        "sqrt(x * x + y * y)",
        "abs(x) + floor(y) + ceil(z)",
        "(n >> 1) % 7 + (n >> 1) / 7",
        "if a > b then (a * c) + (b * c) else a - b",
        "if a > b then (a - b) - (a + b) else c - a",
        "if a > b then fma(a, b, c) else a * c",
        "if x > y then (x * y) + (x - y) else x / y",
        "if x > y then fma(x, y, z) * x else fma(y, x, z) - y",
    ];
    let mut fused_any = 0;
    for src in corpus {
        let (tokens, diags) = forge_syntax::lexer::lex(src);
        assert!(diags.is_empty(), "lex errors for {src:?}: {diags:?}");
        let (ast, diags) = forge_syntax::parser::parse(&tokens);
        assert!(diags.is_empty(), "parse errors for {src:?}: {diags:?}");
        let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
            .unwrap_or_else(|e| panic!("type errors for {src:?}: {e:?}"));
        let func = forge_ir::lower::lower(&typed);

        let (fusions, _) = forge_x64::find_fusable_diamonds(&func);
        if !fusions.is_empty() {
            fused_any += 1;
        }

        let selected = forge_x64::select(&func);
        let intervals = forge_regalloc::build_intervals(&func, &selected);
        let excluded = forge_regalloc::excluded_registers(&func, &selected);
        let (assignment, _bytes) =
            forge_regalloc::allocate(intervals.clone(), &excluded, &selected);

        assert!(
            forge_regalloc::verify_allocation(&intervals, &assignment).is_ok(),
            "{src:?}: fused output must still produce a valid, independently-verified allocation"
        );
    }
    assert!(
        fused_any > 0,
        "corpus must contain at least one fusable diamond, or this test is vacuous -- \
         confirmed by design review that \"(if a > b then a else b) + a\" and the inner \
         diamond of \"if a > b then (if a > c then a else c) else b\" both fuse"
    );
}
