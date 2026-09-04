//! Lightweight benchmark primitive used by the CLI and integration tests.

use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct BenchmarkResult {
    pub iterations: u64,
    pub elapsed: Duration,
    pub last_value: f64,
}

pub fn run(source: &str, args: &[f64], iterations: u64) -> Result<BenchmarkResult, String> {
    let compiled = forge_runtime::compile(source).map_err(|e| e.to_string())?;
    let start = Instant::now();
    let mut last_value = 0.0;
    for _ in 0..iterations {
        last_value = compiled.call(args);
        std::hint::black_box(last_value);
    }
    Ok(BenchmarkResult {
        iterations,
        elapsed: start.elapsed(),
        last_value,
    })
}
