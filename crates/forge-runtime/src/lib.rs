//! The public source-to-execution pipeline.
//!
//! This crate deliberately keeps the pipeline explicit: diagnostics are
//! returned from the front end, IR is verified around optimization, register
//! allocation is independently checked, and only then is executable memory
//! created.

pub use forge_ir::interp::RtValue;
mod tiered;

use forge_ir::{Function, Value};
use forge_mem::{CompiledExpr, ExecutableBuffer};
use forge_syntax::Diagnostic;
use std::collections::HashMap;
pub use tiered::{ExecutionTier, TieredExpr, BASELINE_THRESHOLD, OPTIMIZED_THRESHOLD};

#[derive(Debug)]
pub enum CompileError {
    Lex(Vec<Diagnostic>),
    Parse(Vec<Diagnostic>),
    Type(Vec<Diagnostic>),
    Ir(String),
    Allocation(String),
    UnsupportedTarget(&'static str),
    Memory(std::io::Error),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lex(d) => write!(f, "lexing failed: {d:?}"),
            Self::Parse(d) => write!(f, "parsing failed: {d:?}"),
            Self::Type(d) => write!(f, "type checking failed: {d:?}"),
            Self::Ir(e) => write!(f, "IR verification failed: {e}"),
            Self::Allocation(e) => write!(f, "register allocation verification failed: {e}"),
            Self::UnsupportedTarget(e) => write!(f, "JIT unavailable: {e}"),
            Self::Memory(e) => write!(f, "executable memory allocation failed: {e}"),
        }
    }
}

impl std::error::Error for CompileError {}

/// The inspectable output of the scalar compilation pipeline.
///
/// This is intentionally separate from [`CompiledFunction`]: inspection is
/// useful on hosts that cannot execute x86-64 code (including the project's
/// AArch64 development host), while `CompiledFunction` owns executable memory
/// and is only available when the active target can call the emitted ABI.
pub struct CompilationArtifacts {
    pub function: Function,
    pub selected: forge_x64::SelectedFunction,
    pub intervals: Vec<forge_regalloc::Interval>,
    pub assignment: HashMap<Value, forge_regalloc::Location>,
    pub bytes: Vec<u8>,
}

impl From<std::io::Error> for CompileError {
    fn from(error: std::io::Error) -> Self {
        Self::Memory(error)
    }
}

/// Parses, resolves, type-checks, and lowers one source expression.
pub fn lower_source(source: &str) -> Result<Function, CompileError> {
    let (tokens, lex_diags) = forge_syntax::lexer::lex(source);
    if !lex_diags.is_empty() {
        return Err(CompileError::Lex(lex_diags));
    }
    let (ast, parse_diags) = forge_syntax::parser::parse(&tokens);
    if !parse_diags.is_empty() {
        return Err(CompileError::Parse(parse_diags));
    }
    let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
        .map_err(CompileError::Type)?;
    let function = forge_ir::lower::lower(&typed);
    forge_ir::verify::verify(&function).map_err(CompileError::Ir)?;
    Ok(function)
}

#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
fn function_contains_fma(function: &Function) -> bool {
    function
        .insts
        .iter()
        .any(|inst| matches!(inst, forge_ir::Inst::Fma { .. }))
}

#[cfg(target_arch = "x86_64")]
fn host_supports_scalar_fma() -> bool {
    is_x86_feature_detected!("fma")
}

fn interpret_f64_function(function: &Function, args: &[f64]) -> Result<f64, CompileError> {
    if function.params.len() != args.len()
        || function
            .params
            .iter()
            .any(|(_, ty)| *ty != forge_ir::Ty::F64)
    {
        return Err(CompileError::UnsupportedTarget(
            "portable fallback accepts all-f64 functions only",
        ));
    }
    match forge_ir::interp::interpret(
        function,
        &args.iter().copied().map(RtValue::F64).collect::<Vec<_>>(),
    ) {
        RtValue::F64(value) => Ok(value),
        _ => Err(CompileError::UnsupportedTarget(
            "expression does not return f64",
        )),
    }
}

/// Interprets source using the reference interpreter. This is available on
/// every target and is the portable fallback used by the WASM facade.
pub fn interpret_source(source: &str, args: &[RtValue]) -> Result<RtValue, CompileError> {
    let function = lower_source(source)?;
    Ok(forge_ir::interp::interpret(&function, args))
}

