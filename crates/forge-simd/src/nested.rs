//! Checked scalar evaluation for nested source-level array loops.
//!
//! Nested array indices may be arbitrary i64 expressions over the declared
//! loop indices and scalar broadcasts. They therefore cannot be represented by
//! the contiguous-load vector IR without turning every access into a gather.
//! This module evaluates the typed AST directly, preserving the interpreter's
//! value semantics while enforcing the flattened row-major bounds contract.

use forge_ir::interp::RtValue;
use forge_ir::CmpOp;
use forge_syntax::ast::{Ast, BinaryOp, Expr, ExprIdx, UnaryOp};
use forge_syntax::typeck::{typecheck_array, Ty, TypedArray};
use std::collections::HashMap;

#[derive(Debug, PartialEq)]
pub struct NestedArrayResult {
    pub values: Vec<f64>,
    pub dimensions: Vec<usize>,
}

/// Evaluates `@vectorize output[i, j, ...] = expression` over flattened,
/// row-major f64 columns. Broadcast arguments follow the same order as the
/// existing typed vectorized API: array columns are omitted, while scalar f64
/// and i64 parameters are supplied in declaration order.
pub fn evaluate_nested_vectorized(
    source: &str,
    columns: &[&[f64]],
    dimensions: &[usize],
    broadcasts: &[RtValue],
) -> Result<NestedArrayResult, String> {
    let typed = parse_nested_source(source)?;
    if dimensions.len() != typed.indices.len() {
        return Err(format!(
            "nested array dimensions require {} entries, got {}",
            typed.indices.len(),
            dimensions.len()
        ));
    }
    let elements = dimensions.iter().try_fold(1usize, |product, dimension| {
        product
            .checked_mul(*dimension)
            .ok_or_else(|| "nested array shape product overflows usize".to_string())
    })?;
    let column_count = typed
        .params
        .iter()
        .filter(|(_, ty)| *ty == Ty::ArrayF64)
        .count();
    if columns.len() != column_count {
        return Err(format!(
            "nested array evaluation requires {column_count} columns, got {}",
            columns.len()
        ));
    }
    if columns.iter().any(|column| column.len() != elements) {
        return Err(format!(
            "nested array columns must each contain exactly {elements} elements"
        ));
    }

    let broadcast_count = typed
        .params
        .iter()
        .filter(|(_, ty)| *ty != Ty::ArrayF64)
        .count();
    if broadcasts.len() != broadcast_count {
        return Err(format!(
            "nested array evaluation requires {broadcast_count} broadcasts, got {}",
            broadcasts.len()
        ));
    }
    let mut environment = HashMap::new();
    for (index, name) in typed.indices.iter().enumerate() {
        if dimensions[index] > i64::MAX as usize {
            return Err("nested array dimension does not fit in i64".to_string());
        }
        environment.insert(name.clone(), RtValue::I64(0));
    }
    let mut broadcast = 0usize;
    for (name, ty) in &typed.params {
        if *ty == Ty::ArrayF64 {
            continue;
        }
        let value = broadcasts[broadcast];
        let valid = matches!(
            (ty, value),
            (Ty::F64, RtValue::F64(_)) | (Ty::I64, RtValue::I64(_))
        );
        if !valid {
            return Err(format!(
                "broadcast `{name}` does not match its declared type {ty:?}"
            ));
        }
        environment.insert(name.clone(), value);
        broadcast += 1;
    }

    let mut values = Vec::with_capacity(elements);
    for flat in 0..elements {
        let mut remainder = flat;
        for (dimension, name) in dimensions.iter().zip(typed.indices.iter()).rev() {
            let coordinate = remainder % *dimension;
            remainder /= *dimension;
            environment.insert(name.clone(), RtValue::I64(coordinate as i64));
        }
        let value = eval_expr(
            &typed.ast,
            typed.ast.root,
            &environment,
            &typed.params,
            columns,
        )?;
        let RtValue::F64(value) = value else {
            return Err("nested array body did not evaluate to f64".to_string());
        };
        values.push(value);
    }
    Ok(NestedArrayResult {
        values,
        dimensions: dimensions.to_vec(),
    })
}

