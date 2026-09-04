//! Thread-safe tier promotion for scalar expressions.

use super::{compile, compile_baseline, evaluate, CompileError};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

pub const BASELINE_THRESHOLD: u64 = 10;
pub const OPTIMIZED_THRESHOLD: u64 = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ExecutionTier {
    Interpreter = 0,
    Baseline = 1,
    Optimized = 2,
}

impl ExecutionTier {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Baseline,
            2 => Self::Optimized,
            _ => Self::Interpreter,
        }
    }
}

/// An expression that promotes from the portable interpreter to cached JIT
/// tiers as it becomes hot. Promotion markers use `OnceLock`, so concurrent
/// callers compile each tier at most once. On non-x86 hosts the promotion
/// state still advances, but evaluation remains on the verified interpreter.
pub struct TieredExpr {
    source: String,
    invocations: AtomicU64,
    tier: AtomicU8,
    baseline_once: OnceLock<()>,
    optimized_once: OnceLock<()>,
    baseline: Mutex<Option<super::CompiledFunction>>,
    optimized: Mutex<Option<super::CompiledFunction>>,
    baseline_compile_ns: AtomicU64,
    optimized_compile_ns: AtomicU64,
}

impl TieredExpr {
    pub fn new(source: impl Into<String>) -> Result<Self, CompileError> {
        let source = source.into();
        super::lower_source(&source)?;
        Ok(Self {
            source,
            invocations: AtomicU64::new(0),
            tier: AtomicU8::new(ExecutionTier::Interpreter as u8),
            baseline_once: OnceLock::new(),
            optimized_once: OnceLock::new(),
            baseline: Mutex::new(None),
            optimized: Mutex::new(None),
            baseline_compile_ns: AtomicU64::new(0),
            optimized_compile_ns: AtomicU64::new(0),
        })
    }

    pub fn invocations(&self) -> u64 {
        self.invocations.load(Ordering::Relaxed)
    }

    pub fn tier(&self) -> ExecutionTier {
        ExecutionTier::from_u8(self.tier.load(Ordering::Acquire))
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn compile_time_ns(&self, tier: ExecutionTier) -> u64 {
        match tier {
            ExecutionTier::Baseline => self.baseline_compile_ns.load(Ordering::Acquire),
            ExecutionTier::Optimized => self.optimized_compile_ns.load(Ordering::Acquire),
            ExecutionTier::Interpreter => 0,
        }
    }

    /// Returns the current JIT-vs-interpreter break-even estimate when timing
    /// data is available. It is a conservative diagnostic only: the runtime
    /// does not assume a stable per-call cost across hosts.
    pub fn break_even_calls(&self, interpreter_ns: u64, compiled_ns: u64) -> Option<u64> {
        let compile_ns = self.compile_time_ns(ExecutionTier::Optimized);
        (compiled_ns < interpreter_ns && compile_ns > 0)
            .then(|| compile_ns.div_ceil(interpreter_ns - compiled_ns))
    }

    pub fn eval(&self, args: &[f64]) -> Result<f64, CompileError> {
        let count = self.invocations.fetch_add(1, Ordering::Relaxed) + 1;
        if count >= OPTIMIZED_THRESHOLD {
            self.ensure_optimized();
            self.tier
                .store(ExecutionTier::Optimized as u8, Ordering::Release);
            if let Some(compiled) = self
                .optimized
                .lock()
                .expect("optimized tier poisoned")
                .as_ref()
            {
                return Ok(compiled.call(args));
            }
        } else if count >= BASELINE_THRESHOLD {
            self.ensure_baseline();
            self.tier
                .store(ExecutionTier::Baseline as u8, Ordering::Release);
            if let Some(compiled) = self
                .baseline
                .lock()
                .expect("baseline tier poisoned")
                .as_ref()
            {
                return Ok(compiled.call(args));
            }
        }
        evaluate(&self.source, args)
    }

    fn ensure_baseline(&self) {
        self.baseline_once.get_or_init(|| {
            let start = Instant::now();
            let compiled = compile_baseline(&self.source).ok();
            self.baseline_compile_ns.store(
                start.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                Ordering::Release,
            );
            *self.baseline.lock().expect("baseline tier poisoned") = compiled;
        });
    }

    fn ensure_optimized(&self) {
        self.optimized_once.get_or_init(|| {
            let start = Instant::now();
            let compiled = compile(&self.source).ok();
            self.optimized_compile_ns.store(
                start.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                Ordering::Release,
            );
            *self.optimized.lock().expect("optimized tier poisoned") = compiled;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn promotes_at_documented_thresholds() {
        let expression = TieredExpr::new("x * x + 1").unwrap();
        for _ in 0..BASELINE_THRESHOLD {
            assert_eq!(expression.eval(&[3.0]).unwrap(), 10.0);
        }
        assert_eq!(expression.tier(), ExecutionTier::Baseline);
        for _ in BASELINE_THRESHOLD..OPTIMIZED_THRESHOLD {
            expression.eval(&[3.0]).unwrap();
        }
        assert_eq!(expression.tier(), ExecutionTier::Optimized);
        assert_eq!(expression.invocations(), OPTIMIZED_THRESHOLD);
    }

    #[test]
    fn rejects_invalid_source_before_first_call() {
        assert!(TieredExpr::new("x +").is_err());
    }
}
