//! Small, dependency-free command line front end for the compiler pipeline.
//!
//! The command grammar intentionally stays shell-friendly. Machine-readable
//! inspection commands print stable sections, while errors go to stderr and
//! use a non-zero exit status.

use std::collections::HashMap;
use std::env;
use std::io::{self, BufRead, Write};
use std::time::Instant;

fn usage() {
    eprintln!(
        "usage: forge-cli <command> ...\n\
         commands:\n\
           eval EXPR [ARGS...] [--name VALUE]\n\
           compile EXPR --arch x86_64|aarch64|wasm [--opt 0|1|2] [--features LIST]\n\
           asm EXPR\n\
           ir EXPR [--after opt]\n\
           cfg EXPR [--dot]\n\
           regalloc EXPR\n\
           bench EXPR [--sizes LIST]\n\
           verify EXPR [--iters N]\n\
           cpuinfo\n\
           repl"
    );
}

fn main() {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        usage();
        std::process::exit(2);
    };
    let rest = args.collect::<Vec<_>>();

    let result = match command.as_str() {
        "eval" => eval_command(&rest),
        "compile" => compile_command(&rest),
        "asm" => inspect_command(&rest, Inspection::Assembly),
        "ir" => ir_command(&rest),
        "cfg" => inspect_command(&rest, Inspection::Cfg),
        "regalloc" => inspect_command(&rest, Inspection::Regalloc),
        "bench" => bench_command(&rest),
        "verify" => verify_command(&rest),
        "cpuinfo" => cpuinfo_command(&rest),
        "repl" => repl_command(),
        _ => {
            usage();
            Err(format!("unknown command: {command}"))
        }
    };

    if let Err(error) = result {
        eprintln!("forge-cli: {error}");
        std::process::exit(1);
    }
}

fn source(args: &[String]) -> Result<&str, String> {
    args.first()
        .map(String::as_str)
        .ok_or_else(|| "an expression is required".to_string())
}

fn eval_command(args: &[String]) -> Result<(), String> {
    let expression = source(args)?;
    let function = forge_runtime::lower_source(expression).map_err(|e| e.to_string())?;
    let values = parse_values(&args[1..], &function.params)?;
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

fn compile_command(args: &[String]) -> Result<(), String> {
    let expression = source(args)?;
    let arch = option(args, "arch").unwrap_or("x86_64");
    let opt = option(args, "opt").unwrap_or("2");
    if !matches!(opt, "0" | "1" | "2") {
        return Err(format!("--opt must be 0, 1, or 2, got {opt:?}"));
    }
    if let Some(features) = option(args, "features") {
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
    Cfg,
    Regalloc,
}

fn inspect_command(args: &[String], inspection: Inspection) -> Result<(), String> {
    let expression = source(args)?;
    let artifacts = forge_runtime::compile_artifacts(expression).map_err(|e| e.to_string())?;
    match inspection {
        Inspection::Assembly => print_assembly(&artifacts),
        Inspection::Cfg => print_cfg(&artifacts.function, args.iter().any(|arg| arg == "--dot")),
        Inspection::Regalloc => print_regalloc(&artifacts),
    }
    Ok(())
}

fn ir_command(args: &[String]) -> Result<(), String> {
    let expression = source(args)?;
    let mut function = forge_runtime::lower_source(expression).map_err(|e| e.to_string())?;
    if option(args, "after").is_some_and(|pass| pass != "none" && pass != "lower") {
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

fn bench_command(args: &[String]) -> Result<(), String> {
    let expression = source(args)?;
    let sizes = option(args, "sizes").unwrap_or("1,10,100,1K");
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

fn verify_command(args: &[String]) -> Result<(), String> {
    let expression = source(args)?;
    let iterations = option(args, "iters")
        .unwrap_or("1000")
        .parse::<usize>()
        .map_err(|e| format!("invalid --iters: {e}"))?;
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

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let flag = format!("--{name}");
    args.iter().enumerate().find_map(|(index, argument)| {
        if let Some(value) = argument.strip_prefix(&format!("{flag}=")) {
            Some(value)
        } else if argument == &flag {
            args.get(index + 1).map(String::as_str)
        } else {
            None
        }
    })
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
