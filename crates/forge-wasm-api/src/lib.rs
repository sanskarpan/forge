//! Stable, serialization-friendly API boundary for browser integrations.

use wasm_bindgen::prelude::wasm_bindgen;

pub fn run(source: &str, args: &[f64]) -> Result<f64, String> {
    forge_wasm::evaluate(source, args)
}

pub fn compile(source: &str) -> Result<Vec<u8>, String> {
    forge_wasm::compile(source)
}

pub fn compile_artifact(source: &str) -> Result<forge_wasm::WasmArtifact, String> {
    forge_wasm::compile_artifact(source)
}

pub fn cpu_features() -> &'static str {
    "portable-interpreter"
}

const MAX_BENCHMARK_CALLS: usize = 1_000_000;

/// Benchmarks the portable source evaluator for the requested call counts.
/// The browser workbench separately measures the instantiated WASM export;
/// this API remains useful for hosts that need a deterministic, portable
/// baseline and labels that backend explicitly in its response.
pub fn benchmark_json(source: &str, sizes: &[u32]) -> String {
    let artifact = match compile_artifact(source) {
        Ok(artifact) => artifact,
        Err(error) => return format!(r#"{{"ok":false,"error":{}}}"#, json_string(&error)),
    };
    if artifact.parameter_types.iter().any(|ty| ty != "f64") || artifact.result_type != "f64" {
        return r#"{"ok":false,"error":"benchmark requires an all-f64 expression"}"#.to_string();
    }
    let args = vec![1.25; artifact.parameter_count()];
    let results = sizes
        .iter()
        .map(|requested| {
            let calls = (*requested as usize).min(MAX_BENCHMARK_CALLS);
            let started = now_millis();
            let mut last = None;
            for _ in 0..calls {
                match run(source, &args) {
                    Ok(value) => last = Some(value),
                    Err(error) => {
                        return format!(
                            r#"{{"size":{},"error":{}}}"#,
                            requested,
                            json_string(&error)
                        )
                    }
                }
            }
            let elapsed_ms = now_millis() - started;
            let result = last.map_or_else(
                || "null".to_string(),
                |value| {
                    if value.is_finite() {
                        value.to_string()
                    } else {
                        "null".to_string()
                    }
                },
            );
            format!(
                r#"{{"size":{},"calls":{},"elapsed_ms":{},"last_result":{}}}"#,
                requested, calls, elapsed_ms, result
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"ok":true,"backend":"portable-interpreter","results":[{}]}}"#,
        results
    )
}

fn now_millis() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::sync::OnceLock;
        use std::time::Instant;

        static EPOCH: OnceLock<Instant> = OnceLock::new();
        EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64() * 1_000.0
    }
}

/// Browser-facing error boundary for `run`. The Rust API above remains useful
/// to native callers; these wrappers translate errors to JavaScript values.
#[wasm_bindgen]
pub fn run_wasm(source: &str, args: &[f64]) -> Result<f64, wasm_bindgen::JsValue> {
    run(source, args).map_err(|error| wasm_bindgen::JsValue::from_str(&error))
}

#[wasm_bindgen]
pub fn compile_wasm(source: &str) -> Result<Vec<u8>, wasm_bindgen::JsValue> {
    compile(source).map_err(|error| wasm_bindgen::JsValue::from_str(&error))
}

