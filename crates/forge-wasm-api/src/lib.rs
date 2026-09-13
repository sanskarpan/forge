//! Stable, serialization-friendly API boundary for browser integrations.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use wasm_bindgen::prelude::wasm_bindgen;

pub fn run(source: &str, args: &[f64]) -> Result<f64, String> {
    forge_wasm::evaluate(source, args)
}

pub fn compile(source: &str) -> Result<Vec<u8>, String> {
    forge_wasm::compile(source)
}

pub fn compile_artifact(source: &str) -> Result<forge_wasm::WasmArtifact, String> {
    forge_wasm::compile_artifact(source)
}

pub fn cpu_features() -> &'static str {
    "portable-interpreter"
}

const MAX_BENCHMARK_CALLS: usize = 1_000_000;

/// Benchmarks the portable source evaluator for the requested call counts.
/// The browser workbench separately measures the instantiated WASM export;
/// this API remains useful for hosts that need a deterministic, portable
/// baseline and labels that backend explicitly in its response.
pub fn benchmark_json(source: &str, sizes: &[u32]) -> String {
    let artifact = match compile_artifact(source) {
        Ok(artifact) => artifact,
        Err(error) => return format!(r#"{{"ok":false,"error":{}}}"#, json_string(&error)),
    };
    if artifact.parameter_types.iter().any(|ty| ty != "f64") || artifact.result_type != "f64" {
        return r#"{"ok":false,"error":"benchmark requires an all-f64 expression"}"#.to_string();
    }
    let args = vec![1.25; artifact.parameter_count()];
    let results = sizes
        .iter()
        .map(|requested| {
            let calls = (*requested as usize).min(MAX_BENCHMARK_CALLS);
            let started = now_millis();
            let mut last = None;
            for _ in 0..calls {
                match run(source, &args) {
                    Ok(value) => last = Some(value),
                    Err(error) => {
                        return format!(
                            r#"{{"size":{},"error":{}}}"#,
                            requested,
                            json_string(&error)
                        )
                    }
                }
            }
            let elapsed_ms = now_millis() - started;
            let result = last.map_or_else(
                || "null".to_string(),
                |value| {
                    if value.is_finite() {
                        value.to_string()
                    } else {
                        "null".to_string()
                    }
                },
            );
            format!(
                r#"{{"size":{},"calls":{},"elapsed_ms":{},"last_result":{}}}"#,
                requested, calls, elapsed_ms, result
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"ok":true,"backend":"portable-interpreter","results":[{}]}}"#,
        results
    )
}

fn now_millis() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::sync::OnceLock;
        use std::time::Instant;

        static EPOCH: OnceLock<Instant> = OnceLock::new();
        EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64() * 1_000.0
    }
}

/// Browser-facing error boundary for `run`. The Rust API above remains useful
/// to native callers; these wrappers translate errors to JavaScript values.
#[wasm_bindgen]
pub fn run_wasm(source: &str, args: &[f64]) -> Result<f64, wasm_bindgen::JsValue> {
    run(source, args).map_err(|error| wasm_bindgen::JsValue::from_str(&error))
}

#[wasm_bindgen]
pub fn compile_wasm(source: &str) -> Result<Vec<u8>, wasm_bindgen::JsValue> {
    compile(source).map_err(|error| wasm_bindgen::JsValue::from_str(&error))
}

