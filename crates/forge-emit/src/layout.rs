use forge_ir::{Block, Function, Inst, Ty, Value};
use forge_regalloc::{build_intervals, def_of, reads_of, Location, RegClass};
use forge_x64::{AluOp, Assembler, ConditionCode, MachineInst, PhysReg, SelectedFunction};
use std::collections::{HashMap, HashSet};

use crate::const_pool::{alloc_pool_labels, place_pool};
use crate::translate::translate_inst;

struct EmitContext<'a> {
    intervals: &'a HashMap<Value, (u32, u32)>,
    assignment: &'a HashMap<Value, Location>,
    framed: bool,
}

/// Lowers a selected function into a self-contained x86-64 code sequence.
/// Spilled values are reloaded into allocator-reserved scratch registers and
/// written back after their defining instruction. A frame is emitted only
/// when the allocation contains spills.
pub fn emit_body(
    func: &Function,
    selected: &SelectedFunction,
    assignment: &HashMap<Value, Location>,
) -> Vec<u8> {
    let mut asm = Assembler::new();
    let intervals = build_intervals(func, selected)
        .into_iter()
        .map(|iv| (iv.value, (iv.start, iv.end)))
        .collect::<HashMap<_, _>>();
    let framed = assignment.values().any(|l| matches!(l, Location::Spill(_)))
        || (cfg!(windows) && func.params.len() > 4);
    let spill_bytes = assignment
        .values()
        .filter_map(|l| match l {
            Location::Spill(slot) => Some(slot.saturating_add(1)),
            Location::Reg(_) => None,
        })
        .max()
        .unwrap_or(0)
        .saturating_mul(8);
    let callee_saved: Vec<PhysReg> = forge_x64::SYSV_CALLEE_SAVED
        .iter()
        .copied()
        .filter(|r| assignment.values().any(|l| *l == Location::Reg(*r)))
        .collect();
    if framed {
        forge_x64::emit_prologue(&mut asm, &callee_saved, spill_bytes);
    }

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

        for (offset, inst) in selected.insts[start..end].iter().enumerate() {
            let position = start + offset;
            let scratch = assign_spill_scratch(func, selected, assignment, inst);
            let loc = |v: Value| match assignment[&v] {
                Location::Reg(r) => r,
                Location::Spill(_) => scratch[&v],
            };

            let mut loaded = HashSet::new();
            for value in reads_of(inst) {
                if loaded.insert(value) {
                    if let Location::Spill(slot) = assignment[&value] {
                        let reg = scratch[&value];
                        if value_ty(func, selected, value) == Ty::F64 {
                            asm.movsd_reg_mem(reg, PhysReg::Rbp, spill_offset(slot));
                        } else {
                            asm.mov_reg_mem(reg, PhysReg::Rbp, spill_offset(slot));
                        }
                    }
                }
            }

            match inst {
                MachineInst::Param { dst, index } => {
                    emit_param(func, *index, loc(*dst), &mut asm, framed);
                }
                MachineInst::CallLibm {
                    dst,
                    func: libm,
                    args,
                } => {
                    emit_libm_call(
                        &mut asm,
                        *libm,
                        args,
                        &loc,
                        loc(*dst),
                        position,
                        &EmitContext {
                            intervals: &intervals,
                            assignment,
                            framed,
                        },
                    );
                }
                MachineInst::IntDiv { .. } | MachineInst::IntRem { .. } => {
                    let saved = live_gpr_registers(
                        position,
                        &intervals,
                        assignment,
                        &[PhysReg::Rax, PhysReg::Rdx],
                    );
                    with_saved_gprs(&mut asm, &saved, framed, |asm| {
                        translate_inst(asm, inst, &loc, &pool_labels);
                    });
                }
                MachineInst::Shl { .. } | MachineInst::Shr { .. } | MachineInst::Sar { .. } => {
                    let saved =
                        live_gpr_registers(position, &intervals, assignment, &[PhysReg::Rcx]);
                    with_saved_gprs(&mut asm, &saved, framed, |asm| {
                        translate_inst(asm, inst, &loc, &pool_labels);
                    });
                }
                MachineInst::Jump { target } => {
                    emit_phi_edge_copies(func, block, *target, assignment, &mut asm);
                    asm.jmp(block_labels[target]);
                }
                MachineInst::Branch { cond, then_, else_ } => {
                    emit_phi_edge_copies(func, block, *then_, assignment, &mut asm);
                    let cond_r = loc(*cond);
                    asm.test_reg_reg(cond_r, cond_r);
                    asm.jcc(ConditionCode::NotEqual, block_labels[then_]);
                    emit_phi_edge_copies(func, block, *else_, assignment, &mut asm);
                    asm.jmp(block_labels[else_]);
                }
                MachineInst::Return { value } => {
                    let value_r = loc(*value);
                    let value_ty = value_ty(func, selected, *value);
                    let ret_r = if value_ty == Ty::F64 {
                        PhysReg::Xmm0
                    } else {
                        PhysReg::Rax
                    };
                    if value_r != ret_r {
                        if value_ty == Ty::F64 {
                            asm.movsd_reg_reg(ret_r, value_r);
                        } else {
                            asm.mov_reg_reg(ret_r, value_r);
                        }
                    }
                    if framed {
                        forge_x64::emit_epilogue(&mut asm, &callee_saved, spill_bytes);
                    } else {
                        asm.ret();
                    }
                }
                other => translate_inst(&mut asm, other, &loc, &pool_labels),
            }

            if let Some(dst) = def_of(inst) {
                if let Location::Spill(slot) = assignment[&dst] {
                    let reg = scratch[&dst];
                    if value_ty(func, selected, dst) == Ty::F64 {
                        asm.movsd_mem_reg(PhysReg::Rbp, spill_offset(slot), reg);
                    } else {
                        asm.mov_mem_reg(PhysReg::Rbp, spill_offset(slot), reg);
                    }
                }
            }
        }
    }

    place_pool(&mut asm, &selected.pool, &pool_labels);
    asm.code().to_vec()
}

