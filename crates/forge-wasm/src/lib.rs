//! Portable WASM scalar backend and execution facade.
//!
//! `compile` emits a real, dependency-free WebAssembly module for the
//! all-f64 scalar language subset. Host libm calls and integer/bool function
//! signatures are rejected explicitly until their WASM ABI is specified.

use forge_ir::interp::{interpret, RtValue};
use forge_syntax::ast::{Ast, BinaryOp, Expr, ExprIdx, UnaryOp};
use forge_syntax::typeck::{Ty, TypedAst};
use std::collections::HashMap;

pub fn evaluate(source: &str, args: &[f64]) -> Result<f64, String> {
    let typed = typecheck_source(source)?;
    let function = forge_ir::lower::lower(&typed);
    if function.params.len() != args.len()
        || function
            .params
            .iter()
            .any(|(_, ty)| *ty != forge_ir::Ty::F64)
    {
        return Err("WASM facade currently accepts all-f64 parameters only".to_string());
    }
    let values = args.iter().copied().map(RtValue::F64).collect::<Vec<_>>();
    match interpret(&function, &values) {
        RtValue::F64(value) => Ok(value),
        _ => Err("WASM facade currently returns f64 only".to_string()),
    }
}

/// The structured result of WASM compilation. The byte vector is directly
/// consumable by `WebAssembly.instantiate`; the type metadata lets a browser
/// host validate arguments before invocation without reparsing the source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmArtifact {
    pub wasm_bytes: Vec<u8>,
    pub wasm_hex: String,
    pub parameter_types: Vec<String>,
    pub result_type: String,
}

impl WasmArtifact {
    pub fn parameter_count(&self) -> usize {
        self.parameter_types.len()
    }
}

fn typecheck_source(source: &str) -> Result<TypedAst, String> {
    let (tokens, lex_diags) = forge_syntax::lexer::lex(source);
    if !lex_diags.is_empty() {
        return Err(format!("lexing failed: {lex_diags:?}"));
    }
    let (ast, parse_diags) = forge_syntax::parser::parse(&tokens);
    if !parse_diags.is_empty() {
        return Err(format!("parsing failed: {parse_diags:?}"));
    }
    forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
        .map_err(|diags| format!("type checking failed: {diags:?}"))
}

fn type_name(ty: Ty) -> String {
    match ty {
        Ty::F64 => "f64",
        Ty::I64 => "i64",
        Ty::Bool => "bool",
    }
    .to_string()
}

/// Compiles a typed scalar expression to a one-function WASM module exporting
/// `eval`. Parameters and results use their real WASM value types (`f64`,
/// `i64`, or `i32` for Forge booleans), and each `let` binding becomes a local.
/// The resulting bytes can be passed directly to `WebAssembly.instantiate`.
pub fn compile(source: &str) -> Result<Vec<u8>, String> {
    Ok(compile_artifact(source)?.wasm_bytes)
}

