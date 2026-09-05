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

/// Returns a compact JSON-shaped status string without requiring serde in the
/// WASM bundle. The full structured artifact API is a separate workbench
/// boundary; this function gives editors an immediate diagnostics hook.
#[wasm_bindgen]
pub fn parse_and_check(source: &str) -> String {
    match forge_wasm::compile(source) {
        Ok(_) => r#"{"ok":true}"#.to_string(),
        Err(error) => format!(r#"{{"ok":false,"error":{error:?}}}"#),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_status_reports_success_and_diagnostics() {
        assert_eq!(parse_and_check("x + 1.0"), r#"{"ok":true}"#);
        assert!(parse_and_check("x +").contains(r#""ok":false"#));
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
}