#[derive(Clone, Copy)]
struct PhiCopy {
    src: Location,
    dst: Location,
    ty: Ty,
}

/// Materializes the values selected by φ nodes on one CFG edge. φ nodes are
/// intentionally absent from `MachineInst`; their incoming values therefore
/// have to be copied immediately before the edge's jump. The allocator keeps
/// spill scratch registers out of ordinary assignments, so they are safe for
/// breaking register cycles and for spill-to-spill transfers.
fn emit_phi_edge_copies(
    func: &Function,
    source: Block,
    target: Block,
    assignment: &HashMap<Value, Location>,
    asm: &mut Assembler,
) {
    let mut copies = Vec::new();
    for &phi in &func.blocks[target.0 as usize].insts {
        let Inst::Phi { incoming } = &func.insts[phi.0 as usize] else {
            continue;
        };
        let Some((_, incoming_value)) = incoming.iter().find(|(pred, _)| *pred == source) else {
            continue;
        };
        let Some(&src) = assignment.get(incoming_value) else {
            panic!("missing allocation for φ incoming value {incoming_value:?}");
        };
        let Some(&dst) = assignment.get(&phi) else {
            panic!("missing allocation for φ destination {phi:?}");
        };
        if src != dst {
            copies.push(PhiCopy {
                src,
                dst,
                ty: func.types[phi.0 as usize],
            });
        }
    }
    emit_parallel_copies(&mut copies, asm);
}

fn emit_parallel_copies(copies: &mut Vec<PhiCopy>, asm: &mut Assembler) {
    while !copies.is_empty() {
        let safe = (0..copies.len()).find(|&index| {
            !copies
                .iter()
                .enumerate()
                .any(|(other, copy)| other != index && copy.src == copies[index].dst)
        });
        if let Some(index) = safe {
            let copy = copies.remove(index);
            emit_copy(asm, copy.src, copy.dst, copy.ty);
            continue;
        }

        // No destination is free to overwrite: the remaining moves form a
        // register cycle. Preserve one source in a reserved scratch register
        // and redirect every move that read it to that temporary.
        let first = copies[0];
        let scratch = phi_scratch(first.ty);
        assert!(
            copies.iter().all(
                |copy| copy.src != Location::Reg(scratch) && copy.dst != Location::Reg(scratch)
            ),
            "φ parallel-copy scratch register is unexpectedly allocated"
        );
        emit_copy(asm, first.src, Location::Reg(scratch), first.ty);
        for copy in copies.iter_mut() {
            if copy.src == first.src {
                copy.src = Location::Reg(scratch);
            }
        }
    }
}

