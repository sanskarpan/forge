//! Stable, serialization-friendly API boundary for browser integrations.

use wasm_bindgen::prelude::wasm_bindgen;

pub fn run(source: &str, args: &[f64]) -> Result<f64, String> {
    forge_wasm::evaluate(source, args)
}

pub fn compile(source: &str) -> Result<Vec<u8>, String> {
    forge_wasm::compile(source)
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
}
