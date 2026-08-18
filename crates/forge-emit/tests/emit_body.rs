mod disasm;
use disasm::disassemble;

use forge_ir::builder::Builder;
use forge_ir::{Inst, Terminator, Ty, Value};
use forge_regalloc::Location;
use forge_x64::PhysReg;
use std::collections::HashMap;

fn dummy_span() -> forge_syntax::span::Span {
    forge_syntax::span::Span::new(0, 0)
}

#[test]
fn straight_line_function_returns_a_constant() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let c = b.emit(
        entry,
        Inst::ConstF64(2.5f64.to_bits()),
        Ty::F64,
        dummy_span(),
    );
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(c));

    let selected = forge_x64::select(&b.f);
    let assignment: HashMap<Value, Location> =
        [(c, Location::Reg(PhysReg::Xmm0))].into_iter().collect();

    let code = forge_emit::emit_body(&b.f, &selected, &assignment);
    let lines = disassemble(&code);
    // movsd xmm0,[pool]; ret  (dst already equals the ABI return register, no
    // extra mov). Only the first two lines are real instructions -- the
    // constant pool's raw f64 bytes are placed immediately after `ret` (per
    // `emit_body`'s doc comment) with no jump over them, so a blind linear
    // disassembler (which can't know where code ends and data begins) keeps
    // decoding those data bytes as further bogus instructions. That's
    // expected and not a correctness issue: only `lines[0..2]` are backed by
    // real encoded instructions.
    assert!(lines[0].starts_with("movsd xmm0,"), "got: {}", lines[0]);
    assert_eq!(lines[1], "ret");
}

#[test]
fn return_moves_value_into_abi_register_when_not_already_there() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let c = b.emit(entry, Inst::ConstI64(7), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(c));

    let selected = forge_x64::select(&b.f);
    let assignment: HashMap<Value, Location> =
        [(c, Location::Reg(PhysReg::Rcx))].into_iter().collect();

    let code = forge_emit::emit_body(&b.f, &selected, &assignment);
    let lines = disassemble(&code);
    assert_eq!(lines, vec!["mov rcx,7", "mov rax,rcx", "ret"]);
}

#[test]
fn jump_only_multi_block_function_resolves_labels() {
    let mut b = Builder::new();
    let entry = b.create_block();
    let next = b.create_block();
    b.seal_block(entry);
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Jump(next));
    b.add_pred(next, entry);
    b.seal_block(next);
    let c = b.emit(next, Inst::ConstI64(1), Ty::I64, dummy_span());
    b.f.blocks[next.0 as usize].term = Some(Terminator::Return(c));

    let selected = forge_x64::select(&b.f);
    let assignment: HashMap<Value, Location> =
        [(c, Location::Reg(PhysReg::Rax))].into_iter().collect();

    let code = forge_emit::emit_body(&b.f, &selected, &assignment);
    let lines = disassemble(&code);
    // `next` is a forward reference (not yet bound when `jmp` is emitted in
    // `entry`), so `Assembler::jmp` unconditionally takes the near/rel32
    // form -- NasmFormatter doesn't print a "near"/"short" qualifier for
    // `jmp` (unlike `jcc`), just the resolved absolute target address. The
    // near jmp is 5 bytes (offsets 0..5), so `next`'s block landing right
    // after it at offset 5 is exactly the control-flow this test checks for.
    assert_eq!(lines, vec!["jmp 5", "mov rax,1", "ret"]);
}

#[test]
fn constant_pool_is_placed_after_every_block_not_just_the_first() {
    // A genuinely multi-block function (entry jumps to a second block, same
    // Builder shape as `jump_only_multi_block_function_resolves_labels`)
    // where the F64 constant lives in the NON-entry block. A single-block
    // function can't distinguish "pool placed after all block code" from
    // "pool placed after block 1" -- with two blocks, if `place_pool` were
    // ever called before the loop finishes (or between blocks), the pool's
    // bytes would land before `next`'s code instead of after it.
    let mut b = Builder::new();
    let entry = b.create_block();
    let next = b.create_block();
    b.seal_block(entry);
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Jump(next));
    b.add_pred(next, entry);
    b.seal_block(next);
    let bits = 2.5f64.to_bits();
    let c = b.emit(next, Inst::ConstF64(bits), Ty::F64, dummy_span());
    b.f.blocks[next.0 as usize].term = Some(Terminator::Return(c));

    let selected = forge_x64::select(&b.f);
    let assignment: HashMap<Value, Location> =
        [(c, Location::Reg(PhysReg::Xmm0))].into_iter().collect();

    let code = forge_emit::emit_body(&b.f, &selected, &assignment);
    // The constant pool holds exactly this one f64's raw bits, written by
    // `place_pool` via `emit_u64` (little-endian). If the pool is genuinely
    // placed after ALL block code (not just after block 1, or spliced
    // between blocks), those 8 bytes must be the very last bytes of `code`.
    let pool_bytes = bits.to_le_bytes();
    assert_eq!(&code[code.len() - 8..], &pool_bytes);
}

#[test]
fn branch_diamond_emits_test_jcc_jmp() {
    let mut b = Builder::new();
    let entry = b.create_block();
    let then_blk = b.create_block();
    let else_blk = b.create_block();
    b.seal_block(entry);

    let cond = b.emit(entry, Inst::ConstBool(true), Ty::Bool, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
        cond,
        then_: then_blk,
        else_: else_blk,
    });
    b.add_pred(then_blk, entry);
    b.add_pred(else_blk, entry);
    b.seal_block(then_blk);
    b.seal_block(else_blk);

    let then_val = b.emit(then_blk, Inst::ConstI64(1), Ty::I64, dummy_span());
    b.f.blocks[then_blk.0 as usize].term = Some(Terminator::Return(then_val));
    let else_val = b.emit(else_blk, Inst::ConstI64(2), Ty::I64, dummy_span());
    b.f.blocks[else_blk.0 as usize].term = Some(Terminator::Return(else_val));

    let selected = forge_x64::select(&b.f);
    let mut assignment: HashMap<Value, Location> = HashMap::new();
    assignment.insert(cond, Location::Reg(PhysReg::Rax));
    assignment.insert(then_val, Location::Reg(PhysReg::Rax));
    assignment.insert(else_val, Location::Reg(PhysReg::Rax));

    let code = forge_emit::emit_body(&b.f, &selected, &assignment);
    let lines = disassemble(&code);
    assert_eq!(lines[0], "mov rax,1"); // ConstBool(true) lowers via LoadImmI64
    assert_eq!(lines[1], "test rax,rax");
    // Both then_blk and else_blk are forward references from the branch (not
    // yet bound when `jcc`/`jmp` are emitted), so `Assembler::jcc`/`jmp`
    // unconditionally take the near/rel32 form (see their doc comments in
    // forge-x64/src/assembler.rs) -- there is no short-form encoding to
    // choose from here, unlike same-block or backward jumps.
    assert!(lines[2].starts_with("jne near"), "got: {}", lines[2]);
    assert!(lines[3].starts_with("jmp "), "got: {}", lines[3]);
    // Confirms the branch itself, not just instruction shape: jne must land
    // on the block computing then_val (1), and the fallthrough jmp must land
    // on the block computing else_val (2).
    assert_eq!(lines[4], "mov rax,2"); // else_blk (fallthrough target of jmp)
    assert_eq!(lines[5], "ret");
    assert_eq!(lines[6], "mov rax,1"); // then_blk (jne target)
    assert_eq!(lines[7], "ret");
}