/// Returns the browser-friendly structured artifact boundary. JSON is used
/// instead of a wasm-bindgen struct so this API remains stable for plain JS,
/// TypeScript, and non-wasm native integration tests alike.
#[wasm_bindgen]
pub fn compile_artifact_json(source: &str) -> String {
    match compile_artifact(source) {
        Ok(artifact) => {
            let analysis = match analysis_json(source) {
                Ok(analysis) => analysis,
                Err(error) => return format!(r#"{{"ok":false,"error":{}}}"#, json_string(&error)),
            };
            let params = artifact
                .parameter_types
                .iter()
                .map(|ty| json_string(ty))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                r#"{{"ok":true,"parameter_types":[{params}],"result_type":{},"wasm_bytes_hex":{},"wasm_bytes_len":{},{} }}"#,
                json_string(&artifact.result_type),
                json_string(&artifact.wasm_hex),
                artifact.wasm_bytes.len(),
                analysis
            )
        }
        Err(error) => format!(r#"{{"ok":false,"error":{}}}"#, json_string(&error)),
    }
}

/// Produces the target-independent analysis fields used by the workbench.
/// WASM is a stack machine, so register intervals and native assembly are
/// represented as empty arrays with an explicit encoding marker; the lowered
/// and optimized IR plus CFG remain real artifacts from the compiler itself.
fn analysis_json(source: &str) -> Result<String, String> {
    let (tokens, lex_diags) = forge_syntax::lexer::lex(source);
    if !lex_diags.is_empty() {
        return Err(format!("lexing failed: {lex_diags:?}"));
    }
    let (ast, parse_diags) = forge_syntax::parser::parse(&tokens);
    if !parse_diags.is_empty() {
        return Err(format!("parsing failed: {parse_diags:?}"));
    }
    let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
        .map_err(|diags| format!("type checking failed: {diags:?}"))?;
    let lowered = forge_ir::lower::lower(&typed);
    forge_ir::verify::verify(&lowered)
        .map_err(|error| format!("IR verification failed: {error}"))?;
    let lowered_text = forge_ir::print::print_function(&lowered);
    let mut optimized = forge_ir::lower::lower(&typed);
    forge_opt::optimize(&mut optimized);
    forge_ir::verify::verify(&optimized)
        .map_err(|error| format!("optimized IR verification failed: {error}"))?;
    let optimized_text = forge_ir::print::print_function(&optimized);
    let ir_stages = format!(
        "[{{\"name\":\"lowered\",\"text\":{}}},{{\"name\":\"optimized\",\"text\":{}}}]",
        json_string(&lowered_text),
        json_string(&optimized_text)
    );
    let cfg = cfg_dot(&optimized);
    Ok(format!(
        r#""ir_stages":{ir_stages},"cfg":{},"intervals":[],"asm":[],"encoding":"wasm-stack""#,
        json_string(&cfg)
    ))
}

fn cfg_dot(function: &forge_ir::Function) -> String {
    use std::fmt::Write;

    let mut dot = String::from("digraph forge_cfg {\n");
    for (index, block) in function.blocks.iter().enumerate() {
        writeln!(
            dot,
            "  block{index} [label=\"block{index}\\n{} instructions\"];",
            block.insts.len()
        )
        .expect("writing to a String cannot fail");
        match &block.term {
            Some(forge_ir::Terminator::Jump(target)) => {
                writeln!(dot, "  block{index} -> block{};", target.0)
                    .expect("writing to a String cannot fail");
            }
            Some(forge_ir::Terminator::Branch { then_, else_, .. }) => {
                writeln!(dot, "  block{index} -> block{} [label=\"then\"];", then_.0)
                    .expect("writing to a String cannot fail");
                writeln!(dot, "  block{index} -> block{} [label=\"else\"];", else_.0)
                    .expect("writing to a String cannot fail");
            }
            Some(forge_ir::Terminator::Return(_)) | None => {}
        }
    }
    dot.push('}');
    dot
}

fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", character as u32))
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

#[wasm_bindgen]
pub fn cpu_features_wasm() -> String {
    cpu_features().to_string()
}

#[wasm_bindgen]
pub fn benchmark(source: &str, sizes: &[u32]) -> String {
    benchmark_json(source, sizes)
}

/// Returns a compact JSON-shaped status string without requiring serde in the
/// WASM bundle. The full structured artifact API is a separate workbench
/// boundary; this function gives editors an immediate diagnostics hook.
#[wasm_bindgen]
pub fn parse_and_check(source: &str) -> String {
    let (tokens, lex_diags) = forge_syntax::lexer::lex(source);
    if !lex_diags.is_empty() {
        return diagnostic_report("lex", &lex_diags);
    }
    let (ast, parse_diags) = forge_syntax::parser::parse(&tokens);
    if !parse_diags.is_empty() {
        return diagnostic_report("parse", &parse_diags);
    }
    let typed = match forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast)) {
        Ok(typed) => typed,
        Err(diags) => return diagnostic_report("type", &diags),
    };
    let root = ast_json(&typed.ast, typed.ast.root);
    let params = typed
        .params
        .iter()
        .map(|(name, ty)| {
            format!(
                "{{\"name\":{},\"type\":{}}}",
                json_string(name),
                json_string(&format!("{ty:?}").to_lowercase())
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"ok":true,"stage":"checked","ast":{},"parameters":[{}],"result_type":{}}}"#,
        root,
        params,
        json_string(&format!("{:?}", typed.types[typed.ast.root.index()]).to_lowercase())
    )
}

fn diagnostic_report(stage: &str, diagnostics: &[forge_syntax::Diagnostic]) -> String {
    let diagnostics = diagnostics
        .iter()
        .map(diagnostic_json)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"ok":false,"stage":{},"diagnostics":[{}]}}"#,
        json_string(stage),
        diagnostics
    )
}