fn parse_nested_source(source: &str) -> Result<TypedArray, String> {
    let (tokens, lex_diags) = forge_syntax::lexer::lex(source);
    if !lex_diags.is_empty() {
        return Err(format!("lexing failed: {lex_diags:?}"));
    }
    let (program, parse_diags) = forge_syntax::array::parse(&tokens);
    if !parse_diags.is_empty() {
        return Err(format!("parsing failed: {parse_diags:?}"));
    }
    let program = program.ok_or_else(|| {
        "expected @vectorize output[index, ...] = expression declaration".to_string()
    })?;
    let typed = typecheck_array(program)
        .map_err(|diagnostics| format!("type checking failed: {diagnostics:?}"))?;
    if typed.indices.len() < 2 {
        return Err("nested evaluator requires at least two loop indices".to_string());
    }
    Ok(typed)
}

fn eval_expr(
    ast: &Ast,
    idx: ExprIdx,
    environment: &HashMap<String, RtValue>,
    params: &[(String, Ty)],
    columns: &[&[f64]],
) -> Result<RtValue, String> {
    match ast.get(idx).clone() {
        Expr::Float(value) => Ok(RtValue::F64(value)),
        Expr::Int(value) => Ok(RtValue::I64(value)),
        Expr::Bool(value) => Ok(RtValue::Bool(value)),
        Expr::Ident(name) => environment
            .get(&name)
            .copied()
            .ok_or_else(|| format!("unbound or unindexed array identifier `{name}`")),
        Expr::Unary { op, operand } => {
            let value = eval_expr(ast, operand, environment, params, columns)?;
            match (op, value) {
                (UnaryOp::Neg, RtValue::F64(value)) => Ok(RtValue::F64(-value)),
                (UnaryOp::Neg, RtValue::I64(value)) => Ok(RtValue::I64(value.wrapping_neg())),
                (UnaryOp::Not, RtValue::Bool(value)) => Ok(RtValue::Bool(!value)),
                (UnaryOp::BitNot, RtValue::I64(value)) => Ok(RtValue::I64(!value)),
                (op, value) => Err(format!("invalid {op:?} operand {value:?}")),
            }
        }
        Expr::Binary { op, lhs, rhs } => {
            let lhs = eval_expr(ast, lhs, environment, params, columns)?;
            let rhs = eval_expr(ast, rhs, environment, params, columns)?;
            eval_binary(op, lhs, rhs)
        }
        Expr::Call { callee, args } => {
            let values = args
                .iter()
                .map(|arg| eval_expr(ast, *arg, environment, params, columns))
                .collect::<Result<Vec<_>, _>>()?;
            eval_call(&callee, &values)
        }
        Expr::Index { base, index } => {
            let Expr::Ident(name) = ast.get(base) else {
                return Err("nested array loads must use a column identifier".to_string());
            };
            let source_param = params
                .iter()
                .position(|(param, ty)| param == name && *ty == Ty::ArrayF64)
                .ok_or_else(|| format!("unknown array column `{name}`"))?;
            let source = params[..source_param]
                .iter()
                .filter(|(_, ty)| *ty == Ty::ArrayF64)
                .count();
            let raw_index = eval_expr(ast, index, environment, params, columns)?;
            let RtValue::I64(raw_index) = raw_index else {
                return Err("nested array index must evaluate to i64".to_string());
            };
            let flat_index = usize::try_from(raw_index)
                .map_err(|_| format!("nested array index {raw_index} is negative"))?;
            let value = columns
                .get(source)
                .and_then(|column| column.get(flat_index))
                .copied()
                .ok_or_else(|| format!("nested array index {flat_index} is out of bounds"))?;
            Ok(RtValue::F64(value))
        }
        Expr::If { cond, then_, else_ } => {
            let RtValue::Bool(cond) = eval_expr(ast, cond, environment, params, columns)? else {
                return Err("nested array if condition must be bool".to_string());
            };
            eval_expr(
                ast,
                if cond { then_ } else { else_ },
                environment,
                params,
                columns,
            )
        }
        Expr::Let { name, value, body } => {
            let value = eval_expr(ast, value, environment, params, columns)?;
            let mut nested = environment.clone();
            nested.insert(name, value);
            eval_expr(ast, body, &nested, params, columns)
        }
    }
}