/// Evaluates an all-f64 expression using the native JIT where the active
/// target can execute the x86-64 backend, and the verified interpreter on
/// other hosts. This keeps the public runtime usable on the repository's
/// AArch64 development machines while preserving the native JIT path.
pub fn evaluate(source: &str, args: &[f64]) -> Result<f64, CompileError> {
    #[cfg(target_arch = "x86_64")]
    {
        let function = lower_source(source)?;
        if function_contains_fma(&function) && !host_supports_scalar_fma() {
            // Scalar FMA has observable single-rounding semantics. Running
            // the verified reference interpreter on non-FMA hosts preserves
            // those semantics instead of emitting an illegal FMA3 opcode.
            return interpret_f64_function(&function, args);
        }
        Ok(compile(source)?.call(args))
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let function = lower_source(source)?;
        #[cfg(target_arch = "aarch64")]
        if function.types.last() == Some(&forge_ir::Ty::F64) {
            // Keep the portable interpreter as the fallback for operations
            // that the current AArch64 emitter has not implemented yet (libm
            // calls, integer conversions, and so on). Supported scalar f64
            // expressions execute through the same W^X buffer API used by
            // the x86 runtime.
            if let Ok(bytes) = forge_aarch64::emit_f64(&function) {
                let mut buffer = ExecutableBuffer::new(bytes.len())?;
                buffer.write(|dst| dst[..bytes.len()].copy_from_slice(&bytes));
                buffer.make_executable()?;
                let compiled = CompiledExpr::from_buffer(buffer, args.len());
                return Ok(compiled.call_args(args));
            }
        }
        interpret_f64_function(&function, args)
    }
}

/// A compiled x86-64 scalar expression and its source-level arity.
pub struct CompiledFunction {
    code: CompiledExpr,
    arity: usize,
}

/// Runs the complete scalar x86 pipeline without requiring the active host
/// to be x86-64. The result powers the CLI and workbench inspection surfaces.
pub fn compile_artifacts(source: &str) -> Result<CompilationArtifacts, CompileError> {
    compile_artifacts_with_optimization(source, true)
}

/// Like [`compile_artifacts`], but allows inspection tools to request the
/// unoptimized baseline. `false` means that only frontend lowering and IR
/// verification run; `true` runs the complete current scalar optimization
/// pipeline.
pub fn compile_artifacts_with_optimization(
    source: &str,
    optimize: bool,
) -> Result<CompilationArtifacts, CompileError> {
    let mut function = lower_source(source)?;
    if optimize {
        forge_opt::optimize(&mut function);
    }
    #[cfg(target_arch = "x86_64")]
    if function_contains_fma(&function) && !host_supports_scalar_fma() {
        return Err(CompileError::UnsupportedTarget(
            "scalar FMA requires FMA3 on the native JIT path",
        ));
    }
    forge_ir::verify::verify(&function).map_err(CompileError::Ir)?;
    let selected = forge_x64::select(&function);
    let intervals = forge_regalloc::build_intervals(&function, &selected);
    let excluded = forge_regalloc::excluded_registers(&function, &selected);
    let (assignment, _) = forge_regalloc::allocate(intervals.clone(), &excluded, &selected);
    forge_regalloc::verify_allocation(&intervals, &assignment).map_err(CompileError::Allocation)?;
    let bytes = forge_emit::emit_body(&function, &selected, &assignment);
    Ok(CompilationArtifacts {
        function,
        selected,
        intervals,
        assignment,
        bytes,
    })
}

impl CompiledFunction {
    pub fn arity(&self) -> usize {
        self.arity
    }

    /// Calls the generated `f64 -> f64` ABI entry point. The current public
    /// JIT surface is intentionally restricted to all-f64 functions; mixed
    /// integer/bool execution remains available through `interpret_source`.
    pub fn call(&self, args: &[f64]) -> f64 {
        self.code.call_args(args)
    }
}

/// Runs optimization, selection, allocation, independent allocation
/// verification, and x86-64 emission, then seals the result as executable.
pub fn compile(source: &str) -> Result<CompiledFunction, CompileError> {
    compile_with_optimization(source, true)
}

/// Compiles the scalar entry point without running the optimizer. This is
/// the baseline tier used by [`TieredExpr`].
pub fn compile_baseline(source: &str) -> Result<CompiledFunction, CompileError> {
    compile_with_optimization(source, false)
}