/// Returns the browser-friendly structured artifact boundary. JSON is used
/// instead of a wasm-bindgen struct so this API remains stable for plain JS,
/// TypeScript, and non-wasm native integration tests alike.
#[wasm_bindgen]
pub fn compile_artifact_json(source: &str) -> String {
    match compile_artifact(source) {
        Ok(artifact) => {
            let analysis = match analysis_json(source, &artifact.wasm_bytes) {
                Ok(analysis) => analysis,
                Err(error) => return format!(r#"{{"ok":false,"error":{}}}"#, json_string(&error)),
            };
            let params = artifact
                .parameter_types
                .iter()
                .map(|ty| json_string(ty))
                .collect::<Vec<_>>()
                .join(",");
            let imports = artifact
                .required_imports
                .iter()
                .map(|import| json_string(import))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                r#"{{"ok":true,"parameter_types":[{params}],"result_type":{},"required_imports":[{imports}],"wasm_bytes_hex":{},"wasm_bytes_len":{},{} }}"#,
                json_string(&artifact.result_type),
                json_string(&artifact.wasm_hex),
                artifact.wasm_bytes.len(),
                analysis
            )
        }
        Err(error) => format!(r#"{{"ok":false,"error":{}}}"#, json_string(&error)),
    }
}

/// Compiles a target-specific inspection artifact without allocating
/// executable memory. This is the browser-facing bridge for the Workbench's
/// target selector: WASM is emitted as an executable module, while x86-64 and
/// AArch64 are emitted as inspectable native bytes. Native bytes are never
/// executed in the browser, and host libm addresses are deliberately rejected
/// from the wasm32 x86-64 inspection path because a browser cannot embed a
/// meaningful process-local function pointer in a future JIT call.
#[wasm_bindgen]
pub fn compile_target_artifact_json(source: &str, target: &str) -> String {
    match target {
        "wasm" => compile_artifact_json(source),
        "x86_64" => panic_safe_native_artifact(source, "x86_64", native_x64_artifact),
        "aarch64" => panic_safe_native_artifact(source, "aarch64", native_aarch64_artifact),
        _ => json_error(&format!("unsupported target `{target}`")),
    }
}

fn panic_safe_native_artifact(
    source: &str,
    target: &str,
    build: fn(&str) -> Result<String, String>,
) -> String {
    match catch_unwind(AssertUnwindSafe(|| build(source))) {
        Ok(Ok(json)) => json,
        Ok(Err(error)) => json_error(&format!("{target} artifact unavailable: {error}")),
        Err(_) => json_error(&format!(
            "{target} artifact rejected an unsupported IR shape"
        )),
    }
}

fn native_x64_artifact(source: &str) -> Result<String, String> {
    let mut function = lower_native_function(source)?;
    forge_opt::optimize(&mut function);
    forge_ir::verify::verify(&function).map_err(|error| error.to_string())?;
    if function
        .insts
        .iter()
        .any(|inst| matches!(inst, forge_ir::Inst::Call { .. }))
    {
        return Err(
            "host libm calls need a native process address and are not serializable in wasm32"
                .to_string(),
        );
    }
    let selected = forge_x64::select(&function);
    let intervals = forge_regalloc::build_intervals(&function, &selected);
    let excluded = forge_regalloc::excluded_registers(&function, &selected);
    let (assignment, _) = forge_regalloc::allocate(intervals.clone(), &excluded, &selected);
    forge_regalloc::verify_allocation(&intervals, &assignment)
        .map_err(|error| error.to_string())?;
    let bytes = forge_emit::emit_body(&function, &selected, &assignment);
    // `emit_body` currently returns the finalized function bytes as one
    // buffer; it does not expose instruction boundaries because prologue,
    // spill, pool, and branch-fixup bytes can be inserted between selected
    // instructions. Keep the inspection artifact truthful by reporting that
    // exact contiguous body as one byte-bearing row instead of fabricating
    // empty per-instruction encodings.
    let asm = format!(
        r#"{{"offset":0,"bytes":{},"text":{}}}"#,
        json_string(&hex_bytes(&bytes)),
        json_string(&format!(
            "encoded x86-64 body ({} selected instructions)",
            selected.insts.len()
        ))
    );
    let intervals = intervals_json(&intervals, &assignment);
    let analysis = analysis_core_json(source)?;
    Ok(format!(
        r#"{{"ok":true,"target":"x86_64","parameter_types":[{}],"result_type":{},"bytes_hex":{},"bytes_len":{},"wasm_bytes_hex":"","wasm_bytes_len":0,"asm":[{}],"intervals":[{}],"encoding":"x86-64",{}}}"#,
        parameter_types_json(&function),
        json_string(&result_type_name(&function)),
        json_string(&hex_bytes(&bytes)),
        bytes.len(),
        asm,
        intervals,
        analysis
    ))
}

fn native_aarch64_artifact(source: &str) -> Result<String, String> {
    let mut function = lower_native_function(source)?;
    forge_opt::optimize(&mut function);
    forge_ir::verify::verify(&function).map_err(|error| error.to_string())?;
    let bytes = forge_aarch64::emit_f64(&function)?;
    let asm = bytes
        .chunks(4)
        .enumerate()
        .filter_map(|(index, word)| {
            let [b0, b1, b2, b3] = word else {
                return None;
            };
            let bits = u32::from_le_bytes([*b0, *b1, *b2, *b3]);
            Some(format!(
                r#"{{"offset":{},"bytes":{},"text":{}}}"#,
                index * 4,
                json_string(&hex_bytes(word)),
                json_string(&format!("word 0x{bits:08x}"))
            ))
        })
        .collect::<Vec<_>>()
        .join(",");
    let analysis = analysis_core_json(source)?;
    Ok(format!(
        r#"{{"ok":true,"target":"aarch64","parameter_types":[{}],"result_type":{},"bytes_hex":{},"bytes_len":{},"wasm_bytes_hex":"","wasm_bytes_len":0,"asm":[{}],"intervals":[],"encoding":"aarch64",{}}}"#,
        parameter_types_json(&function),
        json_string(&result_type_name(&function)),
        json_string(&hex_bytes(&bytes)),
        bytes.len(),
        asm,
        analysis
    ))
}

fn lower_native_function(source: &str) -> Result<forge_ir::Function, String> {
    let (tokens, lex_diags) = forge_syntax::lexer::lex(source);
    if !lex_diags.is_empty() {
        return Err(format!("lexing failed: {lex_diags:?}"));
    }
    let (ast, parse_diags) = forge_syntax::parser::parse(&tokens);
    if !parse_diags.is_empty() {
        return Err(format!("parsing failed: {parse_diags:?}"));
    }
    let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
        .map_err(|diags| format!("type checking failed: {diags:?}"))?;
    let function = forge_ir::lower::lower(&typed);
    forge_ir::verify::verify(&function).map_err(|error| error.to_string())?;
    Ok(function)
}

fn parameter_types_json(function: &forge_ir::Function) -> String {
    function
        .params
        .iter()
        .map(|(_, ty)| json_string(&format!("{ty:?}").to_lowercase()))
        .collect::<Vec<_>>()
        .join(",")
}

fn result_type_name(function: &forge_ir::Function) -> String {
    function
        .types
        .last()
        .map(|ty| format!("{ty:?}").to_lowercase())
        .unwrap_or_else(|| "unknown".to_string())
}

fn intervals_json(
    intervals: &[forge_regalloc::Interval],
    assignment: &HashMap<forge_ir::Value, forge_regalloc::Location>,
) -> String {
    intervals
        .iter()
        .map(|interval| {
            let location = assignment
                .get(&interval.value)
                .map(|location| format!("{location:?}"))
                .unwrap_or_else(|| "unassigned".to_string());
            format!(
                r#"{{"value":"v{}","start":{},"end":{},"class":{},"location":{}}}"#,
                interval.value.0,
                interval.start,
                interval.end,
                json_string(&format!("{:?}", interval.reg_class).to_lowercase()),
                json_string(&location)
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn json_error(error: &str) -> String {
    format!(r#"{{"ok":false,"error":{}}}"#, json_string(error))
}

/// Produces the target-independent analysis fields used by the workbench.
/// WASM is a stack machine, so its assembly field is a decoded stack trace and
/// its interval field contains logical stack-value lifetimes rather than
/// native register assignments.
fn analysis_json(source: &str, wasm_bytes: &[u8]) -> Result<String, String> {
    let analysis = analysis_core_json(source)?;
    let (asm, intervals, max_depth) = wasm_stack_analysis(wasm_bytes)?;
    Ok(format!(
        r#"{analysis},"intervals":[{intervals}],"asm":[{asm}],"encoding":"wasm-stack","stack_max_depth":{max_depth}""#
    ))
}

#[derive(Debug)]
struct WasmStackInstruction {
    offset: usize,
    bytes: String,
    text: String,
    stack_before: usize,
    stack_after: usize,
    pops: usize,
    pushes: Option<&'static str>,
}

#[derive(Debug)]
struct WasmStackFrame {
    base_depth: usize,
    result_type: Option<&'static str>,
}

#[derive(Debug)]
struct WasmStackValue {
    id: usize,
    start: usize,
    ty: &'static str,
    depth: usize,
}

/// Decodes the emitted function body into a browser-facing stack trace. This
/// is deliberately a stack artifact, not a native disassembly: each row
/// records the exact module byte span, decoded opcode, and abstract operand
/// stack depth, while the interval rows show the lifetime of logical stack
/// values.
fn wasm_stack_analysis(wasm_bytes: &[u8]) -> Result<(String, String, usize), String> {
    let (body, body_offset) = wasm_function_body(wasm_bytes)?;
    let (local_group_count, mut cursor) = read_uleb(body, 0)?;
    for _ in 0..local_group_count {
        let (count, next) = read_uleb(body, cursor)?;
        cursor = next
            .checked_add(1)
            .ok_or("WASM local declaration overflow")?;
        if cursor > body.len() {
            return Err("WASM local declaration is truncated".to_string());
        }
        if count == 0 {
            return Err("WASM local declaration has an empty group".to_string());
        }
    }

    let mut instructions = Vec::new();
    let mut depth = 0usize;
    let mut max_depth = 0usize;
    let mut frames = Vec::new();
    while cursor < body.len() {
        let start = cursor;
        let (info, next) = decode_wasm_instruction(body, cursor)?;
        cursor = next;
        let before = depth;
        match info.text.as_str() {
            "if" => {
                if depth == 0 {
                    return Err("WASM if has no condition on the stack".to_string());
                }
                depth -= 1;
                frames.push(WasmStackFrame {
                    base_depth: depth,
                    result_type: info.pushes,
                });
            }
            "else" => {
                let frame = frames.last().ok_or("WASM else has no matching if")?;
                depth = frame.base_depth;
            }
            "end" => {
                if let Some(frame) = frames.pop() {
                    depth = frame.base_depth + usize::from(frame.result_type.is_some());
                } else {
                    instructions.push(WasmStackInstruction {
                        offset: body_offset + start,
                        bytes: hex_bytes(&body[start..cursor]),
                        text: info.text,
                        stack_before: before,
                        stack_after: depth,
                        pops: info.pops,
                        pushes: info.pushes,
                    });
                    break;
                }
            }
            _ => {
                if depth < info.pops {
                    return Err(format!("WASM stack underflow while decoding {}", info.text));
                }
                depth = depth - info.pops + usize::from(info.pushes.is_some());
            }
        }
        max_depth = max_depth.max(depth);
        instructions.push(WasmStackInstruction {
            offset: body_offset + start,
            bytes: hex_bytes(&body[start..cursor]),
            text: info.text,
            stack_before: before,
            stack_after: depth,
            pops: info.pops,
            pushes: info.pushes,
        });
    }
    if !frames.is_empty() {
        return Err("WASM control structure is unterminated".to_string());
    }

    let mut values = Vec::new();
    let mut active: Vec<WasmStackValue> = Vec::new();
    let mut next_id = 0;
    let mut frame_values = Vec::new();
    for (index, instruction) in instructions.iter().enumerate() {
        if instruction.text == "if" {
            if let Some(value) = active.pop() {
                values.push((value.id, value.start, index, value.ty, value.depth));
            }
            frame_values.push((active.len(), instruction.pushes));
        } else if instruction.text == "else" {
            let base = frame_values.last().ok_or("WASM else has no value frame")?.0;
            while active.len() > base {
                if let Some(value) = active.pop() {
                    values.push((value.id, value.start, index, value.ty, value.depth));
                }
            }
        } else if instruction.text == "end" {
            if let Some((base, result_type)) = frame_values.pop() {
                while active.len() > base {
                    if let Some(value) = active.pop() {
                        values.push((value.id, value.start, index, value.ty, value.depth));
                    }
                }
                if let Some(ty) = result_type {
                    active.push(WasmStackValue {
                        id: next_id,
                        start: index,
                        ty,
                        depth: active.len(),
                    });
                    next_id += 1;
                }
            }
        } else {
            for _ in 0..instruction.pops {
                if let Some(value) = active.pop() {
                    values.push((value.id, value.start, index, value.ty, value.depth));
                }
            }
            if let Some(ty) = instruction.pushes {
                active.push(WasmStackValue {
                    id: next_id,
                    start: index,
                    ty,
                    depth: active.len(),
                });
                next_id += 1;
            }
        }
    }
    for value in active {
        values.push((
            value.id,
            value.start,
            instructions.len(),
            value.ty,
            value.depth,
        ));
    }
    let asm = instructions
        .iter()
        .map(|instruction| {
            format!(
                r#"{{"offset":{},"bytes":{},"text":{},"stack_before":{},"stack_after":{}}}"#,
                instruction.offset,
                json_string(&instruction.bytes),
                json_string(&instruction.text),
                instruction.stack_before,
                instruction.stack_after
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let intervals = values
        .iter()
        .map(|(id, start, end, ty, depth)| {
            format!(
                r#"{{"value":"s{}","start":{},"end":{},"class":{},"location":"stack[{}]"}}"#,
                id,
                start,
                end,
                json_string(ty),
                depth
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    Ok((asm, intervals, max_depth))
}

struct WasmInstructionInfo {
    text: String,
    pops: usize,
    pushes: Option<&'static str>,
}

fn wasm_function_body(bytes: &[u8]) -> Result<(&[u8], usize), String> {
    if bytes.len() < 8 || &bytes[..4] != b"\0asm" {
        return Err("WASM artifact has an invalid header".to_string());
    }
    let mut cursor = 8;
    while cursor < bytes.len() {
        let id = *bytes.get(cursor).ok_or("WASM section is missing its id")?;
        cursor += 1;
        let (length, next) = read_uleb(bytes, cursor)?;
        cursor = next;
        let end = cursor
            .checked_add(length)
            .ok_or("WASM section length overflow")?;
        if end > bytes.len() {
            return Err("WASM section is truncated".to_string());
        }
        if id == 10 {
            let payload = &bytes[cursor..end];
            let (count, body_start) = read_uleb(payload, 0)?;
            if count != 1 {
                return Err("WASM artifact must contain exactly one function".to_string());
            }
            let (body_length, body_start) = read_uleb(payload, body_start)?;
            let body_end = body_start
                .checked_add(body_length)
                .ok_or("WASM function body length overflow")?;
            if body_end > payload.len() {
                return Err("WASM function body is truncated".to_string());
            }
            return Ok((&payload[body_start..body_end], cursor + body_start));
        }
        cursor = end;
    }
    Err("WASM artifact has no code section".to_string())
}

fn decode_wasm_instruction(
    bytes: &[u8],
    start: usize,
) -> Result<(WasmInstructionInfo, usize), String> {
    let opcode = *bytes.get(start).ok_or("WASM instruction is truncated")?;
    let mut cursor = start + 1;
    let mut text = String::new();
    let mut pops = 0;
    let mut pushes = None;
    match opcode {
        0x04 => {
            let result = *bytes
                .get(cursor)
                .ok_or("WASM if result type is truncated")?;
            cursor += 1;
            text.push_str("if");
            pushes = wasm_result_type(result)?;
        }
        0x05 => text.push_str("else"),
        0x0b => text.push_str("end"),
        0x10 => {
            let (index, next) = read_uleb(bytes, cursor)?;
            cursor = next;
            text = format!("call {index}");
            pops = 2;
            pushes = Some("f64");
        }
        0x20 | 0x21 => {
            let (index, next) = read_uleb(bytes, cursor)?;
            cursor = next;
            text = format!(
                "{} {index}",
                if opcode == 0x20 {
                    "local.get"
                } else {
                    "local.set"
                }
            );
            pops = usize::from(opcode == 0x21);
            pushes = (opcode == 0x20).then_some("value");
        }
        0x41 => {
            let (_, next) = read_sleb(bytes, cursor)?;
            cursor = next;
            text.push_str("i32.const");
            pushes = Some("i32");
        }
        0x42 => {
            let (_, next) = read_sleb(bytes, cursor)?;
            cursor = next;
            text.push_str("i64.const");
            pushes = Some("i64");
        }
        0x44 => {
            cursor = cursor.checked_add(8).ok_or("WASM f64.const overflow")?;
            if cursor > bytes.len() {
                return Err("WASM f64.const is truncated".to_string());
            }
            text.push_str("f64.const");
            pushes = Some("f64");
        }
        0x45 => {
            text.push_str("i32.eqz");
            pops = 1;
            pushes = Some("i32");
        }
        0x46..=0x47 | 0x51..=0x59 | 0x61..=0x66 | 0x71..=0x72 | 0x7c..=0x88 | 0x99..=0xa5 => {
            text.push_str(wasm_opcode_name(opcode));
            pops = if (0x99..=0x9f).contains(&opcode) {
                1
            } else {
                2
            };
            pushes = Some(
                if (0x46..=0x47).contains(&opcode)
                    || (0x51..=0x59).contains(&opcode)
                    || (0x61..=0x66).contains(&opcode)
                    || (0x71..=0x72).contains(&opcode)
                {
                    "i32"
                } else if (0x7c..=0x88).contains(&opcode) {
                    "i64"
                } else {
                    "f64"
                },
            );
        }
        _ => {
            return Err(format!(
                "WASM decoder does not recognize opcode 0x{opcode:02x}"
            ))
        }
    }
    Ok((WasmInstructionInfo { text, pops, pushes }, cursor))
}

fn wasm_opcode_name(opcode: u8) -> &'static str {
    match opcode {
        0x46 => "i32.eq",
        0x47 => "i32.ne",
        0x51 => "i64.eq",
        0x52 => "i64.ne",
        0x53 => "i64.lt_s",
        0x55 => "i64.gt_s",
        0x57 => "i64.le_s",
        0x59 => "i64.ge_s",
        0x61 => "f64.eq",
        0x62 => "f64.ne",
        0x63 => "f64.lt",
        0x64 => "f64.gt",
        0x65 => "f64.le",
        0x66 => "f64.ge",
        0x71 => "i32.and",
        0x72 => "i32.or",
        0x7c => "i64.add",
        0x7d => "i64.sub",
        0x7e => "i64.mul",
        0x7f => "i64.div_s",
        0x81 => "i64.rem_s",
        0x83 => "i64.and",
        0x84 => "i64.or",
        0x85 => "i64.xor",
        0x86 => "i64.shl",
        0x88 => "i64.shr_s",
        0x99 => "f64.abs",
        0x9a => "f64.neg",
        0x9b => "f64.ceil",
        0x9c => "f64.floor",
        0x9d => "f64.trunc",
        0x9e => "f64.nearest",
        0x9f => "f64.sqrt",
        0xa0 => "f64.add",
        0xa1 => "f64.sub",
        0xa2 => "f64.mul",
        0xa3 => "f64.div",
        0xa4 => "f64.min",
        0xa5 => "f64.max",
        _ => "unknown",
    }
}

fn wasm_result_type(byte: u8) -> Result<Option<&'static str>, String> {
    match byte {
        0x40 => Ok(None),
        0x7c => Ok(Some("f64")),
        0x7e => Ok(Some("i64")),
        0x7f => Ok(Some("i32")),
        _ => Err(format!("WASM block result has unknown type 0x{byte:02x}")),
    }
}

fn read_uleb(bytes: &[u8], mut cursor: usize) -> Result<(usize, usize), String> {
    let mut value = 0usize;
    let mut shift = 0usize;
    loop {
        let byte = *bytes
            .get(cursor)
            .ok_or("WASM unsigned immediate is truncated")?;
        cursor += 1;
        value |= usize::from(byte & 0x7f)
            .checked_shl(shift as u32)
            .ok_or("WASM unsigned immediate overflows usize")?;
        if byte & 0x80 == 0 {
            return Ok((value, cursor));
        }
        shift += 7;
        if shift >= usize::BITS as usize {
            return Err("WASM unsigned immediate is too wide".to_string());
        }
    }
}

fn read_sleb(bytes: &[u8], mut cursor: usize) -> Result<(i64, usize), String> {
    let mut value = 0i64;
    let mut shift = 0;
    loop {
        let byte = *bytes
            .get(cursor)
            .ok_or("WASM signed immediate is truncated")?;
        cursor += 1;
        value |= i64::from(byte & 0x7f) << shift;
        let done = byte & 0x80 == 0;
        shift += 7;
        if done {
            if shift < 64 && byte & 0x40 != 0 {
                value |= (!0i64) << shift;
            }
            return Ok((value, cursor));
        }
        if shift >= 64 {
            return Err("WASM signed immediate is too wide".to_string());
        }
    }
}

fn analysis_core_json(source: &str) -> Result<String, String> {
    let (tokens, lex_diags) = forge_syntax::lexer::lex(source);
    if !lex_diags.is_empty() {
        return Err(format!("lexing failed: {lex_diags:?}"));
    }
    let (ast, parse_diags) = forge_syntax::parser::parse(&tokens);
    if !parse_diags.is_empty() {
        return Err(format!("parsing failed: {parse_diags:?}"));
    }
    let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
        .map_err(|diags| format!("type checking failed: {diags:?}"))?;
    let lowered = forge_ir::lower::lower(&typed);
    forge_ir::verify::verify(&lowered)
        .map_err(|error| format!("IR verification failed: {error}"))?;
    let lowered_text = forge_ir::print::print_function(&lowered);
    let mut optimized = forge_ir::lower::lower(&typed);
    forge_opt::optimize(&mut optimized);
    forge_ir::verify::verify(&optimized)
        .map_err(|error| format!("optimized IR verification failed: {error}"))?;
    let optimized_text = forge_ir::print::print_function(&optimized);
    let ir_stages = format!(
        "[{{\"name\":\"lowered\",\"text\":{}}},{{\"name\":\"optimized\",\"text\":{}}}]",
        json_string(&lowered_text),
        json_string(&optimized_text)
    );
    let cfg = cfg_dot(&optimized);
    Ok(format!(
        r#""ir_stages":{ir_stages},"cfg":{}"#,
        json_string(&cfg)
    ))
}

fn cfg_dot(function: &forge_ir::Function) -> String {
    use std::fmt::Write;

    let mut dot = String::from("digraph forge_cfg {\n");
    for (index, block) in function.blocks.iter().enumerate() {
        writeln!(
            dot,
            "  block{index} [label=\"block{index}\\n{} instructions\"];",
            block.insts.len()
        )
        .expect("writing to a String cannot fail");
        match &block.term {
            Some(forge_ir::Terminator::Jump(target)) => {
                writeln!(dot, "  block{index} -> block{};", target.0)
                    .expect("writing to a String cannot fail");
            }
            Some(forge_ir::Terminator::Branch { then_, else_, .. }) => {
                writeln!(dot, "  block{index} -> block{} [label=\"then\"];", then_.0)
                    .expect("writing to a String cannot fail");
                writeln!(dot, "  block{index} -> block{} [label=\"else\"];", else_.0)
                    .expect("writing to a String cannot fail");
            }
            Some(forge_ir::Terminator::Return(_)) | None => {}
        }
    }
    dot.push('}');
    dot
}

fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", character as u32))
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

#[wasm_bindgen]
pub fn cpu_features_wasm() -> String {
    cpu_features().to_string()
}

#[wasm_bindgen]
pub fn benchmark(source: &str, sizes: &[u32]) -> String {
    benchmark_json(source, sizes)
}

/// Returns a compact JSON-shaped status string without requiring serde in the
/// WASM bundle. The full structured artifact API is a separate workbench
/// boundary; this function gives editors an immediate diagnostics hook.
#[wasm_bindgen]
pub fn parse_and_check(source: &str) -> String {
    let (tokens, lex_diags) = forge_syntax::lexer::lex(source);
    if !lex_diags.is_empty() {
        return diagnostic_report("lex", &lex_diags);
    }
    let (ast, parse_diags) = forge_syntax::parser::parse(&tokens);
    if !parse_diags.is_empty() {
        return diagnostic_report("parse", &parse_diags);
    }
    let typed = match forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast)) {
        Ok(typed) => typed,
        Err(diags) => return diagnostic_report("type", &diags),
    };
    let root = ast_json(&typed.ast, typed.ast.root);
    let params = typed
        .params
        .iter()
        .map(|(name, ty)| {
            format!(
                "{{\"name\":{},\"type\":{}}}",
                json_string(name),
                json_string(&format!("{ty:?}").to_lowercase())
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"ok":true,"stage":"checked","ast":{},"parameters":[{}],"result_type":{}}}"#,
        root,
        params,
        json_string(&format!("{:?}", typed.types[typed.ast.root.index()]).to_lowercase())
    )
}

fn diagnostic_report(stage: &str, diagnostics: &[forge_syntax::Diagnostic]) -> String {
    let diagnostics = diagnostics
        .iter()
        .map(diagnostic_json)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"ok":false,"stage":{},"diagnostics":[{}]}}"#,
        json_string(stage),
        diagnostics
    )
}

fn diagnostic_json(diagnostic: &forge_syntax::Diagnostic) -> String {
    let secondary = diagnostic
        .secondary
        .iter()
        .map(label_json)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"message":{},"primary":{},"secondary":[{}]}}"#,
        json_string(&diagnostic.message),
        label_json(&diagnostic.primary),
        secondary
    )
}

fn label_json(label: &forge_syntax::diagnostic::Label) -> String {
    format!(
        r#"{{"start":{},"end":{},"message":{}}}"#,
        label.span.start,
        label.span.end,
        json_string(&label.message)
    )
}

fn ast_json(ast: &forge_syntax::ast::Ast, idx: forge_syntax::ast::ExprIdx) -> String {
    use forge_syntax::ast::Expr;

    let span = ast.span(idx);
    let span_json = format!(r#""span":{{"start":{},"end":{}}}"#, span.start, span.end);
    match ast.get(idx) {
        Expr::Float(value) => format!(r#"{{"kind":"float","value":{},{} }}"#, value, span_json),
        Expr::Int(value) => format!(r#"{{"kind":"int","value":{},{} }}"#, value, span_json),
        Expr::Bool(value) => format!(r#"{{"kind":"bool","value":{},{} }}"#, value, span_json),
        Expr::Ident(name) => format!(
            r#"{{"kind":"ident","name":{},{} }}"#,
            json_string(name),
            span_json
        ),
        Expr::Unary { op, operand } => format!(
            r#"{{"kind":"unary","op":{},"operand":{},{} }}"#,
            json_string(&format!("{op:?}").to_lowercase()),
            ast_json(ast, *operand),
            span_json
        ),
        Expr::Binary { op, lhs, rhs } => format!(
            r#"{{"kind":"binary","op":{},"lhs":{},"rhs":{},{} }}"#,
            json_string(&format!("{op:?}").to_lowercase()),
            ast_json(ast, *lhs),
            ast_json(ast, *rhs),
            span_json
        ),
        Expr::Call { callee, args } => format!(
            r#"{{"kind":"call","callee":{},"args":[{}],{} }}"#,
            json_string(callee),
            args.iter()
                .map(|arg| ast_json(ast, *arg))
                .collect::<Vec<_>>()
                .join(","),
            span_json
        ),
        Expr::Index { base, index } => format!(
            r#"{{"kind":"index","base":{},"index":{},{} }}"#,
            ast_json(ast, *base),
            ast_json(ast, *index),
            span_json
        ),
        Expr::If { cond, then_, else_ } => format!(
            r#"{{"kind":"if","cond":{},"then":{},"else":{},{} }}"#,
            ast_json(ast, *cond),
            ast_json(ast, *then_),
            ast_json(ast, *else_),
            span_json
        ),
        Expr::Let { name, value, body } => format!(
            r#"{{"kind":"let","name":{},"value":{},"body":{},{} }}"#,
            json_string(name),
            ast_json(ast, *value),
            ast_json(ast, *body),
            span_json
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_status_reports_success_and_diagnostics() {
        let success = parse_and_check("x + 1.0");
        assert!(success.contains(r#""ok":true"#));
        assert!(success.contains(r#""kind":"binary""#));
        assert!(success.contains(r#""start":0,"end":7"#));
        let error = parse_and_check("x +");
        assert!(error.contains(r#""ok":false"#));
        assert!(error.contains(r#""stage":"parse""#));
        assert!(error.contains(r#""diagnostics":["#));
    }

    #[test]
    fn browser_artifact_reports_signature_and_bytes() {
        let artifact = compile_artifact_json("x + y");
        assert!(artifact.contains(r#""ok":true"#));
        assert!(artifact.contains(r#""parameter_types":["f64","f64"]"#));
        assert!(artifact.contains(r#""required_imports":[]"#));
        assert!(artifact.contains(r#""wasm_bytes_len":"#));
        assert!(artifact.contains(r#""ir_stages":["#));
        assert!(artifact.contains(r#""cfg":"digraph forge_cfg"#));
        assert!(artifact.contains(r#""intervals":["#));
        assert!(artifact.contains(r#""asm":["#));
        assert!(artifact.contains(r#""stack_max_depth":"#));
        assert!(artifact.contains(r#""encoding":"wasm-stack""#));

        let error = compile_artifact_json("x +");
        assert!(error.contains(r#""ok":false"#));
        assert!(error.contains(r#""error":"#));

        let remainder = compile_artifact_json("x % y");
        assert!(remainder.contains(r#""required_imports":["forge.fmod"]"#));
    }

    #[test]
    fn browser_artifact_reports_stack_trace_for_control_flow_and_locals() {
        let artifact =
            compile_artifact_json("let t = x * x in if t < 9.0 then sqrt(t) else t + 1.0");
        assert!(artifact.contains(r#""ok":true"#));
        assert!(artifact.contains(r#""encoding":"wasm-stack""#));
        assert!(artifact.contains(r#""text":"local.set "#));
        assert!(artifact.contains(r#""text":"if""#));
        assert!(artifact.contains(r#""location":"stack["#));
    }

    #[test]
    fn browser_benchmark_reports_a_labeled_portable_baseline() {
        let report = benchmark_json("x + 1.0", &[0, 2]);
        assert!(report.contains(r#""ok":true"#));
        assert!(report.contains(r#""backend":"portable-interpreter"#));
        assert!(report.contains(r#""size":0,"calls":0"#));
        assert!(report.contains(r#""size":2,"calls":2"#));
    }

    #[test]
    fn target_artifacts_include_real_native_bytes_and_metadata() {
        let x86 = compile_target_artifact_json("x * x + 1.0", "x86_64");
        assert!(x86.contains(r#""ok":true"#));
        assert!(x86.contains(r#""target":"x86_64"#));
        assert!(x86.contains(r#""encoding":"x86-64"#));
        assert!(x86.contains(r#""bytes_len":"#));
        assert!(x86.contains(r#""intervals":["#));
        assert!(x86.contains(r#""asm":["#));
        assert!(!x86.contains(r#""bytes":"","text":"#));
        assert_eq!(x86.matches(r#""encoding":"#).count(), 1);

        let arm = compile_target_artifact_json("x + 1.0", "aarch64");
        assert!(arm.contains(r#""ok":true"#));
        assert!(arm.contains(r#""target":"aarch64"#));
        assert!(arm.contains(r#""encoding":"aarch64"#));
        assert!(arm.contains(r#""asm":["#));
        assert_eq!(arm.matches(r#""encoding":"#).count(), 1);
    }

    #[test]
    fn target_artifacts_reject_process_local_libm_and_unknown_targets() {
        let libm = compile_target_artifact_json("sin(x)", "x86_64");
        assert!(libm.contains(r#""ok":false"#));
        assert!(libm.contains("host libm calls"));

        let unknown = compile_target_artifact_json("x", "riscv64");
        assert!(unknown.contains(r#""ok":false"#));
        assert!(unknown.contains("unsupported target"));
    }
}
