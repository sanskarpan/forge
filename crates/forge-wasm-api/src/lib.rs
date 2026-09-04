//! Stable, serialization-friendly API boundary for browser integrations.

pub fn run(source: &str, args: &[f64]) -> Result<f64, String> {
    forge_wasm::evaluate(source, args)
}

pub fn compile(source: &str) -> Result<Vec<u8>, String> {
    forge_wasm::compile(source)
}

pub fn cpu_features() -> &'static str {
    "portable-interpreter"
}