/// Compiles a source expression and returns both executable bytes and the
/// signature metadata needed by a host-side ABI adapter.
pub fn compile_artifact(source: &str) -> Result<WasmArtifact, String> {
    let typed = typecheck_source(source)?;
    let params = typed
        .params
        .iter()
        .enumerate()
        .map(|(i, (name, _))| (name.clone(), i as u32))
        .collect::<HashMap<_, _>>();
    let mut lets = HashMap::new();
    let mut local_types = Vec::new();
    collect_lets(
        &typed,
        &typed.ast,
        typed.ast.root,
        typed.params.len() as u32,
        &mut lets,
        &mut local_types,
    );
    let mut expr = Vec::new();
    emit_expr(&typed, typed.ast.root, &params, &lets, &mut expr)?;
    expr.push(0x0b); // end

    let mut body = Vec::new();
    if lets.is_empty() {
        body.push(0);
    } else {
        body.push(1);
        push_uleb(local_types.len() as u32, &mut body);
        for ty in local_types {
            body.push(1); // one local in each declaration group
            body.push(wasm_valtype(ty));
        }
    }
    body.extend(expr);

    let mut code_body = Vec::new();
    push_uleb(body.len() as u32, &mut code_body);
    code_body.extend(body);

    let mut module = b"\0asm\x01\0\0\0".to_vec();
    let mut types = vec![1, 0x60];
    push_uleb(params.len() as u32, &mut types);
    types.extend(typed.params.iter().map(|(_, ty)| wasm_valtype(*ty)));
    types.push(1);
    types.push(wasm_valtype(typed.types[typed.ast.root.index()]));
    section(1, types, &mut module);
    section(3, vec![1, 0], &mut module);
    let export = vec![1, 4, b'e', b'v', b'a', b'l', 0, 0];
    section(7, export, &mut module);
    let mut code = vec![1];
    code.extend(code_body);
    section(10, code, &mut module);
    let wasm_hex = module
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(WasmArtifact {
        wasm_bytes: module,
        wasm_hex,
        parameter_types: typed.params.iter().map(|(_, ty)| type_name(*ty)).collect(),
        result_type: type_name(typed.types[typed.ast.root.index()]),
    })
}

fn collect_lets(
    typed: &TypedAst,
    ast: &Ast,
    idx: ExprIdx,
    first_local: u32,
    lets: &mut HashMap<String, u32>,
    local_types: &mut Vec<Ty>,
) {
    match ast.get(idx) {
        Expr::Let { name, value, body } => {
            let local = first_local + lets.len() as u32;
            lets.insert(name.clone(), local);
            local_types.push(typed.types[value.index()]);
            collect_lets(typed, ast, *value, first_local, lets, local_types);
            collect_lets(typed, ast, *body, first_local, lets, local_types);
        }
        Expr::Unary { operand, .. } => {
            collect_lets(typed, ast, *operand, first_local, lets, local_types)
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_lets(typed, ast, *lhs, first_local, lets, local_types);
            collect_lets(typed, ast, *rhs, first_local, lets, local_types);
        }
        Expr::Call { args, .. } => {
            for arg in args {
                collect_lets(typed, ast, *arg, first_local, lets, local_types);
            }
        }
        Expr::If { cond, then_, else_ } => {
            collect_lets(typed, ast, *cond, first_local, lets, local_types);
            collect_lets(typed, ast, *then_, first_local, lets, local_types);
            collect_lets(typed, ast, *else_, first_local, lets, local_types);
        }
        Expr::Float(_) | Expr::Int(_) | Expr::Bool(_) | Expr::Ident(_) => {}
    }
}

