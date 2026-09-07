//! The public source-to-execution pipeline.
//!
//! This crate deliberately keeps the pipeline explicit: diagnostics are
//! returned from the front end, IR is verified around optimization, register
//! allocation is independently checked, and only then is executable memory
//! created.

pub use forge_ir::interp::RtValue;
mod tiered;

#[cfg(target_arch = "aarch64")]
use forge_aarch64::{Assembler as Aarch64Assembler, Gpr as Aarch64Gpr, SP as AARCH64_SP, XZR};
use forge_ir::{Function, Value};
use forge_mem::{CompiledExpr, ExecutableBuffer};
use forge_syntax::Diagnostic;
#[cfg(target_arch = "x86_64")]
use forge_x64::{AluOp, Assembler, PhysReg};
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

/// Executes a typed scalar expression through the native x86-64 emitter when
/// the active ABI can be represented by the runtime trampoline. The packed
/// `u64` argument area is deliberately private: f64 values use their raw
/// bits, i64 values use two's-complement bits, and bool values use 0/1. On a
/// non-x86 host, or for a signature outside the register-only trampoline
/// boundary, the verified interpreter remains the portable fallback.
pub fn evaluate_typed(source: &str, args: &[RtValue]) -> Result<RtValue, CompileError> {
    let function = lower_source(source)?;
    validate_typed_arguments(&function, args)?;

    #[cfg(target_arch = "x86_64")]
    if supports_native_typed_signature(&function) {
        return execute_native_typed(source, args, &function);
    }

    #[cfg(target_arch = "aarch64")]
    if let Some(value) = execute_native_typed_aarch64(args, &function)? {
        return Ok(value);
    }

    Ok(forge_ir::interp::interpret(&function, args))
}

fn value_ty(value: RtValue) -> forge_ir::Ty {
    match value {
        RtValue::F64(_) => forge_ir::Ty::F64,
        RtValue::I64(_) => forge_ir::Ty::I64,
        RtValue::Bool(_) => forge_ir::Ty::Bool,
    }
}

fn validate_typed_arguments(function: &Function, args: &[RtValue]) -> Result<(), CompileError> {
    if args.len() != function.params.len()
        || args
            .iter()
            .zip(function.params.iter())
            .any(|(value, (_, ty))| value_ty(*value) != *ty)
    {
        return Err(CompileError::UnsupportedTarget(
            "typed runtime arguments do not match the function signature",
        ));
    }
    Ok(())
}

#[cfg(target_arch = "x86_64")]
fn supports_native_typed_signature(function: &Function) -> bool {
    if cfg!(windows) {
        // The emitter supports the four register positions followed by the
        // caller-provided stack argument area. Keep the trampoline boundary
        // aligned with the scalar entry point's existing eight-parameter
        // limit; larger signatures remain on the interpreter fallback.
        function.params.len() <= 8
    } else {
        function.params.len() <= 16
    }
}

#[cfg(target_arch = "x86_64")]
fn execute_native_typed(
    source: &str,
    args: &[RtValue],
    function: &Function,
) -> Result<RtValue, CompileError> {
    let artifacts = compile_artifacts(source)?;
    let result_ty = *artifacts
        .function
        .types
        .last()
        .ok_or(CompileError::UnsupportedTarget(
            "function has no result type",
        ))?;
    let mut body = ExecutableBuffer::new(artifacts.bytes.len())?;
    body.write(|dst| dst[..artifacts.bytes.len()].copy_from_slice(&artifacts.bytes));
    body.make_executable()?;

    let mut trampoline = Assembler::new();
    emit_typed_trampoline(
        &mut trampoline,
        body.as_ptr() as usize as i64,
        &function.params,
        result_ty,
    );
    let trampoline_bytes = trampoline.code().to_vec();
    let mut trampoline_buffer = ExecutableBuffer::new(trampoline_bytes.len())?;
    trampoline_buffer.write(|dst| dst[..trampoline_bytes.len()].copy_from_slice(&trampoline_bytes));
    trampoline_buffer.make_executable()?;

    let packed = args
        .iter()
        .map(|value| match value {
            RtValue::F64(value) => value.to_bits(),
            RtValue::I64(value) => *value as u64,
            RtValue::Bool(value) => u64::from(*value),
        })
        .collect::<Vec<_>>();
    // SAFETY: the trampoline has the fixed `extern "C" fn(*const u64) -> u64`
    // ABI, loads exactly one eight-byte word per validated argument, calls
    // the live executable body with its platform-specific scalar ABI, and
    // normalizes an f64 return into RAX before returning.
    let call: unsafe extern "C" fn(*const u64) -> u64 =
        unsafe { std::mem::transmute(trampoline_buffer.as_ptr()) };
    let raw = unsafe { call(packed.as_ptr()) };

    Ok(match result_ty {
        forge_ir::Ty::F64 => RtValue::F64(f64::from_bits(raw)),
        forge_ir::Ty::I64 => RtValue::I64(raw as i64),
        forge_ir::Ty::Bool => {
            if raw > 1 {
                return Err(CompileError::UnsupportedTarget(
                    "native typed bool result was not canonical",
                ));
            }
            RtValue::Bool(raw == 1)
        }
    })
}