fn compile_with_optimization(
    source: &str,
    optimize: bool,
) -> Result<CompiledFunction, CompileError> {
    if !cfg!(target_arch = "x86_64") {
        return Err(CompileError::UnsupportedTarget(
            "the active backend emits x86-64 machine code",
        ));
    }
    let mut function = lower_source(source)?;
    if function
        .params
        .iter()
        .any(|(_, ty)| *ty != forge_ir::Ty::F64)
        || function
            .types
            .last()
            .is_some_and(|ty| *ty != forge_ir::Ty::F64)
    {
        return Err(CompileError::UnsupportedTarget(
            "the scalar JIT entry point currently accepts and returns only f64",
        ));
    }
    if function.params.len() > 8 {
        return Err(CompileError::UnsupportedTarget(
            "the scalar JIT entry point currently supports at most 8 f64 parameters",
        ));
    }
    if optimize {
        forge_opt::optimize(&mut function);
    }
    forge_ir::verify::verify(&function).map_err(CompileError::Ir)?;
    let selected = forge_x64::select(&function);
    let intervals = forge_regalloc::build_intervals(&function, &selected);
    let excluded = forge_regalloc::excluded_registers(&function, &selected);
    let (assignment, _) = forge_regalloc::allocate(intervals.clone(), &excluded, &selected);
    forge_regalloc::verify_allocation(&intervals, &assignment).map_err(CompileError::Allocation)?;
    let bytes = forge_emit::emit_body(&function, &selected, &assignment);
    let mut buffer = ExecutableBuffer::new(bytes.len())?;
    buffer.write(|dst| dst[..bytes.len()].copy_from_slice(&bytes));
    buffer.make_executable()?;
    let arity = function.params.len();
    Ok(CompiledFunction {
        code: CompiledExpr::from_buffer(buffer, arity),
        arity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_source_reports_frontend_errors() {
        assert!(matches!(lower_source("1 +"), Err(CompileError::Parse(_))));
    }

    #[test]
    fn evaluate_runs_on_the_active_execution_path() {
        assert_eq!(evaluate("x * x + 1", &[3.0]).unwrap(), 10.0);
    }

    #[test]
    fn depth_100_expression_compiles_and_runs() {
        let source = (0..100).fold("x".to_string(), |expression, _| {
            format!("({expression} + 1.0)")
        });
        assert_eq!(evaluate(&source, &[1.0]).unwrap(), 101.0);
    }

    #[test]
    fn artifact_pipeline_can_preserve_unoptimized_ir() {
        let baseline = compile_artifacts_with_optimization("x * 1.0", false).unwrap();
        let optimized = compile_artifacts_with_optimization("x * 1.0", true).unwrap();
        let live_count = |function: &Function| {
            function
                .blocks
                .iter()
                .map(|block| block.insts.len())
                .sum::<usize>()
        };
        assert!(live_count(&optimized.function) < live_count(&baseline.function));
    }

    #[test]
    fn function_contains_fma_only_for_fma_expressions() {
        assert!(!function_contains_fma(&lower_source("x * y + z").unwrap()));
        assert!(function_contains_fma(
            &lower_source("fma(x, y, z)").unwrap()
        ));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn compile_runs_a_scalar_expression() {
        let compiled = compile("x * x + 1").unwrap();
        assert_eq!(compiled.arity(), 1);
        assert_eq!(compiled.call(&[3.0]), 10.0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn evaluate_runs_supported_code_natively_and_keeps_libm_fallback() {
        assert_eq!(
            evaluate("if x > 0.0 then x * 2.0 else -x", &[3.0]).unwrap(),
            6.0
        );
        assert_eq!(
            evaluate("if x > 0.0 then x * 2.0 else -x", &[-3.0]).unwrap(),
            3.0
        );
        assert_eq!(evaluate("sqrt(x * x)", &[3.0]).unwrap(), 3.0);
    }

    #[cfg(all(target_arch = "x86_64", target_os = "windows"))]
    #[test]
    fn win64_jit_executes_register_and_stack_f64_arguments() {
        let five = compile("a + b + c + d + e").unwrap();
        assert_eq!(five.call(&[1.0, 2.0, 4.0, 8.0, 16.0]), 31.0);

        let eight = compile("a + b + c + d + e + f + g + h").unwrap();
        assert_eq!(
            eight.call(&[1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0]),
            255.0
        );
    }

    #[cfg(all(target_arch = "x86_64", target_os = "windows"))]
    #[test]
    fn win64_jit_preserves_live_value_across_libm_call() {
        let compiled = compile("sin(x) + y").unwrap();
        let expected = 0.5f64.sin() + 2.0;
        assert_eq!(compiled.call(&[0.5, 2.0]).to_bits(), expected.to_bits());
    }
}
