//! Differential coverage across the native runtime and the executable WASM
//! backend. The native leg is x86-64 on the x86 CI lane and AArch64 on the
//! emulated ARM64 lane, so the same test exercises both native backends while
//! comparing each one with the same real WASM module.

use forge_ir::interp::{interpret, RtValue};
use forge_runtime::{evaluate, lower_source};
use std::process::Command;

const NODE_WASM_EVAL: &str = r#"
const wasm = Buffer.from(process.argv[1], 'hex');
const args = process.argv[2] === '' ? [] : process.argv[2].split(',').map(Number);
WebAssembly.instantiate(wasm).then(({ instance }) => {
    const result = instance.exports.eval(...args);
    const bits = new DataView(new ArrayBuffer(8));
    bits.setFloat64(0, result, true);
    process.stdout.write(bits.getBigUint64(0, true).toString(16));
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
        .args(["-e", NODE_WASM_EVAL, &wasm_hex, &arg_text])
        .output()
        .map_err(|error| format!("could not start Node WASM runtime: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Node WASM runtime failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let bits = u64::from_str_radix(String::from_utf8_lossy(&output.stdout).trim(), 16)
        .map_err(|error| format!("Node WASM runtime returned invalid f64 bits: {error}"))?;
    Ok(f64::from_bits(bits))
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
