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
        .param_kinds
        .iter()
        .filter(|kind| **kind == ArrayParamKind::Column)
        .count();
    for inst in &function.blocks[1].insts {
        match inst {
            ArrayInst::Load {
                result,
                column,
                index,
            } => {
                let Some(element_param) = function
                    .param_kinds
                    .iter()
                    .enumerate()
                    .filter(|(_, kind)| **kind == ArrayParamKind::Column)
                    .nth(*column as usize)
                    .map(|(param, _)| param as u32)
                else {
                    return Err("array load references an unknown column".to_string());
                };
                if *index != induction
                    || !matches!(
                        function.element.insts.get(result.0 as usize),
                        Some(crate::Inst::Param { index, ty: Ty::F64 })
                            if *index == element_param
                    )
                {
                    return Err("array load does not match an element parameter".to_string());
                }
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
    if loads != column_count || stores != 1 {
        return Err("array body must load every column and store exactly once".to_string());
    }
    Ok(())
}

/// Lowers a type-checked vectorized body into the memory/loop envelope and a
/// scalar element function. The current language contract intentionally
/// accepts the canonical `column[index]` addressing form; arbitrary offsets
/// can be added later without weakening this representation. Unindexed f64
/// parameters remain scalar broadcasts in the element function and are not
/// represented by memory loads.
pub fn lower_array(typed: &TypedArray) -> Result<ArrayFunction, String> {
    let mut ast = typed.ast.clone();
    let root = ast.root;
    rewrite_indexed_loads(&mut ast, root, &typed.index)?;
    let element_typed = TypedAst {
        ast,
        types: typed.types.clone(),
        params: typed
            .params
            .iter()
            .map(|(name, ty)| {
                (
                    name.clone(),
                    match ty {
                        AstTy::ArrayF64 => AstTy::F64,
                        other => *other,
                    },
                )
            })
            .collect(),
    };
    let element = crate::lower::lower(&element_typed);
    let induction = ArrayValue(0);
    let loads = typed
        .params
        .iter()
        .enumerate()
        .filter(|(_, (_, ty))| *ty == AstTy::ArrayF64)
        .enumerate()
        .map(|(column, (param, _))| {
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
                .ok_or_else(|| format!("missing element parameter for column {column}"))?;
            Ok(ArrayInst::Load {
                result,
                column: column as u32,
                index: induction,
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
        params: typed
            .params
            .iter()
            .map(|(name, _)| (name.clone(), Ty::F64))
            .collect(),
        param_kinds: typed
            .params
            .iter()
            .map(|(_, ty)| match ty {
                AstTy::ArrayF64 => ArrayParamKind::Column,
                AstTy::F64 => ArrayParamKind::ScalarBroadcast,
                other => panic!("unexpected array parameter type {other:?}"),
            })
            .collect(),
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

fn rewrite_indexed_loads(ast: &mut Ast, idx: ExprIdx, induction_name: &str) -> Result<(), String> {
    match ast.get(idx).clone() {
        Expr::Index { base, index } => {
            if !matches!(ast.get(base), Expr::Ident(_)) {
                return Err("vectorized loads must use a column identifier".to_string());
            }
            if !matches!(ast.get(index), Expr::Ident(name) if name == induction_name) {
                return Err("vectorized loads must use the declared induction variable".to_string());
            }
            let Expr::Ident(name) = ast.get(base).clone() else {
                unreachable!()
            };
            ast.exprs[idx.index()] = Expr::Ident(name);
        }
        Expr::Unary { operand, .. } => rewrite_indexed_loads(ast, operand, induction_name)?,
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_indexed_loads(ast, lhs, induction_name)?;
            rewrite_indexed_loads(ast, rhs, induction_name)?;
        }
        Expr::Call { args, .. } => {
            for arg in args {
                rewrite_indexed_loads(ast, arg, induction_name)?;
            }
        }
        Expr::If { cond, then_, else_ } => {
            rewrite_indexed_loads(ast, cond, induction_name)?;
            rewrite_indexed_loads(ast, then_, induction_name)?;
            rewrite_indexed_loads(ast, else_, induction_name)?;
        }
        Expr::Let { value, body, .. } => {
            rewrite_indexed_loads(ast, value, induction_name)?;
            rewrite_indexed_loads(ast, body, induction_name)?;
        }
        Expr::Float(_) | Expr::Int(_) | Expr::Bool(_) | Expr::Ident(_) => {}
    }
    Ok(())
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
}