#[cfg(target_arch = "x86_64")]
fn emit_typed_trampoline(
    asm: &mut Assembler,
    target: i64,
    params: &[(String, forge_ir::Ty)],
    result_ty: forge_ir::Ty,
) {
    let pointer_arg = if cfg!(windows) {
        PhysReg::Rcx
    } else {
        PhysReg::Rdi
    };
    // R10 is not an incoming scalar argument register on either supported
    // x86-64 ABI and is excluded from ordinary Forge allocation, so it can
    // hold the packed argument pointer while the target registers load.
    asm.mov_reg_reg(PhysReg::R10, pointer_arg);
    let mut integer_ordinal = 0usize;
    let mut float_ordinal = 0usize;
    for (index, (_, ty)) in params.iter().enumerate() {
        let offset = (index * 8) as i32;
        if cfg!(windows) && index >= 4 {
            // Win64 positions five and onward are loaded by the target from
            // the caller's stack argument area below. Leave R10 holding the
            // packed argument pointer until those stores are complete.
            continue;
        }
        if *ty == forge_ir::Ty::F64 {
            if !cfg!(windows) && float_ordinal >= forge_regalloc::SYSV_FLOAT_ARGS.len() {
                float_ordinal += 1;
                continue;
            }
            let dst = if cfg!(windows) {
                [PhysReg::Xmm0, PhysReg::Xmm1, PhysReg::Xmm2, PhysReg::Xmm3][index]
            } else {
                [
                    PhysReg::Xmm0,
                    PhysReg::Xmm1,
                    PhysReg::Xmm2,
                    PhysReg::Xmm3,
                    PhysReg::Xmm4,
                    PhysReg::Xmm5,
                    PhysReg::Xmm6,
                    PhysReg::Xmm7,
                ][float_ordinal]
            };
            asm.movsd_reg_mem(dst, PhysReg::R10, offset);
            float_ordinal += 1;
        } else {
            if !cfg!(windows) && integer_ordinal >= forge_regalloc::SYSV_INT_ARGS.len() {
                integer_ordinal += 1;
                continue;
            }
            let dst = if cfg!(windows) {
                [PhysReg::Rcx, PhysReg::Rdx, PhysReg::R8, PhysReg::R9][index]
            } else {
                [
                    PhysReg::Rdi,
                    PhysReg::Rsi,
                    PhysReg::Rdx,
                    PhysReg::Rcx,
                    PhysReg::R8,
                    PhysReg::R9,
                ][integer_ordinal]
            };
            asm.mov_reg_mem(dst, PhysReg::R10, offset);
            integer_ordinal += 1;
        }
    }

    // The trampoline itself enters with RSP % 16 == 8. Preserve the target's
    // expected entry alignment and reserve Win64 home space plus stack
    // arguments before CALL. The target's framed Win64 parameter loads use
    // [RBP + 48 + (index - 4) * 8], which corresponds to [RSP + 32 + ...]
    // immediately before CALL.
    let sysv_stack_params = if cfg!(windows) {
        Vec::new()
    } else {
        sysv_stack_param_offsets(params)
    };
    let call_stack_bytes = if cfg!(windows) {
        let stack_args = params.len().saturating_sub(4);
        let mut bytes = 32 + stack_args * 8;
        if bytes % 16 != 8 {
            bytes += 8;
        }
        bytes
    } else {
        let mut bytes = sysv_stack_params.len() * 8;
        if bytes % 16 != 8 {
            bytes += 8;
        }
        bytes
    };
    let call_stack_bytes = i32::try_from(call_stack_bytes)
        .expect("typed trampoline stack area is too large for an x86 displacement");
    asm.alu_reg_imm(AluOp::Sub, PhysReg::Rsp, call_stack_bytes);
    if cfg!(windows) {
        for index in 4..params.len() {
            let packed_offset = (index * 8) as i32;
            let stack_offset = 32 + ((index - 4) * 8) as i32;
            asm.mov_reg_mem(PhysReg::R11, PhysReg::R10, packed_offset);
            asm.mov_mem_reg(PhysReg::Rsp, stack_offset, PhysReg::R11);
        }
    } else {
        for (index, stack_ordinal) in sysv_stack_params {
            let packed_offset = (index * 8) as i32;
            let stack_offset = (stack_ordinal * 8) as i32;
            asm.mov_reg_mem(PhysReg::R11, PhysReg::R10, packed_offset);
            asm.mov_mem_reg(PhysReg::Rsp, stack_offset, PhysReg::R11);
        }
    }
    asm.mov_reg_imm(PhysReg::R11, target);
    asm.call_reg(PhysReg::R11);
    asm.alu_reg_imm(AluOp::Add, PhysReg::Rsp, call_stack_bytes);
    if result_ty == forge_ir::Ty::F64 {
        asm.movq_xmm_to_gpr(PhysReg::Rax, PhysReg::Xmm0);
    }
    asm.ret();
}

