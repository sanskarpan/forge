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
/// written back after their defining instruction. A frame is emitted when the
/// allocation contains spills, stack-backed parameters, or callee-saved values.
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
    let framed =
        assignment.values().any(|l| matches!(l, Location::Spill(_)))
            || assignment.values().any(|location| match location {
                Location::Reg(reg) => forge_x64::CALLEE_SAVED.contains(reg),
                Location::Spill(_) => false,
            })
            || (cfg!(windows) && func.params.len() > 4)
            || (!cfg!(windows)
                && func.params.iter().enumerate().any(|(index, (_, ty))| {
                    sysv_param_ordinals(&func.params, index, *ty).1.is_some()
                }));
    let spill_bytes = assignment
        .values()
        .filter_map(|l| match l {
            Location::Spill(slot) => Some(slot.saturating_add(1)),
            Location::Reg(_) => None,
        })
        .max()
        .unwrap_or(0)
        .saturating_mul(8);
    let callee_saved: Vec<PhysReg> = forge_x64::CALLEE_SAVED
        .iter()
        .copied()
        .filter(|r| assignment.values().any(|l| *l == Location::Reg(*r)))
        .collect();
    let spill_bias = if cfg!(windows) {
        i32::try_from(callee_saved.len() * 8)
            .expect("callee-saved frame is too large for an x86 displacement")
    } else {
        0
    };
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
        if block == func.entry {
            emit_params(func, selected, assignment, &mut asm, framed, spill_bias);
        }

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
                            asm.movsd_reg_mem(reg, PhysReg::Rbp, spill_offset(slot, spill_bias));
                        } else {
                            asm.mov_reg_mem(reg, PhysReg::Rbp, spill_offset(slot, spill_bias));
                        }
                    }
                }
            }

            match inst {
                MachineInst::Param { .. } => {}
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
                MachineInst::ExternalCall { dst, address, args } => {
                    emit_external_call(
                        &mut asm,
                        *address,
                        args,
                        &loc,
                        loc(*dst),
                        func,
                        selected,
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
                    emit_phi_edge_copies(func, block, *target, assignment, &mut asm, spill_bias);
                    asm.jmp(block_labels[target]);
                }
                MachineInst::Branch { cond, then_, else_ } => {
                    emit_phi_edge_copies(func, block, *then_, assignment, &mut asm, spill_bias);
                    let cond_r = loc(*cond);
                    asm.test_reg_reg(cond_r, cond_r);
                    asm.jcc(ConditionCode::NotEqual, block_labels[then_]);
                    emit_phi_edge_copies(func, block, *else_, assignment, &mut asm, spill_bias);
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

            if !matches!(inst, MachineInst::Param { .. }) {
                if let Some(dst) = def_of(inst) {
                    if let Location::Spill(slot) = assignment[&dst] {
                        let reg = scratch[&dst];
                        if value_ty(func, selected, dst) == Ty::F64 {
                            asm.movsd_mem_reg(PhysReg::Rbp, spill_offset(slot, spill_bias), reg);
                        } else {
                            asm.mov_mem_reg(PhysReg::Rbp, spill_offset(slot, spill_bias), reg);
                        }
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
    spill_bias: i32,
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
    emit_parallel_copies(&mut copies, asm, spill_bias);
}

fn emit_parallel_copies(copies: &mut Vec<PhiCopy>, asm: &mut Assembler, spill_bias: i32) {
    while !copies.is_empty() {
        let safe = (0..copies.len()).find(|&index| {
            !copies
                .iter()
                .enumerate()
                .any(|(other, copy)| other != index && copy.src == copies[index].dst)
        });
        if let Some(index) = safe {
            let copy = copies.remove(index);
            emit_copy(asm, copy.src, copy.dst, copy.ty, spill_bias);
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
        emit_copy(asm, first.src, Location::Reg(scratch), first.ty, spill_bias);
        for copy in copies.iter_mut() {
            if copy.src == first.src {
                copy.src = Location::Reg(scratch);
            }
        }
    }
}

fn emit_copy(asm: &mut Assembler, src: Location, dst: Location, ty: Ty, spill_bias: i32) {
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
                asm.movsd_reg_mem(dst, PhysReg::Rbp, spill_offset(slot, spill_bias));
            } else {
                asm.mov_reg_mem(dst, PhysReg::Rbp, spill_offset(slot, spill_bias));
            }
        }
        (Location::Reg(src), Location::Spill(slot)) => {
            if ty == Ty::F64 {
                asm.movsd_mem_reg(PhysReg::Rbp, spill_offset(slot, spill_bias), src);
            } else {
                asm.mov_mem_reg(PhysReg::Rbp, spill_offset(slot, spill_bias), src);
            }
        }
        (Location::Spill(src), Location::Spill(dst)) => {
            let scratch = phi_scratch(ty);
            if ty == Ty::F64 {
                asm.movsd_reg_mem(scratch, PhysReg::Rbp, spill_offset(src, spill_bias));
                asm.movsd_mem_reg(PhysReg::Rbp, spill_offset(dst, spill_bias), scratch);
            } else {
                asm.mov_reg_mem(scratch, PhysReg::Rbp, spill_offset(src, spill_bias));
                asm.mov_mem_reg(PhysReg::Rbp, spill_offset(dst, spill_bias), scratch);
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

/// Materializes all entry parameters as one parallel-copy operation. A
/// register-backed parameter cannot be copied independently: its destination
/// may be another parameter's incoming ABI register. Scheduling the complete
/// set together preserves those incoming values and handles register cycles.
fn emit_params(
    func: &Function,
    selected: &SelectedFunction,
    assignment: &HashMap<Value, Location>,
    asm: &mut Assembler,
    framed: bool,
    spill_bias: i32,
) {
    let mut register_copies = Vec::new();
    let mut stack_params = Vec::new();
    for inst in &selected.insts {
        let MachineInst::Param { dst, index } = inst else {
            continue;
        };
        let index = *index as usize;
        let ty = func.params[index].1;
        let destination = assignment[dst];
        if let Some(source) = param_register(&func.params, index, ty) {
            if destination != Location::Reg(source) {
                register_copies.push(PhiCopy {
                    src: Location::Reg(source),
                    dst: destination,
                    ty,
                });
            }
        } else {
            stack_params.push((index, destination, ty));
        }
    }

    emit_parallel_copies(&mut register_copies, asm, spill_bias);
    for (index, destination, ty) in stack_params {
        let destination_register = match destination {
            Location::Reg(reg) => reg,
            Location::Spill(slot) => {
                let scratch = phi_scratch(ty);
                emit_stack_param_load(&func.params, index, ty, scratch, asm, framed);
                emit_copy(
                    asm,
                    Location::Reg(scratch),
                    Location::Spill(slot),
                    ty,
                    spill_bias,
                );
                continue;
            }
        };
        emit_stack_param_load(&func.params, index, ty, destination_register, asm, framed);
    }
}

fn param_register(params: &[(String, Ty)], index: usize, ty: Ty) -> Option<PhysReg> {
    if cfg!(windows) {
        if index >= 4 {
            return None;
        }
        return Some(match RegClass::of(ty) {
            RegClass::Gpr => [PhysReg::Rcx, PhysReg::Rdx, PhysReg::R8, PhysReg::R9][index],
            RegClass::Xmm => [PhysReg::Xmm0, PhysReg::Xmm1, PhysReg::Xmm2, PhysReg::Xmm3][index],
        });
    }

    let (ordinal, stack_ordinal) = sysv_param_ordinals(params, index, ty);
    if stack_ordinal.is_some() {
        None
    } else {
        Some(match RegClass::of(ty) {
            RegClass::Gpr => forge_regalloc::SYSV_INT_ARGS[ordinal],
            RegClass::Xmm => forge_regalloc::SYSV_FLOAT_ARGS[ordinal],
        })
    }
}

fn emit_stack_param_load(
    params: &[(String, Ty)],
    index: usize,
    ty: Ty,
    destination: PhysReg,
    asm: &mut Assembler,
    framed: bool,
) {
    assert!(framed, "stack parameters require a frame pointer");
    let offset = if cfg!(windows) {
        let slot_offset = (index - 4)
            .checked_mul(8)
            .expect("Win64 parameter area is too large for an x86 displacement");
        48i32
            .checked_add(
                i32::try_from(slot_offset)
                    .expect("Win64 parameter area is too large for an x86 displacement"),
            )
            .expect("Win64 parameter area is too large for an x86 displacement")
    } else {
        let (_, stack_ordinal) = sysv_param_ordinals(params, index, ty);
        let stack_ordinal = stack_ordinal.expect("stack parameter must have a stack ordinal");
        let bytes = 16usize
            .checked_add(stack_ordinal * 8)
            .expect("SysV parameter area is too large for an x86 displacement");
        i32::try_from(bytes).expect("SysV parameter area is too large for an x86 displacement")
    };
    if ty == Ty::F64 {
        asm.movsd_reg_mem(destination, PhysReg::Rbp, offset);
    } else {
        asm.mov_reg_mem(destination, PhysReg::Rbp, offset);
    }
}

fn spill_offset(slot: u32, spill_bias: i32) -> i32 {
    let bytes = slot
        .checked_add(1)
        .and_then(|n| n.checked_mul(8))
        .expect("spill frame is too large for an x86 displacement");
    -i32::try_from(bytes)
        .expect("spill frame is too large for an x86 displacement")
        .checked_add(spill_bias)
        .expect("spill frame is too large for an x86 displacement")
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
        // FloatFma copies the addend into dst before issuing the three-source
        // instruction, so a spilled destination can safely reuse the
        // addend's reload scratch. This keeps four spilled values within the
        // three-register XMM scratch budget on both ABIs.
        MachineInst::FloatFma { dst, addend, .. }
            if matches!(assignment[dst], Location::Spill(_))
                && matches!(assignment[addend], Location::Spill(_)) =>
        {
            Some((*dst, *addend))
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

/// Returns the SysV register-bank ordinal and, when that bank is exhausted,
/// the ordinal of the parameter's eight-byte incoming stack slot. Stack slots
/// are assigned in source parameter order, while register ordinals are counted
/// independently for GPR and XMM classes.
fn sysv_param_ordinals(params: &[(String, Ty)], index: usize, ty: Ty) -> (usize, Option<usize>) {
    let mut gpr_seen = 0usize;
    let mut xmm_seen = 0usize;
    let mut stack_seen = 0usize;
    for &(_, prior_ty) in params.iter().take(index) {
        let (seen, capacity) = match RegClass::of(prior_ty) {
            RegClass::Gpr => (&mut gpr_seen, forge_regalloc::SYSV_INT_ARGS.len()),
            RegClass::Xmm => (&mut xmm_seen, forge_regalloc::SYSV_FLOAT_ARGS.len()),
        };
        if *seen >= capacity {
            stack_seen += 1;
        }
        *seen += 1;
    }
    let (ordinal, capacity) = match RegClass::of(ty) {
        RegClass::Gpr => (gpr_seen, forge_regalloc::SYSV_INT_ARGS.len()),
        RegClass::Xmm => (xmm_seen, forge_regalloc::SYSV_FLOAT_ARGS.len()),
    };
    let stack_ordinal = (ordinal >= capacity).then_some(stack_seen);
    (ordinal, stack_ordinal)
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
    let scratch = forge_regalloc::SCRATCH_XMM[2];
    if sources.len() == 2 && sources[0] == PhysReg::Xmm1 && sources[1] == PhysReg::Xmm0 {
        asm.movsd_reg_reg(scratch, PhysReg::Xmm0);
        asm.movsd_reg_reg(PhysReg::Xmm0, PhysReg::Xmm1);
        asm.movsd_reg_reg(PhysReg::Xmm1, scratch);
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
    asm.movsd_reg_reg(scratch, PhysReg::Xmm0);
    for (i, (reg, _)) in saved.iter().enumerate().rev() {
        if is_xmm_reg(*reg) {
            asm.movsd_reg_mem(*reg, PhysReg::Rsp, (save_base + i * 8) as i32);
        } else {
            asm.mov_reg_mem(*reg, PhysReg::Rsp, (save_base + i * 8) as i32);
        }
    }
    if dst != scratch {
        asm.movsd_reg_reg(dst, scratch);
    }
    asm.alu_reg_imm(AluOp::Add, PhysReg::Rsp, bytes as i32);
}

/// Emits a registered native call using the active platform's scalar C ABI.
/// Arguments are first staged in the outgoing area. That makes mixed-bank
/// register moves cycle-safe even when the allocator happened to place an
/// input in another input register.
#[allow(clippy::too_many_arguments)]
fn emit_external_call(
    asm: &mut Assembler,
    address: usize,
    args: &[Value],
    loc: &dyn Fn(Value) -> PhysReg,
    dst: PhysReg,
    func: &Function,
    selected: &SelectedFunction,
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

    let placements = external_arg_placements(args, func, selected);
    let stack_count = placements
        .iter()
        .filter(|(_, stack)| stack.is_some())
        .count();
    let abi_area = if cfg!(windows) {
        32 + stack_count * 8
    } else {
        stack_count * 8
    };
    let stage_base = abi_area;
    let save_base = stage_base + args.len() * 8;
    let raw = save_base + saved.len() * 8;
    let desired = if context.framed { 0 } else { 8 };
    let bytes = raw + (desired + 16 - raw % 16) % 16;
    asm.alu_reg_imm(
        AluOp::Sub,
        PhysReg::Rsp,
        i32::try_from(bytes).expect("external call frame is too large"),
    );
    for (i, (reg, _)) in saved.iter().enumerate() {
        let offset = i32::try_from(save_base + i * 8).expect("external call frame is too large");
        if is_xmm_reg(*reg) {
            asm.movsd_mem_reg(PhysReg::Rsp, offset, *reg);
        } else {
            asm.mov_mem_reg(PhysReg::Rsp, offset, *reg);
        }
    }

    for (i, value) in args.iter().enumerate() {
        let source = loc(*value);
        let offset = i32::try_from(stage_base + i * 8).expect("external call frame is too large");
        if value_ty(func, selected, *value) == Ty::F64 {
            asm.movsd_mem_reg(PhysReg::Rsp, offset, source);
        } else {
            asm.mov_mem_reg(PhysReg::Rsp, offset, source);
        }
    }
    for (i, (register, stack)) in placements.iter().enumerate() {
        let ty = value_ty(func, selected, args[i]);
        let source_offset =
            i32::try_from(stage_base + i * 8).expect("external call frame is too large");
        if let Some(register) = register {
            if ty == Ty::F64 {
                asm.movsd_reg_mem(*register, PhysReg::Rsp, source_offset);
            } else {
                asm.mov_reg_mem(*register, PhysReg::Rsp, source_offset);
            }
        } else {
            let stack_index = stack.expect("external stack placement has no index");
            let stack_offset = if cfg!(windows) {
                32 + stack_index * 8
            } else {
                stack_index * 8
            };
            let stack_offset =
                i32::try_from(stack_offset).expect("external call frame is too large");
            if ty == Ty::F64 {
                let scratch = forge_regalloc::SCRATCH_XMM[2];
                asm.movsd_reg_mem(scratch, PhysReg::Rsp, source_offset);
                asm.movsd_mem_reg(PhysReg::Rsp, stack_offset, scratch);
            } else {
                let scratch = forge_regalloc::SCRATCH_GPR[1];
                asm.mov_reg_mem(scratch, PhysReg::Rsp, source_offset);
                asm.mov_mem_reg(PhysReg::Rsp, stack_offset, scratch);
            }
        }
    }

    asm.mov_reg_imm(
        PhysReg::R11,
        i64::try_from(address).expect("external target address does not fit in i64"),
    );
    asm.call_reg(PhysReg::R11);
    let result_ty = value_ty(
        func,
        selected,
        def_of(&selected.insts[position]).expect("external call has a result"),
    );
    if result_ty == Ty::F64 {
        let scratch = forge_regalloc::SCRATCH_XMM[2];
        asm.movsd_reg_reg(scratch, PhysReg::Xmm0);
        for (i, (reg, _)) in saved.iter().enumerate().rev() {
            let offset =
                i32::try_from(save_base + i * 8).expect("external call frame is too large");
            if is_xmm_reg(*reg) {
                asm.movsd_reg_mem(*reg, PhysReg::Rsp, offset);
            } else {
                asm.mov_reg_mem(*reg, PhysReg::Rsp, offset);
            }
        }
        if dst != scratch {
            asm.movsd_reg_reg(dst, scratch);
        }
    } else {
        let scratch = forge_regalloc::SCRATCH_GPR[1];
        asm.mov_reg_reg(scratch, PhysReg::Rax);
        for (i, (reg, _)) in saved.iter().enumerate().rev() {
            let offset =
                i32::try_from(save_base + i * 8).expect("external call frame is too large");
            if is_xmm_reg(*reg) {
                asm.movsd_reg_mem(*reg, PhysReg::Rsp, offset);
            } else {
                asm.mov_reg_mem(*reg, PhysReg::Rsp, offset);
            }
        }
        if dst != scratch {
            asm.mov_reg_reg(dst, scratch);
        }
    }
    asm.alu_reg_imm(
        AluOp::Add,
        PhysReg::Rsp,
        i32::try_from(bytes).expect("external call frame is too large"),
    );
}

fn external_arg_placements(
    args: &[Value],
    func: &Function,
    selected: &SelectedFunction,
) -> Vec<(Option<PhysReg>, Option<usize>)> {
    let mut gpr = 0usize;
    let mut xmm = 0usize;
    let mut stack = 0usize;
    args.iter()
        .enumerate()
        .map(|(index, value)| {
            let ty = value_ty(func, selected, *value);
            if cfg!(windows) {
                if index < 4 {
                    let register = if ty == Ty::F64 {
                        [PhysReg::Xmm0, PhysReg::Xmm1, PhysReg::Xmm2, PhysReg::Xmm3][index]
                    } else {
                        [PhysReg::Rcx, PhysReg::Rdx, PhysReg::R8, PhysReg::R9][index]
                    };
                    (Some(register), None)
                } else {
                    let slot = stack;
                    stack += 1;
                    (None, Some(slot))
                }
            } else if ty == Ty::F64 {
                if xmm < forge_regalloc::SYSV_FLOAT_ARGS.len() {
                    let register = forge_regalloc::SYSV_FLOAT_ARGS[xmm];
                    xmm += 1;
                    (Some(register), None)
                } else {
                    let slot = stack;
                    stack += 1;
                    (None, Some(slot))
                }
            } else if gpr < forge_regalloc::SYSV_INT_ARGS.len() {
                let register = forge_regalloc::SYSV_INT_ARGS[gpr];
                gpr += 1;
                (Some(register), None)
            } else {
                let slot = stack;
                stack += 1;
                (None, Some(slot))
            }
        })
        .collect()
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
