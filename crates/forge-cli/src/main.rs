//! Small, dependency-free command line front end for the compiler pipeline.
//!
//! The command grammar intentionally stays shell-friendly. Machine-readable
//! inspection commands print stable sections, while errors go to stderr and
//! use a non-zero exit status.

use clap::{Args, Parser, Subcommand};
use std::collections::HashMap;
use std::env;
use std::io::{self, BufRead, Write};
use std::time::Instant;

#[derive(Debug, Parser)]
#[command(
    name = "forge-cli",
    version,
    about = "Inspect and run Forge expressions"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Evaluate an expression. Positional values follow parameter order;
    /// named values use --name VALUE or --name=VALUE.
    Eval(EvalArgs),
    /// Compile an expression for an inspection target.
    Compile(CompileArgs),
    /// Print selected instructions and encoded bytes.
    Asm(SourceArgs),
    /// Print textual SSA IR.
    Ir(IrArgs),
    /// Print the control-flow graph.
    Cfg(CfgArgs),
    /// Print live intervals and physical assignments.
    Regalloc(SourceArgs),
    /// Benchmark repeated expression evaluation.
    Bench(BenchArgs),
    /// Compare the interpreter and compiled evaluator.
    Verify(VerifyArgs),
    /// Print detected host features.
    Cpuinfo,
    /// Start the interactive evaluator.
    Repl,
}

#[derive(Debug, Args)]
struct SourceArgs {
    expression: String,
}

#[derive(Debug, Args)]
struct EvalArgs {
    expression: String,
    /// Numeric positional values and dynamic --name VALUE bindings.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    values: Vec<String>,
}

#[derive(Debug, Args)]
struct CompileArgs {
    expression: String,
    #[arg(long, default_value = "x86_64")]
    arch: String,
    #[arg(long, default_value = "2")]
    opt: String,
    #[arg(long)]
    features: Option<String>,
}

#[derive(Debug, Args)]
struct IrArgs {
    expression: String,
    #[arg(long)]
    after: Option<String>,
}

#[derive(Debug, Args)]
struct CfgArgs {
    expression: String,
    #[arg(long)]
    dot: bool,
}

#[derive(Debug, Args)]
struct BenchArgs {
    expression: String,
    #[arg(long, default_value = "1,10,100,1K")]
    sizes: String,
}

#[derive(Debug, Args)]
struct VerifyArgs {
    expression: String,
    #[arg(long, default_value_t = 1000)]
    iters: usize,
}

fn main() {
    let cli = Cli::parse();
    let result = match &cli.command {
        Command::Eval(args) => eval_command(&args.expression, &args.values),
        Command::Compile(args) => compile_command(
            &args.expression,
            &args.arch,
            &args.opt,
            args.features.as_deref(),
        ),
        Command::Asm(args) => inspect_command(&args.expression, Inspection::Assembly),
        Command::Ir(args) => ir_command(&args.expression, args.after.as_deref()),
        Command::Cfg(args) => inspect_command_with_dot(&args.expression, args.dot),
        Command::Regalloc(args) => inspect_command(&args.expression, Inspection::Regalloc),
        Command::Bench(args) => bench_command(&args.expression, &args.sizes),
        Command::Verify(args) => verify_command(&args.expression, args.iters),
        Command::Cpuinfo => cpuinfo_command(&[]),
        Command::Repl => repl_command(),
    };

    if let Err(error) = result {
        eprintln!("forge-cli: {error}");
        std::process::exit(exit_code(&cli.command, &error));
    }
}

fn exit_code(command: &Command, error: &str) -> i32 {
    if matches!(command, Command::Verify(_)) && error.starts_with("mismatch") {
        return 3;
    }
    if is_compile_error(error) {
        2
    } else {
        1
    }
}

fn is_compile_error(error: &str) -> bool {
    [
        "lexing failed",
        "parsing failed",
        "type checking failed",
        "IR verification failed",
        "register allocation verification failed",
        "JIT unavailable",
        "executable memory allocation failed",
        "unknown architecture",
        "--opt must be",
        "WASM backend",
        "AArch64 currently",
    ]
    .iter()
    .any(|prefix| error.starts_with(prefix))
}

fn eval_command(expression: &str, args: &[String]) -> Result<(), String> {
    let function = forge_runtime::lower_source(expression).map_err(|e| e.to_string())?;
    let values = parse_values(args, &function.params)?;
    let result = forge_runtime::evaluate(expression, &values).map_err(|e| e.to_string())?;
    println!("{result}");
    Ok(())
}