fn diagnostic_json(diagnostic: &forge_syntax::Diagnostic) -> String {
    let secondary = diagnostic
        .secondary
        .iter()
        .map(label_json)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"message":{},"primary":{},"secondary":[{}]}}"#,
        json_string(&diagnostic.message),
        label_json(&diagnostic.primary),
        secondary
    )
}

fn label_json(label: &forge_syntax::diagnostic::Label) -> String {
    format!(
        r#"{{"start":{},"end":{},"message":{}}}"#,
        label.span.start,
        label.span.end,
        json_string(&label.message)
    )
}

fn ast_json(ast: &forge_syntax::ast::Ast, idx: forge_syntax::ast::ExprIdx) -> String {
    use forge_syntax::ast::Expr;

    let span = ast.span(idx);
    let span_json = format!(r#""span":{{"start":{},"end":{}}}"#, span.start, span.end);
    match ast.get(idx) {
        Expr::Float(value) => format!(r#"{{"kind":"float","value":{},{} }}"#, value, span_json),
        Expr::Int(value) => format!(r#"{{"kind":"int","value":{},{} }}"#, value, span_json),
        Expr::Bool(value) => format!(r#"{{"kind":"bool","value":{},{} }}"#, value, span_json),
        Expr::Ident(name) => format!(
            r#"{{"kind":"ident","name":{},{} }}"#,
            json_string(name),
            span_json
        ),
        Expr::Unary { op, operand } => format!(
            r#"{{"kind":"unary","op":{},"operand":{},{} }}"#,
            json_string(&format!("{op:?}").to_lowercase()),
            ast_json(ast, *operand),
            span_json
        ),
        Expr::Binary { op, lhs, rhs } => format!(
            r#"{{"kind":"binary","op":{},"lhs":{},"rhs":{},{} }}"#,
            json_string(&format!("{op:?}").to_lowercase()),
            ast_json(ast, *lhs),
            ast_json(ast, *rhs),
            span_json
        ),
        Expr::Call { callee, args } => format!(
            r#"{{"kind":"call","callee":{},"args":[{}],{} }}"#,
            json_string(callee),
            args.iter()
                .map(|arg| ast_json(ast, *arg))
                .collect::<Vec<_>>()
                .join(","),
            span_json
        ),
        Expr::If { cond, then_, else_ } => format!(
            r#"{{"kind":"if","cond":{},"then":{},"else":{},{} }}"#,
            ast_json(ast, *cond),
            ast_json(ast, *then_),
            ast_json(ast, *else_),
            span_json
        ),
        Expr::Let { name, value, body } => format!(
            r#"{{"kind":"let","name":{},"value":{},"body":{},{} }}"#,
            json_string(name),
            ast_json(ast, *value),
            ast_json(ast, *body),
            span_json
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_status_reports_success_and_diagnostics() {
        let success = parse_and_check("x + 1.0");
        assert!(success.contains(r#""ok":true"#));
        assert!(success.contains(r#""kind":"binary""#));
        assert!(success.contains(r#""start":0,"end":7"#));
        let error = parse_and_check("x +");
        assert!(error.contains(r#""ok":false"#));
        assert!(error.contains(r#""stage":"parse""#));
        assert!(error.contains(r#""diagnostics":["#));
    }

    #[test]
    fn browser_artifact_reports_signature_and_bytes() {
        let artifact = compile_artifact_json("x + y");
        assert!(artifact.contains(r#""ok":true"#));
        assert!(artifact.contains(r#""parameter_types":["f64","f64"]"#));
        assert!(artifact.contains(r#""wasm_bytes_len":"#));
        assert!(artifact.contains(r#""ir_stages":["#));
        assert!(artifact.contains(r#""cfg":"digraph forge_cfg"#));
        assert!(artifact.contains(r#""intervals":[]"#));
        assert!(artifact.contains(r#""encoding":"wasm-stack""#));

        let error = compile_artifact_json("x +");
        assert!(error.contains(r#""ok":false"#));
        assert!(error.contains(r#""error":"#));
    }

    #[test]
    fn browser_benchmark_reports_a_labeled_portable_baseline() {
        let report = benchmark_json("x + 1.0", &[0, 2]);
        assert!(report.contains(r#""ok":true"#));
        assert!(report.contains(r#""backend":"portable-interpreter"#));
        assert!(report.contains(r#""size":0,"calls":0"#));
        assert!(report.contains(r#""size":2,"calls":2"#));
    }
}