#[cfg(target_arch = "x86_64")]
fn sysv_stack_param_offsets(params: &[(String, forge_ir::Ty)]) -> Vec<(usize, usize)> {
    let mut integer_ordinal = 0usize;
    let mut float_ordinal = 0usize;
    let mut stack_ordinal = 0usize;
    let mut offsets = Vec::new();
    for (index, (_, ty)) in params.iter().enumerate() {
        let (ordinal, capacity) = if *ty == forge_ir::Ty::F64 {
            let ordinal = float_ordinal;
            float_ordinal += 1;
            (ordinal, forge_regalloc::SYSV_FLOAT_ARGS.len())
        } else {
            let ordinal = integer_ordinal;
            integer_ordinal += 1;
            (ordinal, forge_regalloc::SYSV_INT_ARGS.len())
        };
        if ordinal >= capacity {
            offsets.push((index, stack_ordinal));
            stack_ordinal += 1;
        }
    }
    offsets
}

#[cfg(target_arch = "aarch64")]
fn execute_native_typed_aarch64(
    args: &[RtValue],
    function: &Function,
) -> Result<Option<RtValue>, CompileError> {
    let result_ty = *function
        .types
        .last()
        .ok_or(CompileError::UnsupportedTarget(
            "function has no result type",
        ))?;
    let float_args = function
        .params
        .iter()
        .filter(|(_, ty)| *ty == forge_ir::Ty::F64)
        .count();
    let integer_args = function
        .params
        .iter()
        .filter(|(_, ty)| *ty != forge_ir::Ty::F64)
        .count();
    if float_args > 8 || integer_args > 8 {
        return Ok(None);
    }

    let bytes = match result_ty {
        forge_ir::Ty::F64 => forge_aarch64::emit_f64(function),
        forge_ir::Ty::I64
            if function
                .params
                .iter()
                .all(|(_, ty)| *ty == forge_ir::Ty::I64) =>
        {
            forge_aarch64::emit_i64(function)
        }
        _ => return Ok(None),
    };
    let Ok(bytes) = bytes else {
        return Ok(None);
    };
    let mut body = ExecutableBuffer::new(bytes.len())?;
    body.write(|dst| dst[..bytes.len()].copy_from_slice(&bytes));
    body.make_executable()?;

    let mut trampoline = Aarch64Assembler::new();
    emit_typed_trampoline_aarch64(&mut trampoline, body.as_ptr() as usize, &function.params);
    let trampoline_bytes = trampoline.bytes();
    let mut trampoline_buffer = ExecutableBuffer::new(trampoline_bytes.len())?;
    trampoline_buffer.write(|dst| dst[..trampoline_bytes.len()].copy_from_slice(&trampoline_bytes));
    trampoline_buffer.make_executable()?;

    let packed = args
        .iter()
        .map(|value| match value {
            RtValue::F64(value) => value.to_bits(),
            RtValue::I64(value) => *value as u64,
            RtValue::Bool(value) => u64::from(*value),
        })
        .collect::<Vec<_>>();
    match result_ty {
        forge_ir::Ty::F64 => {
            // SAFETY: the trampoline has the AAPCS64 C ABI for one pointer
            // argument and an f64 result, loads each validated packed value
            // into the matching AAPCS64 bank, and tail-preserves the target's
            // f64 return in D0.
            let call: unsafe extern "C" fn(*const u64) -> f64 =
                unsafe { std::mem::transmute(trampoline_buffer.as_ptr()) };
            Ok(Some(RtValue::F64(unsafe { call(packed.as_ptr()) })))
        }
        forge_ir::Ty::I64 => {
            // SAFETY: the i64 path has the same validated pointer ABI and
            // returns its scalar result in X0 as required by AAPCS64.
            let call: unsafe extern "C" fn(*const u64) -> i64 =
                unsafe { std::mem::transmute(trampoline_buffer.as_ptr()) };
            Ok(Some(RtValue::I64(unsafe { call(packed.as_ptr()) })))
        }
        forge_ir::Ty::Bool => Ok(None),
    }
}

