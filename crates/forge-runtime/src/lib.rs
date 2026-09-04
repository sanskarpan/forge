//! The public source-to-execution pipeline.
//!
//! This crate deliberately keeps the pipeline explicit: diagnostics are
//! returned from the front end, IR is verified around optimization, register
//! allocation is independently checked, and only then is executable memory
//! created.

pub use forge_ir::interp::RtValue;
use forge_ir::{Function, Value};
use forge_mem::{CompiledExpr, ExecutableBuffer};
use forge_syntax::Diagnostic;
use std::collections::HashMap;

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
    if cfg!(target_arch = "x86_64") {
        return Ok(compile(source)?.call(args));
    }
    let function = lower_source(source)?;
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
        &function,
        &args.iter().copied().map(RtValue::F64).collect::<Vec<_>>(),
    ) {
        RtValue::F64(value) => Ok(value),
        _ => Err(CompileError::UnsupportedTarget(
            "expression does not return f64",
        )),
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
        self.code.call_n(args)
    }
}

/// Runs optimization, selection, allocation, independent allocation
/// verification, and x86-64 emission, then seals the result as executable.
pub fn compile(source: &str) -> Result<CompiledFunction, CompileError> {
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
    forge_opt::optimize(&mut function);
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

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn compile_runs_a_scalar_expression() {
        let compiled = compile("x * x + 1").unwrap();
        assert_eq!(compiled.arity(), 1);
        assert_eq!(compiled.call(&[3.0]), 10.0);
    }
}
