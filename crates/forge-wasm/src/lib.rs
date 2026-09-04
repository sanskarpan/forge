//! Portable WASM scalar backend and execution facade.
//!
//! `compile` emits a real, dependency-free WebAssembly module for the
//! all-f64 scalar language subset. Host libm calls and integer/bool function
//! signatures are rejected explicitly until their WASM ABI is specified.

use forge_ir::interp::{interpret, RtValue};
use forge_syntax::ast::{Ast, BinaryOp, Expr, ExprIdx, UnaryOp};
use std::collections::HashMap;

pub fn evaluate(source: &str, args: &[f64]) -> Result<f64, String> {
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

/// Compiles an all-f64 expression to a one-function WASM module exporting
/// `eval`. The module uses the standard scalar f64 opcodes and a local for
/// each `let` binding; it can be passed directly to `WebAssembly.instantiate`.
pub fn compile(source: &str) -> Result<Vec<u8>, String> {
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
    if typed
        .params
        .iter()
        .any(|(_, ty)| *ty != forge_syntax::typeck::Ty::F64)
        || typed.types[typed.ast.root.index()] != forge_syntax::typeck::Ty::F64
    {
        return Err(
            "WASM backend currently accepts all-f64 parameters and returns f64".to_string(),
        );
    }
    let params = typed
        .params
        .iter()
        .enumerate()
        .map(|(i, (name, _))| (name.clone(), i as u32))
        .collect::<HashMap<_, _>>();
    let mut lets = HashMap::new();
    collect_lets(
        &typed.ast,
        typed.ast.root,
        typed.params.len() as u32,
        &mut lets,
    );
    let mut expr = Vec::new();
    emit_expr(&typed.ast, typed.ast.root, &params, &lets, &mut expr)?;
    expr.push(0x0b); // end

    let mut body = Vec::new();
    if lets.is_empty() {
        body.push(0);
    } else {
        body.push(1);
        push_uleb(lets.len() as u32, &mut body);
        body.push(0x7c); // f64 local type
    }
    body.extend(expr);

    let mut code_body = Vec::new();
    push_uleb(body.len() as u32, &mut code_body);
    code_body.extend(body);

    let mut module = b"\0asm\x01\0\0\0".to_vec();
    let mut types = vec![1, 0x60];
    push_uleb(params.len() as u32, &mut types);
    types.extend(vec![0x7c; params.len()]);
    types.push(1);
    types.push(0x7c);
    section(1, types, &mut module);
    section(3, vec![1, 0], &mut module);
    let export = vec![1, 4, b'e', b'v', b'a', b'l', 0, 0];
    section(7, export, &mut module);
    let mut code = vec![1];
    code.extend(code_body);
    section(10, code, &mut module);
    Ok(module)
}

fn collect_lets(ast: &Ast, idx: ExprIdx, first_local: u32, lets: &mut HashMap<String, u32>) {
    match ast.get(idx) {
        Expr::Let { name, value, body } => {
            let local = first_local + lets.len() as u32;
            lets.insert(name.clone(), local);
            collect_lets(ast, *value, first_local, lets);
            collect_lets(ast, *body, first_local, lets);
        }
        Expr::Unary { operand, .. } => collect_lets(ast, *operand, first_local, lets),
        Expr::Binary { lhs, rhs, .. } => {
            collect_lets(ast, *lhs, first_local, lets);
            collect_lets(ast, *rhs, first_local, lets);
        }
        Expr::Call { args, .. } => {
            for arg in args {
                collect_lets(ast, *arg, first_local, lets);
            }
        }
        Expr::If { cond, then_, else_ } => {
            collect_lets(ast, *cond, first_local, lets);
            collect_lets(ast, *then_, first_local, lets);
            collect_lets(ast, *else_, first_local, lets);
        }
        Expr::Float(_) | Expr::Int(_) | Expr::Bool(_) | Expr::Ident(_) => {}
    }
}

fn emit_expr(
    ast: &Ast,
    idx: ExprIdx,
    params: &HashMap<String, u32>,
    lets: &HashMap<String, u32>,
    out: &mut Vec<u8>,
) -> Result<(), String> {
    match ast.get(idx) {
        Expr::Float(value) => {
            out.push(0x44);
            out.extend(value.to_le_bytes());
        }
        Expr::Ident(name) => {
            let local = params
                .get(name)
                .or_else(|| lets.get(name))
                .ok_or_else(|| format!("unknown local {name}"))?;
            out.push(0x20);
            push_uleb(*local, out);
        }
        Expr::Unary {
            op: UnaryOp::Neg,
            operand,
        } => {
            emit_expr(ast, *operand, params, lets, out)?;
            out.push(0x9a);
        }
        Expr::Unary { .. } => {
            return Err("boolean and integer unary operations are not f64 WASM ops".to_string())
        }
        Expr::Binary { op, lhs, rhs } => {
            emit_expr(ast, *lhs, params, lets, out)?;
            emit_expr(ast, *rhs, params, lets, out)?;
            out.push(match op {
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
                BinaryOp::Rem => return Err("WASM has no scalar f64 remainder opcode".to_string()),
                _ => {
                    return Err(
                        "bitwise, logical, and shift operations need a non-f64 WASM path"
                            .to_string(),
                    )
                }
            });
        }
        Expr::Call { callee, args } => {
            if args.len() != 1 {
                return Err(format!(
                    "WASM backend does not emit multi-argument call {callee}"
                ));
            }
            emit_expr(ast, args[0], params, lets, out)?;
            out.push(match callee.as_str() {
                "sqrt" => 0x9f,
                "abs" => 0x99,
                "floor" => 0x9c,
                "ceil" => 0x9b,
                "round" => 0x9e,
                "trunc" => 0x9d,
                _ => {
                    return Err(format!(
                        "WASM backend has no inline implementation for {callee}"
                    ))
                }
            });
        }
        Expr::If { cond, then_, else_ } => {
            emit_expr(ast, *cond, params, lets, out)?;
            out.extend([0x04, 0x7c]);
            emit_expr(ast, *then_, params, lets, out)?;
            out.push(0x05);
            emit_expr(ast, *else_, params, lets, out)?;
            out.push(0x0b);
        }
        Expr::Let { name, value, body } => {
            emit_expr(ast, *value, params, lets, out)?;
            out.push(0x21);
            push_uleb(lets[name], out);
            emit_expr(ast, *body, params, lets, out)?;
        }
        Expr::Int(_) | Expr::Bool(_) => {
            return Err("WASM backend currently emits f64 expressions only".to_string())
        }
    }
    Ok(())
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
    fn unsupported_libm_call_is_reported_before_emission() {
        let error = compile("sin(x)").unwrap_err();
        assert!(error.contains("inline implementation"));
    }
}