#[cfg(target_arch = "aarch64")]
fn emit_typed_trampoline_aarch64(
    asm: &mut Aarch64Assembler,
    target: usize,
    params: &[(String, forge_ir::Ty)],
) {
    let packed = Aarch64Gpr::new(16);
    let target_reg = Aarch64Gpr::new(17);
    let link = Aarch64Gpr::new(30);
    asm.sub_imm(AARCH64_SP, AARCH64_SP, 16, false);
    asm.str(link, AARCH64_SP, 0);
    asm.orr_reg(packed, Aarch64Gpr::new(0), XZR);

    let mut integer_ordinal = 0u8;
    let mut float_ordinal = 0u8;
    for (index, (_, ty)) in params.iter().enumerate() {
        let offset = u16::try_from(index * 8).expect("packed typed arguments fit AArch64 offset");
        if *ty == forge_ir::Ty::F64 {
            asm.ldr_d(Aarch64Gpr::new_d(float_ordinal), packed, offset);
            float_ordinal += 1;
        } else {
            asm.ldr(Aarch64Gpr::new(integer_ordinal), packed, offset);
            integer_ordinal += 1;
        }
    }

    let target = target as u64;
    asm.movz(target_reg, (target & 0xffff) as u16, 0);
    asm.movk(target_reg, ((target >> 16) & 0xffff) as u16, 16);
    asm.movk(target_reg, ((target >> 32) & 0xffff) as u16, 32);
    asm.movk(target_reg, ((target >> 48) & 0xffff) as u16, 48);
    asm.blr(target_reg);
    asm.ldr(link, AARCH64_SP, 0);
    asm.add_imm(AARCH64_SP, AARCH64_SP, 16, false);
    asm.ret();
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
    fn typed_runtime_executes_mixed_and_non_f64_results() {
        assert_eq!(
            evaluate_typed("x + (n & 1)", &[RtValue::F64(2.5), RtValue::I64(3)],).unwrap(),
            RtValue::F64(3.5)
        );
        assert_eq!(
            evaluate_typed(
                "if flag then x else x + 1.0",
                &[RtValue::Bool(false), RtValue::F64(2.5)],
            )
            .unwrap(),
            RtValue::F64(3.5)
        );
        assert_eq!(
            evaluate_typed("n & 7", &[RtValue::I64(11)]).unwrap(),
            RtValue::I64(3)
        );
        assert_eq!(
            evaluate_typed(
                "left && right",
                &[RtValue::Bool(true), RtValue::Bool(false)]
            )
            .unwrap(),
            RtValue::Bool(false)
        );
    }

    #[test]
    fn typed_runtime_marshals_stack_backed_win64_shape() {
        assert_eq!(
            evaluate_typed(
                "a + (n & 1) + (m & 1) + (k & 1) + (q & 1)",
                &[
                    RtValue::F64(2.5),
                    RtValue::I64(3),
                    RtValue::I64(4),
                    RtValue::I64(5),
                    RtValue::I64(6),
                ],
            )
            .unwrap(),
            RtValue::F64(4.5)
        );
    }

    #[test]
    fn typed_runtime_marshals_stack_backed_sysv_shape() {
        let args = [
            RtValue::I64(1),
            RtValue::I64(2),
            RtValue::I64(3),
            RtValue::I64(4),
            RtValue::I64(5),
            RtValue::I64(6),
            RtValue::I64(7),
        ];
        for target in 0..7 {
            let source = (0..7)
                .map(|index| format!("(p{index} & {})", if index == target { "-1" } else { "0" }))
                .collect::<Vec<_>>()
                .join(" + ");
            assert_eq!(
                evaluate_typed(&source, &args).unwrap(),
                RtValue::I64((target + 1) as i64),
                "failed to marshal parameter {target}"
            );
        }

        let float_args = [
            RtValue::F64(1.0),
            RtValue::F64(2.0),
            RtValue::F64(3.0),
            RtValue::F64(4.0),
            RtValue::F64(5.0),
            RtValue::F64(6.0),
            RtValue::F64(7.0),
            RtValue::F64(8.0),
            RtValue::F64(9.0),
        ];
        assert_eq!(
            evaluate_typed("p0 + p1 + p2 + p3 + p4 + p5 + p6 + p7 + p8", &float_args,).unwrap(),
            RtValue::F64(45.0)
        );
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
