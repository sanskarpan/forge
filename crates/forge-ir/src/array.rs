//! Inspectable memory and loop IR for source-level `@vectorize` programs.
//!
//! The element expression stays in the ordinary verified SSA [`Function`].
//! This companion IR describes the outer row loop explicitly, including the
//! induction value, each column load, and the output store. Keeping the two
//! layers separate lets native scalar compilation retain its existing ABI
//! while SIMD backends consume a precise memory envelope.

use crate::{Block, Function, Ty, Value};
use forge_syntax::ast::{Ast, Expr, ExprIdx};
use forge_syntax::typeck::{Ty as AstTy, TypedArray, TypedAst};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ArrayValue(pub u32);

/// Describes how a source-level array parameter is supplied to each row of
/// the vectorized loop. Columns are loaded at the induction index; scalar
/// broadcasts are splatted by the packed evaluator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrayParamKind {
    Column,
    ScalarBroadcast,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArrayInst {
    Induction {
        result: ArrayValue,
        start: usize,
        step: usize,
    },
    Load {
        result: Value,
        column: u32,
        index: ArrayValue,
        /// Element displacement from the logical loop index. The evaluator
        /// validates the resulting window before executing the loop.
        offset: i32,
    },
    Store {
        output: String,
        index: ArrayValue,
        value: Value,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrayTerminator {
    LoopIfLess {
        index: ArrayValue,
        bound: ArrayBound,
        body: Block,
        exit: Block,
    },
    Jump(Block),
    Return,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrayBound {
    InputLength,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrayBlock {
    pub insts: Vec<ArrayInst>,
    pub term: ArrayTerminator,
}

#[derive(Clone, Debug)]
pub struct ArrayFunction {
    pub element: Function,
    pub params: Vec<(String, Ty)>,
    pub param_kinds: Vec<ArrayParamKind>,
    /// For a column parameter, identifies the source column ordinal. A
    /// source column may have several element parameters when it is loaded at
    /// several constant offsets.
    pub param_sources: Vec<Option<u32>>,
    pub output: String,
    pub index: String,
    pub blocks: Vec<ArrayBlock>,
    pub entry: Block,
}

/// Verifies the small memory/control-flow envelope independently from the
/// scalar element verifier. This catches malformed loop edges, missing loads,
/// stores that target an unknown output, and values that are not defined by
/// the element function before a backend consumes the plan.
pub fn verify_array(function: &ArrayFunction) -> Result<(), String> {
    crate::verify::verify(&function.element)?;
    if function.blocks.len() != 3 || function.entry != Block(0) {
        return Err("array loop must contain entry, body, and exit blocks".to_string());
    }
    if function.params.len() != function.element.params.len()
        || function.param_kinds.len() != function.params.len()
        || function.param_sources.len() != function.params.len()
    {
        return Err("array parameter metadata does not match the element function".to_string());
    }
    let induction = match function.blocks[0].insts.as_slice() {
        [ArrayInst::Induction {
            result,
            start,
            step,
        }] if *start == 0 && *step > 0 => *result,
        _ => return Err("array entry must define one positive-step induction value".to_string()),
    };
    if !matches!(
        function.blocks[0].term,
        ArrayTerminator::LoopIfLess {
            index,
            bound: ArrayBound::InputLength,
            body: Block(1),
            exit: Block(2),
        } if index == induction
    ) {
        return Err("array entry has an invalid bounds-check edge".to_string());
    }
    if !matches!(function.blocks[1].term, ArrayTerminator::Jump(Block(0))) {
        return Err("array body must jump back to the loop header".to_string());
    }
    if !matches!(function.blocks[2].term, ArrayTerminator::Return) {
        return Err("array exit must return".to_string());
    }
    let mut loads = 0usize;
    let mut stores = 0usize;
    let column_count = function
        .param_sources
        .iter()
        .flatten()
        .max()
        .map_or(0, |column| *column as usize + 1);
    let mut loaded_params = vec![false; function.params.len()];
    for inst in &function.blocks[1].insts {
        match inst {
            ArrayInst::Load {
                result,
                column,
                index,
                ..
            } => {
                if *column as usize >= column_count {
                    return Err("array load references an unknown column".to_string());
                }
                let Some(crate::Inst::Param {
                    index: element_param,
                    ty: Ty::F64,
                }) = function.element.insts.get(result.0 as usize)
                else {
                    return Err("array load does not match an element parameter".to_string());
                };
                if *index != induction
                    || function.param_kinds.get(*element_param as usize)
                        != Some(&ArrayParamKind::Column)
                    || function.param_sources.get(*element_param as usize) != Some(&Some(*column))
                    || loaded_params.get_mut(*element_param as usize).is_none()
                {
                    return Err("array load does not match an element parameter".to_string());
                }
                loaded_params[*element_param as usize] = true;
                loads += 1;
            }
            ArrayInst::Store {
                output,
                index,
                value,
            } => {
                if output != &function.output || *index != induction {
                    return Err("array store does not match the loop output".to_string());
                }
                if function.element.insts.get(value.0 as usize).is_none() {
                    return Err("array store references an undefined element value".to_string());
                }
                stores += 1;
            }
            ArrayInst::Induction { .. } => {
                return Err("array induction must be defined in the loop header".to_string())
            }
        }
    }
    if loads != loaded_params.iter().filter(|loaded| **loaded).count()
        || loaded_params
            .iter()
            .enumerate()
            .any(|(param, loaded)| function.param_kinds[param] == ArrayParamKind::Column && !loaded)
        || stores != 1
    {
        return Err(
            "array body must load every column parameter and store exactly once".to_string(),
        );
    }
    Ok(())
}

/// Lowers a type-checked vectorized body into the memory/loop envelope and a
/// scalar element function. The current language contract intentionally
/// accepts indexed column addressing with a constant element displacement.
/// Unindexed f64 parameters remain scalar broadcasts in the element function
/// and are not represented by memory loads.
pub fn lower_array(typed: &TypedArray) -> Result<ArrayFunction, String> {
    if typed.indices.len() != 1 {
        return Err(
            "nested vectorized declarations require the nested-array evaluation entry point"
                .to_string(),
        );
    }
    if typed.params.iter().any(|(_, ty)| *ty == AstTy::I64) {
        return Err(
            "dynamic array offsets require the typed vectorized broadcast entry point".to_string(),
        );
    }
    let mut ast = typed.ast.clone();
    let root = ast.root;
    let mut column_offsets = Vec::new();
    rewrite_indexed_loads(&mut ast, root, &typed.index, &mut column_offsets)?;
    let mut element_params = Vec::new();
    let mut element_sources = Vec::new();
    for (source_column, (name, ty)) in typed.params.iter().enumerate() {
        if *ty == AstTy::ArrayF64 {
            for (_, _offset, synthetic) in column_offsets
                .iter()
                .filter(|(column_name, _, _)| column_name == name)
            {
                element_params.push((synthetic.clone(), AstTy::F64));
                element_sources.push(Some(source_column as u32));
            }
        } else {
            element_params.push((name.clone(), *ty));
            element_sources.push(None);
        }
    }
    let element_typed = TypedAst {
        ast,
        types: typed.types.clone(),
        params: element_params.clone(),
    };
    let element = crate::lower::lower(&element_typed);
    let induction = ArrayValue(0);
    let loads = column_offsets
        .iter()
        .enumerate()
        .map(|(load, (name, offset, synthetic))| {
            let source_column = typed
                .params
                .iter()
                .position(|(param, ty)| param == name && *ty == AstTy::ArrayF64)
                .ok_or_else(|| format!("missing indexed column parameter `{name}`"))?;
            let param = element_params
                .iter()
                .position(|(param, _)| param == synthetic)
                .ok_or_else(|| format!("missing element parameter for load {load}"))?;
            let result = element
                .insts
                .iter()
                .enumerate()
                .find_map(|(value, inst)| match inst {
                    crate::Inst::Param { index, .. } if *index == param as u32 => {
                        Some(Value(value as u32))
                    }
                    _ => None,
                })
                .ok_or_else(|| format!("missing element parameter for load {load}"))?;
            Ok(ArrayInst::Load {
                result,
                column: source_column as u32,
                index: induction,
                offset: *offset,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let result = element
        .blocks
        .iter()
        .find_map(|block| match block.term {
            Some(crate::Terminator::Return(value)) => Some(value),
            _ => None,
        })
        .ok_or_else(|| "array element function has no return".to_string())?;
    let body = Block(1);
    let exit = Block(2);
    let mut body_insts = loads;
    body_insts.push(ArrayInst::Store {
        output: typed.output.clone(),
        index: induction,
        value: result,
    });
    let function = ArrayFunction {
        element,
        params: element_params
            .iter()
            .map(|(name, _)| (name.clone(), Ty::F64))
            .collect(),
        param_kinds: element_params
            .iter()
            .zip(element_sources.iter())
            .map(|((_, ty), source)| match (ty, source) {
                (AstTy::F64, Some(_)) => ArrayParamKind::Column,
                (AstTy::F64, None) => ArrayParamKind::ScalarBroadcast,
                (other, _) => panic!("unexpected array parameter type {other:?}"),
            })
            .collect(),
        param_sources: element_sources,
        output: typed.output.clone(),
        index: typed.index.clone(),
        blocks: vec![
            ArrayBlock {
                insts: vec![ArrayInst::Induction {
                    result: induction,
                    start: 0,
                    step: 1,
                }],
                term: ArrayTerminator::LoopIfLess {
                    index: induction,
                    bound: ArrayBound::InputLength,
                    body,
                    exit,
                },
            },
            ArrayBlock {
                insts: body_insts,
                term: ArrayTerminator::Jump(Block(0)),
            },
            ArrayBlock {
                insts: Vec::new(),
                term: ArrayTerminator::Return,
            },
        ],
        entry: Block(0),
    };
    verify_array(&function)?;
    Ok(function)
}

fn rewrite_indexed_loads(
    ast: &mut Ast,
    idx: ExprIdx,
    induction_name: &str,
    column_offsets: &mut Vec<(String, i32, String)>,
) -> Result<(), String> {
    match ast.get(idx).clone() {
        Expr::Index { base, index } => {
            if !matches!(ast.get(base), Expr::Ident(_)) {
                return Err("vectorized loads must use a column identifier".to_string());
            }
            let offset = parse_index_offset(ast, index, induction_name)?;
            let Expr::Ident(name) = ast.get(base).clone() else {
                unreachable!()
            };
            let synthetic = format!("{name}%array_load_{}", column_offsets.len());
            column_offsets.push((name, offset, synthetic.clone()));
            ast.exprs[idx.index()] = Expr::Ident(synthetic);
        }
        Expr::Unary { operand, .. } => {
            rewrite_indexed_loads(ast, operand, induction_name, column_offsets)?
        }
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_indexed_loads(ast, lhs, induction_name, column_offsets)?;
            rewrite_indexed_loads(ast, rhs, induction_name, column_offsets)?;
        }
        Expr::Call { args, .. } => {
            for arg in args {
                rewrite_indexed_loads(ast, arg, induction_name, column_offsets)?;
            }
        }
        Expr::If { cond, then_, else_ } => {
            rewrite_indexed_loads(ast, cond, induction_name, column_offsets)?;
            rewrite_indexed_loads(ast, then_, induction_name, column_offsets)?;
            rewrite_indexed_loads(ast, else_, induction_name, column_offsets)?;
        }
        Expr::Let { value, body, .. } => {
            rewrite_indexed_loads(ast, value, induction_name, column_offsets)?;
            rewrite_indexed_loads(ast, body, induction_name, column_offsets)?;
        }
        Expr::Float(_) | Expr::Int(_) | Expr::Bool(_) | Expr::Ident(_) => {}
    }
    Ok(())
}

fn parse_index_offset(ast: &Ast, index: ExprIdx, induction_name: &str) -> Result<i32, String> {
    let offset = match ast.get(index) {
        Expr::Ident(name) if name == induction_name => Some(0),
        Expr::Binary {
            op: forge_syntax::ast::BinaryOp::Add,
            lhs,
            rhs,
        } => match (ast.get(*lhs), ast.get(*rhs)) {
            (Expr::Ident(name), _constant) if name == induction_name => constant_i64(ast, *rhs),
            (_constant, Expr::Ident(name)) if name == induction_name => constant_i64(ast, *lhs),
            _ => None,
        },
        Expr::Binary {
            op: forge_syntax::ast::BinaryOp::Sub,
            lhs,
            rhs,
        } if matches!(ast.get(*lhs), Expr::Ident(name) if name == induction_name) => {
            constant_i64(ast, *rhs).and_then(|value| value.checked_neg())
        }
        _ => None,
    }
    .ok_or_else(|| {
        "vectorized loads must use the induction variable plus a constant integer offset"
            .to_string()
    })?;
    i32::try_from(offset).map_err(|_| "vectorized load offset must fit in i32".to_string())
}

fn constant_i64(ast: &Ast, idx: ExprIdx) -> Option<i64> {
    match ast.get(idx) {
        Expr::Int(value) => Some(*value),
        Expr::Unary {
            op: forge_syntax::ast::UnaryOp::Neg,
            operand,
        } => constant_i64(ast, *operand).and_then(|value| value.checked_neg()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_syntax::array::parse;
    use forge_syntax::lexer::lex;
    use forge_syntax::typeck::typecheck_array;

    #[test]
    fn lowers_vectorize_to_explicit_memory_loop() {
        let (tokens, lex_diags) = lex("@vectorize result[i] = a[i] * b[i] + c[i]");
        assert!(lex_diags.is_empty(), "{lex_diags:?}");
        let (program, parse_diags) = parse(&tokens);
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let typed = typecheck_array(program.expect("vector program")).unwrap();
        let function = lower_array(&typed).unwrap();

        assert_eq!(function.blocks.len(), 3);
        assert_eq!(function.entry, Block(0));
        assert!(matches!(
            function.blocks[0].term,
            ArrayTerminator::LoopIfLess {
                bound: ArrayBound::InputLength,
                body: Block(1),
                exit: Block(2),
                ..
            }
        ));
        assert_eq!(
            function.blocks[1]
                .insts
                .iter()
                .filter(|inst| matches!(inst, ArrayInst::Load { .. }))
                .count(),
            3
        );
        assert!(matches!(
            function.blocks[1].insts.last(),
            Some(ArrayInst::Store { output, .. }) if output == "result"
        ));
        assert_eq!(function.element.params.len(), 3);
    }

    #[test]
    fn lowers_scalar_broadcast_without_an_array_load() {
        let (tokens, lex_diags) = lex("@vectorize result[i] = a[i] + scale");
        assert!(lex_diags.is_empty(), "{lex_diags:?}");
        let (program, parse_diags) = parse(&tokens);
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let typed = typecheck_array(program.expect("vector program")).unwrap();
        let function = lower_array(&typed).unwrap();

        assert_eq!(
            function.param_kinds,
            vec![ArrayParamKind::Column, ArrayParamKind::ScalarBroadcast]
        );
        assert_eq!(
            function.blocks[1]
                .insts
                .iter()
                .filter(|inst| matches!(inst, ArrayInst::Load { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn lowers_constant_index_offsets_into_verified_loads() {
        let (tokens, lex_diags) =
            lex("@vectorize result[i] = left[i + 2] + right[i - 1] + left[i + 2]");
        assert!(lex_diags.is_empty(), "{lex_diags:?}");
        let (program, parse_diags) = parse(&tokens);
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let typed = typecheck_array(program.expect("vector program")).unwrap();
        let function = lower_array(&typed).unwrap();
        let offsets = function.blocks[1]
            .insts
            .iter()
            .filter_map(|inst| match inst {
                ArrayInst::Load { column, offset, .. } => Some((*column, *offset)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(offsets, vec![(0, 2), (1, -1), (0, 2)]);
    }

    #[test]
    fn rejects_dynamic_index_offsets() {
        let source = "@vectorize result[i] = a[i + shift]";
        let (tokens, lex_diags) = lex(source);
        assert!(lex_diags.is_empty(), "{lex_diags:?}");
        let (program, parse_diags) = parse(&tokens);
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let Some(program) = program else {
            panic!("array parser rejected {source}");
        };
        if let Ok(typed) = typecheck_array(program) {
            assert!(
                lower_array(&typed).is_err(),
                "expected rejection for {source}"
            );
        }
    }

    #[test]
    fn directs_nested_declarations_to_nested_evaluation() {
        let source = "@vectorize result[i, j] = a[i + j]";
        let (tokens, lex_diags) = lex(source);
        assert!(lex_diags.is_empty(), "{lex_diags:?}");
        let (program, parse_diags) = parse(&tokens);
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let typed = typecheck_array(program.expect("nested array program")).unwrap();
        let error = lower_array(&typed).unwrap_err();
        assert!(error.contains("nested-array evaluation entry point"));
    }
}