fn eval_binary(op: BinaryOp, lhs: RtValue, rhs: RtValue) -> Result<RtValue, String> {
    use BinaryOp::*;
    match op {
        Add | Sub | Mul | Div | Rem => eval_numeric(op, lhs, rhs),
        BitAnd | BitOr | BitXor => match (op, lhs, rhs) {
            (BitAnd, RtValue::I64(a), RtValue::I64(b)) => Ok(RtValue::I64(a & b)),
            (BitOr, RtValue::I64(a), RtValue::I64(b)) => Ok(RtValue::I64(a | b)),
            (BitXor, RtValue::I64(a), RtValue::I64(b)) => Ok(RtValue::I64(a ^ b)),
            (BitAnd, RtValue::Bool(a), RtValue::Bool(b)) => Ok(RtValue::Bool(a & b)),
            (BitOr, RtValue::Bool(a), RtValue::Bool(b)) => Ok(RtValue::Bool(a | b)),
            (BitXor, RtValue::Bool(a), RtValue::Bool(b)) => Ok(RtValue::Bool(a ^ b)),
            _ => Err("invalid bitwise operands".to_string()),
        },
        And => match (lhs, rhs) {
            (RtValue::Bool(a), RtValue::Bool(b)) => Ok(RtValue::Bool(a && b)),
            _ => Err("invalid logical-and operands".to_string()),
        },
        Or => match (lhs, rhs) {
            (RtValue::Bool(a), RtValue::Bool(b)) => Ok(RtValue::Bool(a || b)),
            _ => Err("invalid logical-or operands".to_string()),
        },
        Shl | Shr => match (lhs, rhs) {
            (RtValue::I64(a), RtValue::I64(b)) if matches!(op, Shl) => {
                Ok(RtValue::I64(a.wrapping_shl(b as u32)))
            }
            (RtValue::I64(a), RtValue::I64(b)) => {
                Ok(RtValue::I64((a as u64).wrapping_shr(b as u32) as i64))
            }
            _ => Err("invalid shift operands".to_string()),
        },
        Eq | Ne | Lt | Le | Gt | Ge => {
            let op = match op {
                Eq => CmpOp::Eq,
                Ne => CmpOp::Ne,
                Lt => CmpOp::Lt,
                Le => CmpOp::Le,
                Gt => CmpOp::Gt,
                Ge => CmpOp::Ge,
                _ => unreachable!(),
            };
            Ok(RtValue::Bool(compare(op, lhs, rhs)?))
        }
    }
}

fn eval_numeric(op: BinaryOp, lhs: RtValue, rhs: RtValue) -> Result<RtValue, String> {
    let result = match (lhs, rhs) {
        (RtValue::F64(a), RtValue::F64(b)) => match op {
            BinaryOp::Add => RtValue::F64(a + b),
            BinaryOp::Sub => RtValue::F64(a - b),
            BinaryOp::Mul => RtValue::F64(a * b),
            BinaryOp::Div => RtValue::F64(a / b),
            BinaryOp::Rem => RtValue::F64(a % b),
            _ => unreachable!(),
        },
        (RtValue::I64(a), RtValue::I64(b)) => match op {
            BinaryOp::Add => RtValue::I64(a.wrapping_add(b)),
            BinaryOp::Sub => RtValue::I64(a.wrapping_sub(b)),
            BinaryOp::Mul => RtValue::I64(a.wrapping_mul(b)),
            BinaryOp::Div => RtValue::I64(a.wrapping_div(b)),
            BinaryOp::Rem => RtValue::I64(a.wrapping_rem(b)),
            _ => unreachable!(),
        },
        (RtValue::F64(a), RtValue::I64(b)) => {
            let b = b as f64;
            RtValue::F64(match op {
                BinaryOp::Add => a + b,
                BinaryOp::Sub => a - b,
                BinaryOp::Mul => a * b,
                BinaryOp::Div => a / b,
                BinaryOp::Rem => a % b,
                _ => unreachable!(),
            })
        }
        (RtValue::I64(a), RtValue::F64(b)) => {
            let a = a as f64;
            RtValue::F64(match op {
                BinaryOp::Add => a + b,
                BinaryOp::Sub => a - b,
                BinaryOp::Mul => a * b,
                BinaryOp::Div => a / b,
                BinaryOp::Rem => a % b,
                _ => unreachable!(),
            })
        }
        _ => return Err("invalid numeric operands".to_string()),
    };
    Ok(result)
}

