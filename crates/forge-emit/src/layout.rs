use forge_ir::{Block, Function, Ty, Value};
use forge_regalloc::Location;
use forge_x64::{Assembler, ConditionCode, MachineInst, PhysReg, SelectedFunction};
use std::collections::HashMap;

use crate::const_pool::{alloc_pool_labels, place_pool};
use crate::translate::translate_inst;

/// Lowers `selected` (register-only operands — every `Value` in `assignment`
/// must be `Location::Reg`) into a runnable, self-contained x86-64 byte
/// sequence: real control flow, constant pool placed after the code, and a
/// bare `ret` after each `Return`'s value-placement move (Phase 9a owns its
/// own `ret` emission; splicing a real prologue/epilogue around this is
/// Phase 9f's job).
pub fn emit_body(
    func: &Function,
    selected: &SelectedFunction,
    assignment: &HashMap<Value, Location>,
) -> Vec<u8> {
    let mut asm = Assembler::new();
    let loc = |v: Value| match assignment[&v] {
        Location::Reg(r) => r,
        Location::Spill(_) => {
            panic!("forge-emit (Phase 9a): spilled operand not yet supported — Phase 9c")
        }
    };

    let pool_labels = alloc_pool_labels(&mut asm, &selected.pool);

    let block_labels: HashMap<Block, forge_x64::Label> = selected
        .block_starts
        .iter()
        .map(|&(block, _)| (block, asm.new_label()))
        .collect();

    for (i, &(block, start)) in selected.block_starts.iter().enumerate() {
        let end = selected
            .block_starts
            .get(i + 1)
            .map(|&(_, s)| s)
            .unwrap_or(selected.insts.len());

        asm.bind(block_labels[&block]);

        for inst in &selected.insts[start..end] {
            match inst {
                MachineInst::Jump { target } => asm.jmp(block_labels[target]),
                MachineInst::Branch { cond, then_, else_ } => {
                    let cond_r = loc(*cond);
                    asm.test_reg_reg(cond_r, cond_r);
                    asm.jcc(ConditionCode::NotEqual, block_labels[then_]);
                    asm.jmp(block_labels[else_]);
                }
                MachineInst::Return { value } => {
                    let value_r = loc(*value);
                    let ret_r = if value_ty(func, selected, *value) == Ty::F64 {
                        PhysReg::Xmm0
                    } else {
                        PhysReg::Rax
                    };
                    if value_r != ret_r {
                        if ret_r == PhysReg::Xmm0 {
                            asm.movsd_reg_reg(ret_r, value_r);
                        } else {
                            asm.mov_reg_reg(ret_r, value_r);
                        }
                    }
                    asm.ret();
                }
                other => translate_inst(&mut asm, other, &loc, &pool_labels),
            }
        }
    }

    place_pool(&mut asm, &selected.pool, &pool_labels);

    asm.code().to_vec()
}

fn value_ty(func: &Function, selected: &SelectedFunction, v: Value) -> Ty {
    selected
        .synthetic_types
        .get(&v)
        .copied()
        .unwrap_or_else(|| func.types[v.0 as usize])
}