fn emit_copy(asm: &mut Assembler, src: Location, dst: Location, ty: Ty) {
    match (src, dst) {
        (Location::Reg(src), Location::Reg(dst)) => {
            if src != dst {
                if ty == Ty::F64 {
                    asm.movsd_reg_reg(dst, src);
                } else {
                    asm.mov_reg_reg(dst, src);
                }
            }
        }
        (Location::Spill(slot), Location::Reg(dst)) => {
            if ty == Ty::F64 {
                asm.movsd_reg_mem(dst, PhysReg::Rbp, spill_offset(slot));
            } else {
                asm.mov_reg_mem(dst, PhysReg::Rbp, spill_offset(slot));
            }
        }
        (Location::Reg(src), Location::Spill(slot)) => {
            if ty == Ty::F64 {
                asm.movsd_mem_reg(PhysReg::Rbp, spill_offset(slot), src);
            } else {
                asm.mov_mem_reg(PhysReg::Rbp, spill_offset(slot), src);
            }
        }
        (Location::Spill(src), Location::Spill(dst)) => {
            let scratch = phi_scratch(ty);
            if ty == Ty::F64 {
                asm.movsd_reg_mem(scratch, PhysReg::Rbp, spill_offset(src));
                asm.movsd_mem_reg(PhysReg::Rbp, spill_offset(dst), scratch);
            } else {
                asm.mov_reg_mem(scratch, PhysReg::Rbp, spill_offset(src));
                asm.mov_mem_reg(PhysReg::Rbp, spill_offset(dst), scratch);
            }
        }
    }
}

fn phi_scratch(ty: Ty) -> PhysReg {
    if ty == Ty::F64 {
        forge_regalloc::SCRATCH_XMM[0]
    } else {
        forge_regalloc::SCRATCH_GPR[0]
    }
}

fn spill_offset(slot: u32) -> i32 {
    let bytes = slot
        .checked_add(1)
        .and_then(|n| n.checked_mul(8))
        .expect("spill frame is too large for an x86 displacement");
    -(i32::try_from(bytes).expect("spill frame is too large for an x86 displacement"))
}

fn assign_spill_scratch(
    func: &Function,
    selected: &SelectedFunction,
    assignment: &HashMap<Value, Location>,
    inst: &MachineInst,
) -> HashMap<Value, PhysReg> {
    let mut out = HashMap::new();
    let mut next = [0usize, 0usize];
    let spill_dst_alias = match inst {
        // IntCmov reads all three inputs before it writes its destination.
        // Reusing the then-value's scratch register keeps this four-value
        // machine instruction within the three-register scratch budget.
        MachineInst::IntCmov { dst, then_val, .. }
            if matches!(assignment[dst], Location::Spill(_))
                && matches!(assignment[then_val], Location::Spill(_)) =>
        {
            Some((*dst, *then_val))
        }
        _ => None,
    };
    let mut values = reads_of(inst);
    if let Some(dst) = def_of(inst) {
        values.push(dst);
    }
    for value in values {
        let Location::Spill(_) = assignment[&value] else {
            continue;
        };
        if out.contains_key(&value) {
            continue;
        }
        if let Some((dst, source)) = spill_dst_alias {
            if value == dst {
                let reg = *out
                    .get(&source)
                    .expect("IntCmov then-value scratch must be assigned before its destination");
                out.insert(value, reg);
                continue;
            }
        }
        let class = if value_ty(func, selected, value) == Ty::F64 {
            1
        } else {
            0
        };
        let scratch = if class == 0 {
            forge_regalloc::SCRATCH_GPR
        } else {
            forge_regalloc::SCRATCH_XMM
        };
        let slot = next[class];
        assert!(
            slot < scratch.len(),
            "instruction needs more spilled {} operands than available scratch registers",
            if class == 0 { "GPR" } else { "XMM" }
        );
        out.insert(value, scratch[slot]);
        next[class] += 1;
    }
    out
}