fn parse_values(args: &[String], params: &[(String, forge_ir::Ty)]) -> Result<Vec<f64>, String> {
    let mut positional = Vec::new();
    let mut named = HashMap::new();
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if let Some(name) = argument.strip_prefix("--") {
            if name.is_empty() {
                return Err("empty named argument".to_string());
            }
            let (name, value) = if let Some((name, value)) = name.split_once('=') {
                (name.to_string(), value.to_string())
            } else {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| format!("missing value for --{name}"))?
                    .clone();
                (name.to_string(), value)
            };
            let value = value
                .parse::<f64>()
                .map_err(|e| format!("invalid value for --{name}: {e}"))?;
            named.insert(name, value);
        } else {
            positional.push(
                argument
                    .parse::<f64>()
                    .map_err(|e| format!("invalid numeric argument {argument:?}: {e}"))?,
            );
        }
        index += 1;
    }

    if params.iter().any(|(_, ty)| *ty != forge_ir::Ty::F64) {
        return Err("eval currently accepts f64 parameters only".to_string());
    }
    if positional.len() > params.len() {
        return Err(format!(
            "expected at most {} arguments, got {}",
            params.len(),
            positional.len()
        ));
    }
    let mut values = Vec::with_capacity(params.len());
    for (index, (name, _)) in params.iter().enumerate() {
        if let Some(value) = named.get(name) {
            values.push(*value);
        } else if let Some(value) = positional.get(index) {
            values.push(*value);
        } else {
            return Err(format!("missing value for parameter {name:?}"));
        }
    }
    for name in named.keys() {
        if !params.iter().any(|(parameter, _)| parameter == name) {
            return Err(format!("unknown parameter --{name}"));
        }
    }
    Ok(values)
}

fn compile_command(
    expression: &str,
    arch: &str,
    opt: &str,
    features: Option<&str>,
) -> Result<(), String> {
    if !matches!(opt, "0" | "1" | "2") {
        return Err(format!("--opt must be 0, 1, or 2, got {opt:?}"));
    }
    if let Some(features) = features {
        println!("features: {features}");
    }

    match arch {
        "wasm" => {
            let bytes = forge_wasm::compile(expression)?;
            println!("target: wasm\nbytes: {}\nhex: {}", bytes.len(), hex(&bytes));
        }
        "x86_64" => {
            let artifacts =
                forge_runtime::compile_artifacts_with_optimization(expression, opt != "0")
                    .map_err(|e| e.to_string())?;
            println!(
                "target: x86_64\noptimization: {opt}\nbytes: {}\nhex: {}",
                artifacts.bytes.len(),
                hex(&artifacts.bytes)
            );
        }
        "aarch64" => {
            return Err(
                "AArch64 currently provides the tested scalar encoder only; expression selection and emission are not implemented"
                    .to_string(),
            );
        }
        other => return Err(format!("unknown architecture {other:?}")),
    }
    Ok(())
}

enum Inspection {
    Assembly,
    Regalloc,
}

fn inspect_command(expression: &str, inspection: Inspection) -> Result<(), String> {
    let artifacts = forge_runtime::compile_artifacts(expression).map_err(|e| e.to_string())?;
    match inspection {
        Inspection::Assembly => print_assembly(&artifacts),
        Inspection::Regalloc => print_regalloc(&artifacts),
    }
    Ok(())
}

fn inspect_command_with_dot(expression: &str, dot: bool) -> Result<(), String> {
    let artifacts = forge_runtime::compile_artifacts(expression).map_err(|e| e.to_string())?;
    print_cfg(&artifacts.function, dot);
    Ok(())
}

fn ir_command(expression: &str, after: Option<&str>) -> Result<(), String> {
    let mut function = forge_runtime::lower_source(expression).map_err(|e| e.to_string())?;
    if after.is_some_and(|pass| pass != "none" && pass != "lower") {
        forge_opt::optimize(&mut function);
    }
    print!("{}", forge_ir::print::print_function(&function));
    Ok(())
}

fn print_assembly(artifacts: &forge_runtime::CompilationArtifacts) {
    println!("; selected x86-64 instructions");
    for (index, instruction) in artifacts.selected.insts.iter().enumerate() {
        println!("{index:04}: {instruction:?}");
    }
    println!("; encoded bytes ({})", artifacts.bytes.len());
    println!("{}", hex(&artifacts.bytes));
}

fn print_cfg(function: &forge_ir::Function, dot: bool) {
    if dot {
        println!("digraph forge_cfg {{");
        for (index, block) in function.blocks.iter().enumerate() {
            println!(
                "  block{index} [label=\"block{index}\\n{} instructions\"];",
                block.insts.len()
            );
            match &block.term {
                Some(forge_ir::Terminator::Jump(target)) => {
                    println!("  block{index} -> block{};", target.0)
                }
                Some(forge_ir::Terminator::Branch { then_, else_, .. }) => {
                    println!("  block{index} -> block{} [label=\"then\"];", then_.0);
                    println!("  block{index} -> block{} [label=\"else\"];", else_.0);
                }
                Some(forge_ir::Terminator::Return(_)) | None => {}
            }
        }
        println!("}}");
    } else {
        for (index, block) in function.blocks.iter().enumerate() {
            let successors = match &block.term {
                Some(forge_ir::Terminator::Jump(target)) => format!("block{}", target.0),
                Some(forge_ir::Terminator::Branch { then_, else_, .. }) => {
                    format!("block{}, block{}", then_.0, else_.0)
                }
                Some(forge_ir::Terminator::Return(_)) => "return".to_string(),
                None => "unterminated".to_string(),
            };
            println!(
                "block{index}: {} instructions -> {successors}",
                block.insts.len()
            );
        }
    }
}