fn compare(op: CmpOp, lhs: RtValue, rhs: RtValue) -> Result<bool, String> {
    let result = match (op, lhs, rhs) {
        (CmpOp::Eq, RtValue::F64(a), RtValue::F64(b)) => a == b,
        (CmpOp::Ne, RtValue::F64(a), RtValue::F64(b)) => a != b,
        (CmpOp::Lt, RtValue::F64(a), RtValue::F64(b)) => a < b,
        (CmpOp::Le, RtValue::F64(a), RtValue::F64(b)) => a <= b,
        (CmpOp::Gt, RtValue::F64(a), RtValue::F64(b)) => a > b,
        (CmpOp::Ge, RtValue::F64(a), RtValue::F64(b)) => a >= b,
        (CmpOp::Eq, RtValue::I64(a), RtValue::I64(b)) => a == b,
        (CmpOp::Ne, RtValue::I64(a), RtValue::I64(b)) => a != b,
        (CmpOp::Lt, RtValue::I64(a), RtValue::I64(b)) => a < b,
        (CmpOp::Le, RtValue::I64(a), RtValue::I64(b)) => a <= b,
        (CmpOp::Gt, RtValue::I64(a), RtValue::I64(b)) => a > b,
        (CmpOp::Ge, RtValue::I64(a), RtValue::I64(b)) => a >= b,
        (CmpOp::Eq, RtValue::Bool(a), RtValue::Bool(b)) => a == b,
        (CmpOp::Ne, RtValue::Bool(a), RtValue::Bool(b)) => a != b,
        _ => return Err("invalid comparison operands".to_string()),
    };
    Ok(result)
}

fn eval_call(callee: &str, args: &[RtValue]) -> Result<RtValue, String> {
    let f64_arg = |index: usize| match args.get(index) {
        Some(RtValue::F64(value)) => Ok(*value),
        Some(RtValue::I64(value)) => Ok(*value as f64),
        _ => Err(format!("{callee}() expects numeric arguments")),
    };
    let value = match callee {
        "sqrt" => f64_arg(0)?.sqrt(),
        "abs" => f64_arg(0)?.abs(),
        "floor" => f64_arg(0)?.floor(),
        "ceil" => f64_arg(0)?.ceil(),
        "round" => f64_arg(0)?.round(),
        "trunc" => f64_arg(0)?.trunc(),
        "sin" => f64_arg(0)?.sin(),
        "cos" => f64_arg(0)?.cos(),
        "tan" => f64_arg(0)?.tan(),
        "exp" => f64_arg(0)?.exp(),
        "log" => f64_arg(0)?.ln(),
        "min" => forge_min(f64_arg(0)?, f64_arg(1)?),
        "max" => forge_max(f64_arg(0)?, f64_arg(1)?),
        "pow" => f64_arg(0)?.powf(f64_arg(1)?),
        "fma" => f64_arg(0)?.mul_add(f64_arg(1)?, f64_arg(2)?),
        _ => return Err(format!("unknown intrinsic `{callee}`")),
    };
    Ok(RtValue::F64(value))
}

fn forge_min(x: f64, y: f64) -> f64 {
    if x.is_nan() {
        return y;
    }
    if y.is_nan() {
        return x;
    }
    if x < y {
        x
    } else if y < x {
        y
    } else if x == 0.0 && y == 0.0 {
        if x.is_sign_negative() || y.is_sign_negative() {
            -0.0
        } else {
            0.0
        }
    } else {
        x
    }
}

fn forge_max(x: f64, y: f64) -> f64 {
    if x.is_nan() {
        return y;
    }
    if y.is_nan() {
        return x;
    }
    if x > y {
        x
    } else if y > x {
        y
    } else if x == 0.0 && y == 0.0 {
        if x.is_sign_negative() && y.is_sign_negative() {
            -0.0
        } else {
            0.0
        }
    } else {
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_nested_row_major_source() {
        let left = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let result = evaluate_nested_vectorized(
            "@vectorize result[i, j] = a[i * width + j] + bias",
            &[&left],
            &[2, 3],
            &[RtValue::I64(3), RtValue::F64(10.0)],
        )
        .unwrap();
        assert_eq!(result.dimensions, vec![2, 3]);
        assert_eq!(result.values, vec![10.0, 11.0, 12.0, 13.0, 14.0, 15.0]);
    }

    #[test]
    fn rejects_shape_mismatch_before_evaluation() {
        let error = evaluate_nested_vectorized(
            "@vectorize result[i, j] = a[i + j]",
            &[&[1.0, 2.0]],
            &[2, 2],
            &[],
        )
        .unwrap_err();
        assert!(error.contains("exactly 4 elements"));
    }

    #[test]
    fn rejects_out_of_bounds_nested_gather() {
        let error = evaluate_nested_vectorized(
            "@vectorize result[i, j] = a[i * width + j + shift]",
            &[&[1.0, 2.0, 3.0, 4.0]],
            &[2, 2],
            &[RtValue::I64(2), RtValue::I64(1)],
        )
        .unwrap_err();
        assert!(error.contains("out of bounds"));
    }
}