fn emit_param(func: &Function, index: u32, dst: PhysReg, asm: &mut Assembler, framed: bool) {
    let ty = func.params[index as usize].1;
    if cfg!(windows) {
        if index >= 4 {
            assert!(framed, "Win64 stack parameters require a frame pointer");
            let slot_offset = (index - 4)
                .checked_mul(8)
                .expect("Win64 parameter area is too large for an x86 displacement");
            let offset = 48i32
                .checked_add(
                    i32::try_from(slot_offset)
                        .expect("Win64 parameter area is too large for an x86 displacement"),
                )
                .expect("Win64 parameter area is too large for an x86 displacement");
            if ty == Ty::F64 {
                asm.movsd_reg_mem(dst, PhysReg::Rbp, offset);
            } else {
                asm.mov_reg_mem(dst, PhysReg::Rbp, offset);
            }
            return;
        }
        let src = match RegClass::of(ty) {
            RegClass::Gpr => [PhysReg::Rcx, PhysReg::Rdx, PhysReg::R8, PhysReg::R9][index as usize],
            RegClass::Xmm => {
                [PhysReg::Xmm0, PhysReg::Xmm1, PhysReg::Xmm2, PhysReg::Xmm3][index as usize]
            }
        };
        if dst != src {
            if ty == Ty::F64 {
                asm.movsd_reg_reg(dst, src);
            } else {
                asm.mov_reg_reg(dst, src);
            }
        }
        return;
    }
    let ordinal = func.params[..index as usize]
        .iter()
        .filter(|(_, prior_ty)| RegClass::of(*prior_ty) == RegClass::of(ty))
        .count();
    let src = match RegClass::of(ty) {
        RegClass::Gpr => forge_regalloc::SYSV_INT_ARGS[ordinal],
        RegClass::Xmm => forge_regalloc::SYSV_FLOAT_ARGS[ordinal],
    };
    if dst != src {
        if ty == Ty::F64 {
            asm.movsd_reg_reg(dst, src);
        } else {
            asm.mov_reg_reg(dst, src);
        }
    }
}

fn live_gpr_registers(
    position: usize,
    intervals: &HashMap<Value, (u32, u32)>,
    assignment: &HashMap<Value, Location>,
    candidates: &[PhysReg],
) -> Vec<(PhysReg, Value)> {
    let mut out = Vec::new();
    for (&value, &(start, end)) in intervals {
        if start < position as u32 && end > position as u32 {
            if let Location::Reg(reg) = assignment[&value] {
                if candidates.contains(&reg) && !out.iter().any(|(r, _)| *r == reg) {
                    out.push((reg, value));
                }
            }
        }
    }
    out.sort_by_key(|(reg, _)| reg.encoding());
    out
}

fn with_saved_gprs(
    asm: &mut Assembler,
    saved: &[(PhysReg, Value)],
    framed: bool,
    body: impl FnOnce(&mut Assembler),
) {
    if saved.is_empty() {
        body(asm);
        return;
    }
    let bytes = aligned_temporary_bytes(saved.len(), framed);
    asm.alu_reg_imm(AluOp::Sub, PhysReg::Rsp, bytes as i32);
    for (i, (reg, _)) in saved.iter().enumerate() {
        asm.mov_mem_reg(PhysReg::Rsp, (i * 8) as i32, *reg);
    }
    body(asm);
    for (i, (reg, _)) in saved.iter().enumerate().rev() {
        asm.mov_reg_mem(*reg, PhysReg::Rsp, (i * 8) as i32);
    }
    asm.alu_reg_imm(AluOp::Add, PhysReg::Rsp, bytes as i32);
}

fn aligned_temporary_bytes(slots: usize, framed: bool) -> usize {
    let raw = slots * 8;
    let desired = if framed { 0 } else { 8 };
    raw + (desired + 16 - raw % 16) % 16
}