fn print_regalloc(artifacts: &forge_runtime::CompilationArtifacts) {
    println!("value  range   class  location");
    for interval in &artifacts.intervals {
        let location = artifacts
            .assignment
            .get(&interval.value)
            .map_or("<missing>".to_string(), |location| format!("{location:?}"));
        println!(
            "v{}    {}..{}  {:?}  {}",
            interval.value.0, interval.start, interval.end, interval.reg_class, location
        );
    }
    let spills = artifacts
        .assignment
        .values()
        .filter(|location| matches!(location, forge_regalloc::Location::Spill(_)))
        .count();
    println!("spills: {spills}");
}

fn bench_command(expression: &str, sizes: &str) -> Result<(), String> {
    let function = forge_runtime::lower_source(expression).map_err(|e| e.to_string())?;
    if function
        .params
        .iter()
        .any(|(_, ty)| *ty != forge_ir::Ty::F64)
    {
        return Err("bench currently accepts f64 parameters only".to_string());
    }
    let values = vec![1.25; function.params.len()];
    println!("size\ttotal_us\tper_eval_ns");
    for size in sizes
        .split(',')
        .map(parse_size)
        .collect::<Result<Vec<_>, _>>()?
    {
        let start = Instant::now();
        for _ in 0..size {
            std::hint::black_box(
                forge_runtime::evaluate(expression, &values).map_err(|e| e.to_string())?,
            );
        }
        let elapsed = start.elapsed();
        let nanos = elapsed.as_nanos() as f64 / size as f64;
        println!(
            "{size}\t{:.3}\t{nanos:.1}",
            elapsed.as_secs_f64() * 1_000_000.0
        );
    }
    Ok(())
}

fn verify_command(expression: &str, iterations: usize) -> Result<(), String> {
    let function = forge_runtime::lower_source(expression).map_err(|e| e.to_string())?;
    if function
        .params
        .iter()
        .any(|(_, ty)| *ty != forge_ir::Ty::F64)
        || function.types.last() != Some(&forge_ir::Ty::F64)
    {
        return Err("verify currently accepts f64 parameters and result only".to_string());
    }
    for iteration in 0..iterations {
        let values = (0..function.params.len())
            .map(|index| ((iteration as f64 + 1.0) * (index as f64 + 1.0)).sin())
            .collect::<Vec<_>>();
        let interpreted = forge_runtime::interpret_source(
            expression,
            &values
                .iter()
                .copied()
                .map(forge_runtime::RtValue::F64)
                .collect::<Vec<_>>(),
        )
        .map_err(|e| e.to_string())?;
        let compiled = forge_runtime::evaluate(expression, &values).map_err(|e| e.to_string())?;
        let forge_runtime::RtValue::F64(interpreted) = interpreted else {
            return Err("interpreter returned a non-f64 result".to_string());
        };
        if interpreted.to_bits() != compiled.to_bits() {
            return Err(format!(
                "mismatch at iteration {iteration}: interpreter={interpreted:?}, compiled={compiled:?}"
            ));
        }
    }
    println!("ok ({iterations} cases)");
    Ok(())
}

fn cpuinfo_command(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("cpuinfo takes no arguments".to_string());
    }
    println!("target: {}", env::consts::ARCH);
    println!("os: {}", env::consts::OS);
    let features = forge_simd::CpuFeatures::detect();
    println!("simd_width: {:?}", forge_simd::best_width());
    println!("simd_available: {}", forge_simd::host_supports_simd());
    println!("sse2: {}", features.sse2);
    println!("sse41: {}", features.sse41);
    println!("avx: {}", features.avx);
    println!("avx2: {}", features.avx2);
    println!("fma: {}", features.fma);
    println!("avx512f: {}", features.avx512f);
    println!("avx512dq: {}", features.avx512dq);
    println!("bmi2: {}", features.bmi2);
    println!("neon: {}", features.neon);
    println!("sve: {}", features.sve);
    Ok(())
}

fn repl_command() -> Result<(), String> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut line = String::new();
    loop {
        print!("forge> ");
        stdout.flush().map_err(|e| e.to_string())?;
        line.clear();
        if stdin
            .lock()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?
            == 0
        {
            break;
        }
        let expression = line.trim();
        if expression == ":quit" || expression == ":q" {
            break;
        }
        if expression.is_empty() {
            continue;
        }
        match forge_runtime::evaluate(expression, &[]) {
            Ok(value) => println!("{value}"),
            Err(error) => eprintln!("forge-cli: {error}"),
        }
    }
    Ok(())
}

fn parse_size(value: &str) -> Result<usize, String> {
    let (number, multiplier) = match value.strip_suffix('K') {
        Some(number) => (number, 1_000usize),
        None => match value.strip_suffix('M') {
            Some(number) => (number, 1_000_000usize),
            None => (value, 1),
        },
    };
    number
        .parse::<usize>()
        .map(|number| number.saturating_mul(multiplier))
        .map_err(|e| format!("invalid benchmark size {value:?}: {e}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
