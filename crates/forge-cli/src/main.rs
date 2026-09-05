//! Small, dependency-free command line front end for the compiler pipeline.
//!
//! The command grammar intentionally stays shell-friendly. Machine-readable
//! inspection commands print stable sections, while errors go to stderr and
//! use a non-zero exit status.

use clap::{Args, Parser, Subcommand};
use std::collections::HashMap;
use std::env;
use std::io::{self, BufRead, IsTerminal, Write};
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
    /// Include the generated machine/module bytes in the report.
    #[arg(long)]
    emit: bool,
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
            args.emit,
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
        eprintln!(
            "{}",
            paint(
                &format!("forge-cli: {error}"),
                31,
                io::stderr().is_terminal()
            )
        );
        std::process::exit(exit_code(&cli.command, &error));
    }
}

fn paint(text: &str, color: u8, terminal: bool) -> String {
    if terminal && env::var_os("NO_COLOR").is_none() {
        format!("\x1b[{color}m{text}\x1b[0m")
    } else {
        text.to_string()
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
        "AArch64 compilation failed",
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
    emit: bool,
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
            println!("target: wasm\nbytes: {}", bytes.len());
            if emit {
                println!("hex: {}", hex(&bytes));
            }
        }
        "x86_64" => {
            let artifacts =
                forge_runtime::compile_artifacts_with_optimization(expression, opt != "0")
                    .map_err(|e| e.to_string())?;
            println!(
                "target: x86_64\noptimization: {opt}\nbytes: {}",
                artifacts.bytes.len()
            );
            if emit {
                println!("hex: {}", hex(&artifacts.bytes));
            }
        }
        "aarch64" => {
            let mut function =
                forge_runtime::lower_source(expression).map_err(|e| e.to_string())?;
            if opt != "0" {
                forge_opt::optimize(&mut function);
            }
            forge_ir::verify::verify(&function).map_err(|e| e.to_string())?;
            let bytes = forge_aarch64::emit_f64(&function)
                .map_err(|error| format!("AArch64 compilation failed: {error}"))?;
            println!(
                "target: aarch64\noptimization: {opt}\nnative: {}\nbytes: {}",
                forge_aarch64::is_native_target(),
                bytes.len()
            );
            if emit {
                println!("hex: {}", hex(&bytes));
            }
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
    println!(
        "{}",
        paint(
            "; selected x86-64 instructions",
            36,
            io::stdout().is_terminal()
        )
    );
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
    let mut bindings = HashMap::new();
    let mut history = Vec::new();
    loop {
        let prompt = paint("forge>", 36, stdout.is_terminal());
        print!("{prompt} ");
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
        history.push(expression.to_string());
        if expression.starts_with(':') {
            if expression == ":quit" || expression == ":q" {
                break;
            }
            repl_command_line(expression, &mut bindings, &mut history)?;
            continue;
        }
        match repl_evaluate(expression, &bindings) {
            Ok(value) => println!("{value}"),
            Err(error) => eprintln!(
                "{}",
                paint(
                    &format!("forge-cli: {error}"),
                    31,
                    io::stderr().is_terminal()
                )
            ),
        }
    }
    Ok(())
}

fn repl_command_line(
    line: &str,
    bindings: &mut HashMap<String, f64>,
    history: &mut [String],
) -> Result<(), String> {
    let mut parts = line
        .splitn(3, char::is_whitespace)
        .filter(|part| !part.is_empty());
    match parts.next().unwrap_or_default() {
        ":help" => {
            println!(":set NAME VALUE  bind a persistent f64 value");
            println!(":unset NAME       remove a binding");
            println!(":vars              list bindings");
            println!(":history           list expressions entered this session");
            println!(":clear             remove all bindings");
            println!(":asm EXPR          inspect selected instructions and bytes");
            println!(":ir EXPR           inspect SSA IR");
            println!(":bench EXPR [SIZES] benchmark an expression");
            println!(":quit              leave the REPL");
        }
        ":set" => {
            let name = parts.next().ok_or("usage: :set NAME VALUE")?;
            let value = parts
                .next()
                .ok_or("usage: :set NAME VALUE")?
                .parse::<f64>()
                .map_err(|error| format!("invalid value for {name}: {error}"))?;
            if !is_identifier(name) {
                return Err(format!("invalid binding name {name:?}"));
            }
            bindings.insert(name.to_string(), value);
        }
        ":unset" => {
            let name = parts.next().ok_or("usage: :unset NAME")?;
            bindings.remove(name);
        }
        ":vars" => {
            let mut names = bindings.keys().collect::<Vec<_>>();
            names.sort();
            for name in names {
                println!("{name} = {}", bindings[name]);
            }
        }
        ":history" => {
            for (index, entry) in history.iter().enumerate() {
                println!("{:>4}  {entry}", index + 1);
            }
        }
        ":clear" => bindings.clear(),
        ":asm" => {
            let expression = parts.next().ok_or("usage: :asm EXPR")?;
            inspect_command(expression, Inspection::Assembly)?;
        }
        ":ir" => {
            let expression = parts.next().ok_or("usage: :ir EXPR")?;
            ir_command(expression, None)?;
        }
        ":bench" => {
            let expression = parts.next().ok_or("usage: :bench EXPR [SIZES]")?;
            let sizes = parts.next().unwrap_or("1,10,100,1K");
            bench_command(expression, sizes)?;
        }
        command => return Err(format!("unknown REPL command {command:?}; try :help")),
    }
    Ok(())
}

fn repl_evaluate(expression: &str, bindings: &HashMap<String, f64>) -> Result<f64, String> {
    let function = forge_runtime::lower_source(expression).map_err(|e| e.to_string())?;
    if function
        .params
        .iter()
        .any(|(_, ty)| *ty != forge_ir::Ty::F64)
        || function.types.last() != Some(&forge_ir::Ty::F64)
    {
        return Err("REPL currently accepts f64 parameters and results only".to_string());
    }
    let values = function
        .params
        .iter()
        .map(|(name, _)| {
            bindings
                .get(name)
                .copied()
                .ok_or_else(|| format!("missing binding for parameter {name:?}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    forge_runtime::evaluate(expression, &values).map_err(|e| e.to_string())
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_output_is_disabled_without_a_terminal() {
        assert_eq!(paint("error", 31, false), "error");
    }

    #[test]
    fn repl_bindings_are_reused_by_later_expressions() {
        let mut bindings = HashMap::new();
        let mut history = Vec::new();
        repl_command_line(":set x 3", &mut bindings, &mut history).unwrap();
        assert_eq!(repl_evaluate("x * 2", &bindings).unwrap(), 6.0);
    }

    #[test]
    fn binding_names_are_checked_before_insertion() {
        let mut bindings = HashMap::new();
        let mut history = Vec::new();
        let error = repl_command_line(":set 3x 1", &mut bindings, &mut history).unwrap_err();
        assert!(error.contains("invalid binding name"));
        assert!(bindings.is_empty());
    }

    #[test]
    fn exit_codes_distinguish_compile_and_verification_failures() {
        let compile = Command::Compile(CompileArgs {
            expression: "if x then x else x".to_string(),
            arch: "aarch64".to_string(),
            opt: "2".to_string(),
            features: None,
            emit: false,
        });
        assert_eq!(
            exit_code(&compile, "AArch64 compilation failed: unsupported"),
            2
        );

        let verify = Command::Verify(VerifyArgs {
            expression: "x".to_string(),
            iters: 1,
        });
        assert_eq!(exit_code(&verify, "mismatch at iteration 0"), 3);
        assert_eq!(exit_code(&verify, "type checking failed: bad input"), 2);
    }
}