fn emit_libm_call(
    asm: &mut Assembler,
    func: forge_ir::LibFunc,
    args: &[Value],
    loc: &dyn Fn(Value) -> PhysReg,
    dst: PhysReg,
    position: usize,
    context: &EmitContext<'_>,
) {
    let caller_saved = [
        PhysReg::Rax,
        PhysReg::Rcx,
        PhysReg::Rdx,
        PhysReg::Rsi,
        PhysReg::Rdi,
        PhysReg::R8,
        PhysReg::R9,
        PhysReg::R10,
        PhysReg::R11,
    ];
    let mut saved = Vec::new();
    for (&value, &(start, end)) in context.intervals {
        if start < position as u32 && end > position as u32 {
            if let Location::Reg(reg) = context.assignment[&value] {
                if (caller_saved.contains(&reg) || is_xmm_reg(reg))
                    && !saved.iter().any(|(r, _)| *r == reg)
                {
                    saved.push((reg, value));
                }
            }
        }
    }
    saved.sort_by_key(|(reg, _)| (is_xmm_reg(*reg), reg.encoding()));
    let bytes = aligned_call_bytes(saved.len(), context.framed);
    let save_base = if cfg!(windows) { 32 } else { 0 };
    asm.alu_reg_imm(AluOp::Sub, PhysReg::Rsp, bytes as i32);
    for (i, (reg, _)) in saved.iter().enumerate() {
        if is_xmm_reg(*reg) {
            asm.movsd_mem_reg(PhysReg::Rsp, (save_base + i * 8) as i32, *reg);
        } else {
            asm.mov_mem_reg(PhysReg::Rsp, (save_base + i * 8) as i32, *reg);
        }
    }

    let sources: Vec<PhysReg> = args.iter().map(|v| loc(*v)).collect();
    if sources.len() == 2 && sources[0] == PhysReg::Xmm1 && sources[1] == PhysReg::Xmm0 {
        asm.movsd_reg_reg(PhysReg::Xmm15, PhysReg::Xmm0);
        asm.movsd_reg_reg(PhysReg::Xmm0, PhysReg::Xmm1);
        asm.movsd_reg_reg(PhysReg::Xmm1, PhysReg::Xmm15);
    } else {
        for (i, source) in sources.iter().enumerate() {
            let target = [PhysReg::Xmm0, PhysReg::Xmm1][i];
            if *source != target {
                asm.movsd_reg_reg(target, *source);
            }
        }
    }
    asm.mov_reg_imm(PhysReg::R11, forge_x64::libm_address(func));
    asm.call_reg(PhysReg::R11);
    asm.movsd_reg_reg(PhysReg::Xmm15, PhysReg::Xmm0);
    for (i, (reg, _)) in saved.iter().enumerate().rev() {
        if is_xmm_reg(*reg) {
            asm.movsd_reg_mem(*reg, PhysReg::Rsp, (save_base + i * 8) as i32);
        } else {
            asm.mov_reg_mem(*reg, PhysReg::Rsp, (save_base + i * 8) as i32);
        }
    }
    if dst != PhysReg::Xmm15 {
        asm.movsd_reg_reg(dst, PhysReg::Xmm15);
    }
    asm.alu_reg_imm(AluOp::Add, PhysReg::Rsp, bytes as i32);
}

fn aligned_call_bytes(saved_slots: usize, framed: bool) -> usize {
    if cfg!(windows) {
        let raw = 32 + saved_slots * 8;
        let desired = if framed { 0 } else { 8 };
        raw + (desired + 16 - raw % 16) % 16
    } else {
        aligned_temporary_bytes(saved_slots, framed)
    }
}

fn is_xmm_reg(reg: PhysReg) -> bool {
    matches!(
        reg,
        PhysReg::Xmm0
            | PhysReg::Xmm1
            | PhysReg::Xmm2
            | PhysReg::Xmm3
            | PhysReg::Xmm4
            | PhysReg::Xmm5
            | PhysReg::Xmm6
            | PhysReg::Xmm7
            | PhysReg::Xmm8
            | PhysReg::Xmm9
            | PhysReg::Xmm10
            | PhysReg::Xmm11
            | PhysReg::Xmm12
            | PhysReg::Xmm13
            | PhysReg::Xmm14
            | PhysReg::Xmm15
            | PhysReg::Xmm16
            | PhysReg::Xmm17
            | PhysReg::Xmm18
            | PhysReg::Xmm19
            | PhysReg::Xmm20
            | PhysReg::Xmm21
            | PhysReg::Xmm22
            | PhysReg::Xmm23
            | PhysReg::Xmm24
            | PhysReg::Xmm25
            | PhysReg::Xmm26
            | PhysReg::Xmm27
            | PhysReg::Xmm28
            | PhysReg::Xmm29
            | PhysReg::Xmm30
            | PhysReg::Xmm31
    )
}

fn value_ty(func: &Function, selected: &SelectedFunction, v: Value) -> Ty {
    selected
        .synthetic_types
        .get(&v)
        .copied()
        .unwrap_or_else(|| func.types[v.0 as usize])
}