fn emit_expr(
    typed: &TypedAst,
    idx: ExprIdx,
    params: &HashMap<String, u32>,
    lets: &HashMap<String, u32>,
    out: &mut Vec<u8>,
) -> Result<(), String> {
    let ast = &typed.ast;
    match ast.get(idx) {
        Expr::Float(value) => {
            out.push(0x44);
            out.extend(value.to_le_bytes());
        }
        Expr::Int(value) => {
            out.push(0x42);
            push_sleb(*value, out);
        }
        Expr::Bool(value) => {
            out.extend([0x41, u8::from(*value)]);
        }
        Expr::Ident(name) => {
            let local = params
                .get(name)
                .or_else(|| lets.get(name))
                .ok_or_else(|| format!("unknown local {name}"))?;
            out.push(0x20);
            push_uleb(*local, out);
        }
        Expr::Unary { op, operand } => match (op, typed.types[idx.index()]) {
            (UnaryOp::Neg, Ty::F64) => {
                emit_expr(typed, *operand, params, lets, out)?;
                out.push(0x9a);
            }
            (UnaryOp::Neg, Ty::I64) => {
                out.extend([0x42, 0]);
                emit_expr(typed, *operand, params, lets, out)?;
                out.push(0x7d); // i64.sub
            }
            (UnaryOp::Not, Ty::Bool) => {
                emit_expr(typed, *operand, params, lets, out)?;
                out.push(0x45); // i32.eqz
            }
            (UnaryOp::BitNot, Ty::I64) => {
                emit_expr(typed, *operand, params, lets, out)?;
                out.push(0x42);
                push_sleb(-1, out);
                out.push(0x85); // i64.xor
            }
            _ => return Err("invalid typed unary operation".to_string()),
        },
        Expr::Binary { op, lhs, rhs } => {
            emit_expr(typed, *lhs, params, lets, out)?;
            emit_expr(typed, *rhs, params, lets, out)?;
            let lhs_ty = typed.types[lhs.index()];
            out.push(match lhs_ty {
                Ty::F64 => match op {
                    BinaryOp::Add => 0xa0,
                    BinaryOp::Sub => 0xa1,
                    BinaryOp::Mul => 0xa2,
                    BinaryOp::Div => 0xa3,
                    BinaryOp::Eq => 0x61,
                    BinaryOp::Ne => 0x62,
                    BinaryOp::Lt => 0x63,
                    BinaryOp::Gt => 0x64,
                    BinaryOp::Le => 0x65,
                    BinaryOp::Ge => 0x66,
                    BinaryOp::Rem => {
                        return Err("WASM has no scalar f64 remainder opcode".to_string())
                    }
                    _ => return Err("invalid f64 binary operation".to_string()),
                },
                Ty::I64 => match op {
                    BinaryOp::Add => 0x7c,
                    BinaryOp::Sub => 0x7d,
                    BinaryOp::Mul => 0x7e,
                    BinaryOp::Div => 0x7f,
                    BinaryOp::Rem => 0x81,
                    BinaryOp::BitAnd => 0x83,
                    BinaryOp::BitOr => 0x84,
                    BinaryOp::BitXor => 0x85,
                    BinaryOp::Shl => 0x86,
                    BinaryOp::Shr => 0x88,
                    BinaryOp::Eq => 0x51,
                    BinaryOp::Ne => 0x52,
                    BinaryOp::Lt => 0x53,
                    BinaryOp::Gt => 0x55,
                    BinaryOp::Le => 0x57,
                    BinaryOp::Ge => 0x59,
                    _ => return Err("invalid i64 binary operation".to_string()),
                },
                Ty::Bool => match op {
                    BinaryOp::And => 0x71,
                    BinaryOp::Or => 0x72,
                    BinaryOp::Eq => 0x46,
                    BinaryOp::Ne => 0x47,
                    _ => return Err("invalid bool binary operation".to_string()),
                },
            });
        }
        Expr::Call { callee, args } => match (callee.as_str(), args.as_slice()) {
            ("min", [lhs, rhs]) | ("max", [lhs, rhs]) => {
                emit_expr(typed, *lhs, params, lets, out)?;
                emit_expr(typed, *rhs, params, lets, out)?;
                out.push(if callee == "min" { 0xa4 } else { 0xa5 });
            }
            ("fma", [a, b, c]) => {
                emit_expr(typed, *a, params, lets, out)?;
                emit_expr(typed, *b, params, lets, out)?;
                out.push(0xa2); // f64.mul
                emit_expr(typed, *c, params, lets, out)?;
                out.push(0xa0); // f64.add
            }
            (name, [operand]) => {
                emit_expr(typed, *operand, params, lets, out)?;
                out.push(match name {
                    "sqrt" => 0x9f,
                    "abs" => 0x99,
                    "floor" => 0x9c,
                    "ceil" => 0x9b,
                    "round" => 0x9e,
                    "trunc" => 0x9d,
                    _ => {
                        return Err(format!(
                            "WASM backend has no inline implementation for {name}"
                        ))
                    }
                });
            }
            (_, _) => {
                return Err(format!(
                    "WASM backend does not emit call {callee} with {} argument(s)",
                    args.len()
                ))
            }
        },
        Expr::If { cond, then_, else_ } => {
            emit_expr(typed, *cond, params, lets, out)?;
            out.extend([0x04, wasm_valtype(typed.types[idx.index()])]);
            emit_expr(typed, *then_, params, lets, out)?;
            out.push(0x05);
            emit_expr(typed, *else_, params, lets, out)?;
            out.push(0x0b);
        }
        Expr::Let { name, value, body } => {
            emit_expr(typed, *value, params, lets, out)?;
            out.push(0x21);
            push_uleb(lets[name], out);
            emit_expr(typed, *body, params, lets, out)?;
        }
    }
    Ok(())
}

