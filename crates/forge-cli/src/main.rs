//! Small, dependency-free command line front end for the runtime pipeline.
//! The syntax is intentionally stable and shell-friendly while the richer
//! workbench/inspection commands are still separate deliverables.

use std::env;

fn usage() {
    eprintln!("usage: forge-cli <eval|ir|verify> <expression> [f64 arguments ...]");
}

fn main() {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        usage();
        std::process::exit(2);
    };
    let Some(source) = args.next() else {
        usage();
        std::process::exit(2);
    };

    let result = match command.as_str() {
        "eval" => {
            let values = args
                .map(|arg| arg.parse::<f64>().map_err(|e| e.to_string()))
                .collect::<Result<Vec<_>, _>>();
            match values {
                Ok(values) => match forge_runtime::evaluate(&source, &values) {
                    Ok(value) => {
                        println!("{value}");
                        Ok(())
                    }
                    Err(e) => Err(e.to_string()),
                },
                Err(e) => Err(format!("invalid numeric argument: {e}")),
            }
        }
        "ir" => match forge_runtime::lower_source(&source) {
            Ok(function) => {
                print!("{}", forge_ir::print::print_function(&function));
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        },
        "verify" => match forge_runtime::lower_source(&source) {
            Ok(_) => {
                println!("ok");
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        },
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
