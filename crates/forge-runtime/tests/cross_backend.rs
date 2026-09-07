//! Differential coverage across the native runtime and the executable WASM
//! backend. The native leg is x86-64 on the x86 CI lane and AArch64 on the
//! emulated ARM64 lane, so the same test exercises both native backends while
//! comparing each one with the same real WASM module.

use forge_ir::interp::{interpret, RtValue};
use forge_runtime::{evaluate, lower_source};
use std::process::Command;

const NODE_WASM_EVAL: &str = r#"
const wasm = Buffer.from(process.argv[1], 'hex');
const args = process.argv[2] === '' ? [] : process.argv[2].split(',').map((token) => {
    if (token === 'true') return true;
    if (token === 'false') return false;
    if (token.endsWith('n')) return BigInt(token.slice(0, -1));
    return Number(token);
});
const resultType = process.argv[3];
const imports = { forge: { fmod: (lhs, rhs) => lhs % rhs } };
WebAssembly.instantiate(wasm, imports).then(({ instance }) => {
    const result = instance.exports.eval(...args);
    if (resultType === 'i64') {
        if (typeof result !== 'bigint') throw new Error(`expected BigInt result, got ${typeof result}`);
        process.stdout.write(`i64:${result}`);
    } else if (resultType === 'bool') {
        if (typeof result !== 'number' || ![0, 1].includes(result)) {
            throw new Error(`expected canonical bool result, got ${result}`);
        }
        process.stdout.write(`bool:${result === 1}`);
    } else {
        const bits = new DataView(new ArrayBuffer(8));
        bits.setFloat64(0, result, true);
        process.stdout.write(`f64:${bits.getBigUint64(0, true).toString(16)}`);
    }
}).catch((error) => {
    console.error(error);
    process.exitCode = 1;
});
"#;

fn wasm_evaluate(source: &str, args: &[f64]) -> Result<f64, String> {
    let artifact = forge_wasm::compile_artifact(source)?;
    if artifact.parameter_types.iter().any(|ty| ty != "f64") || artifact.result_type != "f64" {
        return Err("cross-backend cases must use an all-f64 signature".to_string());
    }
    let wasm_hex = artifact
        .wasm_bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let arg_text = args
        .iter()
        .map(|value| {
            if value.is_nan() {
                "NaN".to_string()
            } else if value.is_infinite() {
                if value.is_sign_negative() {
                    "-Infinity".to_string()
                } else {
                    "Infinity".to_string()
                }
            } else {
                value.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let output = Command::new("node")
        .args(["-e", NODE_WASM_EVAL, &wasm_hex, &arg_text, "f64"])
        .output()
        .map_err(|error| format!("could not start Node WASM runtime: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Node WASM runtime failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let bits = u64::from_str_radix(stdout.trim().strip_prefix("f64:").unwrap_or_default(), 16)
        .map_err(|error| format!("Node WASM runtime returned invalid f64 bits: {error}"))?;
    Ok(f64::from_bits(bits))
}

fn wasm_execute_typed(source: &str, args: &str) -> Result<String, String> {
    let artifact = forge_wasm::compile_artifact(source)?;
    let wasm_hex = artifact
        .wasm_bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let output = Command::new("node")
        .args([
            "-e",
            NODE_WASM_EVAL,
            &wasm_hex,
            args,
            artifact.result_type.as_str(),
        ])
        .output()
        .map_err(|error| format!("could not start Node WASM runtime: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Node typed WASM runtime failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn assert_equivalent(expected: f64, actual: f64, backend: &str, source: &str, args: &[f64]) {
    if expected.is_nan() || actual.is_nan() {
        assert!(
            expected.is_nan() && actual.is_nan(),
            "{backend} NaN mismatch for {source:?} with args={args:?}: interpreter={expected:?}, backend={actual:?}"
        );
    } else {
        assert_eq!(
            expected.to_bits(),
            actual.to_bits(),
            "{backend} bit mismatch for {source:?} with args={args:?}: interpreter={expected:?}, backend={actual:?}"
        );
    }
}

#[test]
fn native_and_wasm_backends_match_interpreter_for_supported_f64_cases() {
    let cases = [
        ("x + y * 2.0", vec![3.5, -2.0]),
        ("let t = x * x in sqrt(t + 1.0)", vec![-3.0]),
        ("if x < 0.0 then abs(x) else x + 1.0", vec![-4.0]),
        ("if x < 0.0 then abs(x) else x + 1.0", vec![0.0]),
        ("min(x, y) + max(x, y)", vec![1.25, 4.5]),
        ("x / (abs(y) + 1.0)", vec![f64::MAX, -2.0]),
        ("x + 0.0", vec![-0.0]),
        ("sqrt(abs(x))", vec![f64::INFINITY]),
        ("x * 1.0", vec![f64::NAN]),
        ("x % y", vec![5.5, 2.0]),
        ("x % y", vec![-0.0, 3.0]),
    ];

    for (source, args) in cases {
        let function = lower_source(source).expect("cross-backend source must lower");
        let interpreter_args = args.iter().copied().map(RtValue::F64).collect::<Vec<_>>();
        let expected = match interpret(&function, &interpreter_args) {
            RtValue::F64(value) => value,
            other => panic!("cross-backend source returned {other:?}: {source:?}"),
        };
        let native = evaluate(source, &args).expect("native backend must execute");
        let wasm = wasm_evaluate(source, &args).expect("WASM backend must execute");
        assert_equivalent(expected, native, "native", source, &args);
        assert_equivalent(expected, wasm, "WASM", source, &args);
    }
}

#[test]
fn typed_wasm_results_execute_in_a_real_runtime() {
    let cases = [
        ("x & 17", "5n", vec![RtValue::I64(5)]),
        (
            "let t = (x * y) & 7 in t",
            "5n,3n",
            vec![RtValue::I64(5), RtValue::I64(3)],
        ),
        (
            "if (x & 1) == 0 then x else -x",
            "-42n",
            vec![RtValue::I64(-42)],
        ),
        (
            "(x << 3) | (y & 7)",
            "5n,11n",
            vec![RtValue::I64(5), RtValue::I64(11)],
        ),
    ];

    for (source, wasm_args, interpreter_args) in cases {
        let function = lower_source(source).expect("typed WASM source must lower");
        let expected = interpret(&function, &interpreter_args);
        let actual = wasm_execute_typed(source, wasm_args).expect("typed WASM must execute");
        assert_eq!(actual, format!("i64:{}", expected.as_i64()), "{source:?}");
    }

    let bool_cases = [
        ("!flag", "true", vec![RtValue::Bool(true)]),
        (
            "left && right",
            "true,false",
            vec![RtValue::Bool(true), RtValue::Bool(false)],
        ),
        ("if flag then 7 else 3", "false", vec![RtValue::Bool(false)]),
    ];

    for (source, wasm_args, interpreter_args) in bool_cases {
        let function = lower_source(source).expect("typed WASM source must lower");
        let expected = interpret(&function, &interpreter_args);
        let actual = wasm_execute_typed(source, wasm_args).expect("typed WASM must execute");
        match expected {
            RtValue::Bool(value) => assert_eq!(actual, format!("bool:{value}"), "{source:?}"),
            RtValue::I64(value) => assert_eq!(actual, format!("i64:{value}"), "{source:?}"),
            other => panic!("unexpected typed result {other:?}: {source:?}"),
        }
    }
}