fn wasm_valtype(ty: Ty) -> u8 {
    match ty {
        Ty::F64 => 0x7c,
        Ty::I64 => 0x7e,
        Ty::Bool => 0x7f,
    }
}

fn push_uleb(mut value: u32, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn push_sleb(mut value: i64, out: &mut Vec<u8>) {
    loop {
        let byte = (value as u8) & 0x7f;
        let sign_done = (value >> 6 == 0) || (value >> 6 == -1);
        value >>= 7;
        out.push(if sign_done { byte } else { byte | 0x80 });
        if sign_done {
            break;
        }
    }
}

fn section(id: u8, payload: Vec<u8>, module: &mut Vec<u8>) {
    module.push(id);
    push_uleb(payload.len() as u32, module);
    module.extend(payload);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_a_valid_wasm_header_and_eval_export() {
        let bytes = compile("x + 1.0").unwrap();
        assert_eq!(&bytes[..8], b"\0asm\x01\0\0\0");
        assert!(bytes.windows(4).any(|window| window == b"eval"));
    }

    #[test]
    fn portable_evaluation_matches_the_ir_interpreter() {
        assert_eq!(evaluate("let t = x * x in sqrt(t)", &[3.0]).unwrap(), 3.0);
    }

    #[test]
    fn emits_binary_min_and_max_calls() {
        let min = compile("min(x, y)").unwrap();
        let max = compile("max(x, y)").unwrap();
        assert!(min.contains(&0xa4));
        assert!(max.contains(&0xa5));
    }

    #[test]
    fn emits_integer_and_boolean_wasm_types() {
        let integer = compile("x & 1").unwrap();
        assert!(integer.contains(&0x7e)); // i64 parameter/result type
        assert!(integer.contains(&0x83)); // i64.and

        let boolean = compile("x == y").unwrap();
        assert!(boolean.contains(&0x7f)); // i32 result type for bool
        assert!(boolean.contains(&0x61)); // f64.eq
    }

    #[test]
    fn emits_typed_conditionals_and_fma() {
        let conditional = compile("if flag then 1 else 2").unwrap();
        assert!(conditional.contains(&0x04)); // if
        assert!(conditional.contains(&0x7e)); // i64 block result

        let fma = compile("fma(x, y, z)").unwrap();
        assert!(fma.contains(&0xa2)); // f64.mul
        assert!(fma.contains(&0xa0)); // f64.add
    }

    #[test]
    fn structured_artifact_contains_signature_and_hex() {
        let artifact = compile_artifact("x + y").unwrap();
        assert_eq!(artifact.parameter_types, ["f64", "f64"]);
        assert_eq!(artifact.result_type, "f64");
        assert_eq!(artifact.parameter_count(), 2);
        assert_eq!(
            artifact.wasm_hex.split_whitespace().count(),
            artifact.wasm_bytes.len()
        );
        assert_eq!(artifact.wasm_hex.split_whitespace().next(), Some("00"));
    }

    #[test]
    fn unsupported_libm_call_is_reported_before_emission() {
        let error = compile("sin(x)").unwrap_err();
        assert!(error.contains("inline implementation"));
    }
}
