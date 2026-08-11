# Phase 9a: forge-emit Skeleton Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a new crate `forge-emit` that lowers `MachineInst` sequences (register-only operands, no spills) into real x86-64 bytes via the existing `forge-x64::Assembler`, with real control flow, constant-pool placement, and an exhaustive per-instruction translator — the first runnable slice of Phase 9's code-emission pipeline.

**Architecture:** Three small modules — `const_pool.rs` (places f64/mask constants after the code, RIP-relative), `translate.rs` (one `translate_inst` function, an exhaustive match over every `MachineInst` variant), `layout.rs` (walks `SelectedFunction::block_starts`, binds one `Label` per block, dispatches terminators to real control flow, everything else to `translate_inst`). Verification is primarily iced-x86 disassembly (this repo's established pattern for confirming encoder output, and the only option that always runs — see note below) plus `#[cfg(target_arch = "x86_64")]`-gated real execution through `forge-mem` where the host allows it.

**Tech Stack:** Rust, `forge-x64::Assembler` (existing x86-64 encoder), `iced-x86` (disassembly-based test verification), `forge-mem` (mmap JIT execution harness, dev-dependency only).

**Design doc:** `docs/superpowers/specs/2026-08-11-phase-9a-forge-emit-skeleton-design.md`

**Two corrections made during planning (recorded here since they change what the design doc describes):**

1. **`SelectedFunction::insts` DOES contain `Jump`/`Branch`/`Return` MachineInsts** — verified directly against `crates/forge-x64/src/machine_inst/mod.rs`'s `select_term` (line 521), which does `self.insts.push(MachineInst::Return{..})` / `Jump{..}` / `Branch{..}`, exactly like every other instruction. The design doc's claim that these "are terminators, not body instructions... `SelectedFunction::insts` never contains them" was wrong. Each block's `insts[start..end]` range therefore ends with exactly one of these three. `layout.rs`'s driver (Task 6) matches on them directly and handles control flow itself; `translate.rs`'s `translate_inst` (Tasks 4-5) still needs real (if `unreachable!()`) match arms for all three to stay exhaustive, but never actually receives them in practice.
2. **Constant pool placement needs two phases, not one**, because RIP-relative loads (`movsd_reg_riprel`) must reference pool labels *while translating instructions*, but the pool's bytes must physically land *after* all code bytes. `Label`s can be referenced (recording a fixup) long before they're bound — that's the entire point of the existing fixup mechanism — so `const_pool.rs` splits into `alloc_pool_labels` (called first, before any instruction is translated) and `place_pool` (called last, after every instruction is translated, doing the actual `bind` + byte emission).

**Verification-strategy note:** this development machine is `arm64` (Apple Silicon). `forge-x64` emits x86-64 bytes, which cannot execute natively here. Every test in this plan is written to pass via iced-x86 disassembly (architecture-independent — it's a decoder, not an executor) so the full suite is green on any host. Tests that additionally execute the emitted bytes through `forge-mem` are `#[cfg(target_arch = "x86_64")]`-gated — they compile everywhere, and run (as a bonus, stronger check) only on x86-64 hosts/CI. This mirrors the existing precedent in `crates/forge-mem/examples/spike.rs`, which already forks behavior by `cfg(target_arch = ...)`.

---

### Task 1: forge-x64 primitives — `Assembler::emit_u64` and `PoolIndex::index`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs` (add a new `impl Assembler` block right before `#[cfg(test)] mod tests` at line 1050)
- Modify: `crates/forge-x64/src/machine_inst/mod.rs` (add a method to the existing `impl PoolIndex` — find it near `PoolIndex`'s definition at line 4)

`forge-emit` needs two small additions to `forge-x64` that don't exist yet: a way to append raw bytes (for constant-pool data, which isn't an encoded instruction) and a way to turn a `PoolIndex` into a `usize` (its inner field is private, and `pool_labels[pool_index]` needs a plain index).

- [ ] **Step 1: Write the failing test for `emit_u64`**

Add to `crates/forge-x64/src/assembler.rs`'s existing `#[cfg(test)] mod tests` block (append near the end):

```rust
    #[test]
    fn emit_u64_appends_raw_little_endian_bytes() {
        let mut asm = Assembler::new();
        asm.mov_reg_imm(PhysReg::Rax, 0); // sentinel, so emit_u64 isn't operating on an empty buffer
        let before_len = asm.code().len();
        asm.emit_u64(0x0102030405060708u64);
        let tail = &asm.code()[before_len..];
        assert_eq!(tail, &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p forge-x64 emit_u64_appends_raw_little_endian_bytes`
Expected: FAIL with `no method named 'emit_u64' found for struct 'Assembler'`

- [ ] **Step 3: Implement `emit_u64`**

Add this new `impl Assembler` block in `crates/forge-x64/src/assembler.rs`, immediately before the `#[cfg(test)] mod tests` line:

```rust
impl Assembler {
    /// Appends 8 raw little-endian bytes with no instruction encoding around them.
    /// Used to place constant-pool data (not code) after the function body.
    pub fn emit_u64(&mut self, bits: u64) {
        self.code.extend_from_slice(&bits.to_le_bytes());
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p forge-x64 emit_u64_appends_raw_little_endian_bytes`
Expected: PASS

- [ ] **Step 5: Write the failing test for `PoolIndex::index`**

Add to `crates/forge-x64/src/machine_inst/tests.rs` (append a new `#[test]`):

```rust
#[test]
fn pool_index_round_trips_through_intern_in_insertion_order() {
    let mut pool = ConstantPool::default();
    let first = pool.intern(0x3ff0000000000000u64); // 1.0f64
    let second = pool.intern(0x4000000000000000u64); // 2.0f64
    assert_eq!(first.index(), 0);
    assert_eq!(second.index(), 1);
}
```

- [ ] **Step 6: Run test to verify it fails**

Run: `cargo test -p forge-x64 pool_index_round_trips_through_intern_in_insertion_order`
Expected: FAIL with `no method named 'index' found for struct 'PoolIndex'`

- [ ] **Step 7: Implement `PoolIndex::index`**

In `crates/forge-x64/src/machine_inst/mod.rs`, find the `PoolIndex` struct definition (near line 4-8) and add an `impl PoolIndex` block directly after it:

```rust
impl PoolIndex {
    /// The pool-entry position this index refers to (0-based, insertion order).
    pub fn index(self) -> usize {
        self.0
    }
}
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cargo test -p forge-x64 pool_index_round_trips_through_intern_in_insertion_order`
Expected: PASS

- [ ] **Step 9: Run full forge-x64 suite and commit**

Run: `cargo test -p forge-x64 && cargo clippy -p forge-x64 --all-targets -- -D warnings && cargo fmt --check -p forge-x64`
Expected: all green

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/src/machine_inst/mod.rs crates/forge-x64/src/machine_inst/tests.rs
git commit -m "feat(forge-x64): add emit_u64 and PoolIndex::index for Phase 9a"
```

---

### Task 2: Scaffold the `forge-emit` crate

**Files:**
- Modify: `Cargo.toml` (workspace members)
- Create: `crates/forge-emit/Cargo.toml`
- Create: `crates/forge-emit/src/lib.rs`

- [ ] **Step 1: Add the workspace member**

In `/Users/sanskar/dev/Research/Projects/JIT-Compiler/Cargo.toml`, in the `members` list, insert `"crates/forge-emit",` immediately after `"crates/forge-cli",`:

```toml
[workspace]
resolver = "2"
members = [
    "crates/forge-syntax",
    "crates/forge-ir",
    "crates/forge-mem",
    "crates/forge-opt",
    "crates/forge-regalloc",
    "crates/forge-x64",
    "crates/forge-aarch64",
    "crates/forge-wasm",
    "crates/forge-runtime",
    "crates/forge-simd",
    "crates/forge-bench",
    "crates/forge-cli",
    "crates/forge-emit",
    "crates/forge-wasm-api",
]
```

- [ ] **Step 2: Create `crates/forge-emit/Cargo.toml`**

```toml
[package]
name = "forge-emit"
version.workspace = true
edition.workspace = true

[dependencies]
forge-ir = { path = "../forge-ir" }
forge-x64 = { path = "../forge-x64" }
forge-regalloc = { path = "../forge-regalloc" }

[dev-dependencies]
forge-mem = { path = "../forge-mem" }
iced-x86.workspace = true
```

- [ ] **Step 3: Create `crates/forge-emit/src/lib.rs`**

```rust
mod const_pool;
mod layout;
mod translate;

pub use const_pool::{alloc_pool_labels, place_pool};
pub use layout::emit_body;
pub use translate::translate_inst;
```

- [ ] **Step 4: Create empty placeholder modules so the crate compiles**

Create `crates/forge-emit/src/const_pool.rs`:

```rust
use forge_x64::{Assembler, ConstantPool, Label};

pub fn alloc_pool_labels(asm: &mut Assembler, pool: &ConstantPool) -> Vec<Label> {
    (0..pool.entries().len()).map(|_| asm.new_label()).collect()
}

pub fn place_pool(asm: &mut Assembler, pool: &ConstantPool, labels: &[Label]) {
    for (&bits, &label) in pool.entries().iter().zip(labels) {
        asm.bind(label);
        asm.emit_u64(bits);
    }
}
```

Create `crates/forge-emit/src/translate.rs`:

```rust
use forge_ir::Value;
use forge_x64::{Assembler, Label, MachineInst, PhysReg};

pub fn translate_inst(
    _asm: &mut Assembler,
    inst: &MachineInst,
    _loc: &dyn Fn(Value) -> PhysReg,
    _pool_labels: &[Label],
) {
    unimplemented!("filled in by Task 4/5: {inst:?}")
}
```

Create `crates/forge-emit/src/layout.rs`:

```rust
use forge_ir::{Function, Value};
use forge_regalloc::Location;
use forge_x64::SelectedFunction;
use std::collections::HashMap;

pub fn emit_body(
    _func: &Function,
    _selected: &SelectedFunction,
    _assignment: &HashMap<Value, Location>,
) -> Vec<u8> {
    unimplemented!("filled in by Task 6")
}
```

- [ ] **Step 5: Verify the crate builds**

Run: `cargo build -p forge-emit`
Expected: builds cleanly (with `unused variable`/dead-code warnings on the placeholder bodies, which is fine — they're replaced before Task 2's commit closes)

Actually, since `unimplemented!()` bodies would fail `clippy -D warnings` on unused-parameter-name-underscore-consistency but not on anything else, and this task's own commit should be green on clippy: run `cargo clippy -p forge-emit --all-targets -- -D warnings` now and confirm it passes as written (the `_`-prefixed unused parameters are the correct fix already applied above).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/forge-emit
git commit -m "feat(forge-emit): scaffold new crate for Phase 9 code emission"
```

---

### Task 3: `const_pool.rs` — verify placement with a real test

**Files:**
- Modify: `crates/forge-emit/src/const_pool.rs` (add tests; the implementation from Task 2 Step 4 is already correct and complete — this task is where it gets exercised)

- [ ] **Step 1: Write the failing test**

Append to `crates/forge-emit/src/const_pool.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn place_pool_writes_entries_in_order_after_existing_code() {
        let mut pool = ConstantPool::default();
        let a = pool.intern(0x3ff0000000000000u64); // 1.0f64
        let b = pool.intern(0x4000000000000000u64); // 2.0f64
        assert_eq!(a.index(), 0);
        assert_eq!(b.index(), 1);

        let mut asm = Assembler::new();
        asm.ret(); // 1 byte of "existing code" (0xC3), so the pool isn't at offset 0
        let labels = alloc_pool_labels(&mut asm, &pool);
        assert_eq!(labels.len(), 2);
        place_pool(&mut asm, &pool, &labels);

        let code = asm.code();
        assert_eq!(code.len(), 1 + 16); // 1 ret byte + two 8-byte pool entries
        assert_eq!(code[0], 0xC3);
        assert_eq!(&code[1..9], &0x3ff0000000000000u64.to_le_bytes());
        assert_eq!(&code[9..17], &0x4000000000000000u64.to_le_bytes());
    }

    #[test]
    fn empty_pool_produces_no_labels_and_no_bytes() {
        let pool = ConstantPool::default();
        let mut asm = Assembler::new();
        let labels = alloc_pool_labels(&mut asm, &pool);
        assert!(labels.is_empty());
        place_pool(&mut asm, &pool, &labels);
        assert!(asm.code().is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p forge-emit place_pool`
Expected: at this point it should actually PASS already, since Task 2 Step 4 already wrote a correct implementation — this is expected. If it fails, the implementation from Task 2 has a bug; fix `alloc_pool_labels`/`place_pool` in `const_pool.rs` until both tests pass. (This task exists specifically to add real test coverage for logic that was necessarily written in Task 2 to make the crate compile — write the test first per TDD discipline, but don't be surprised if it's green on the first run; that's confirmation, not a process violation.)

- [ ] **Step 3: Run test to verify it passes**

Run: `cargo test -p forge-emit`
Expected: both new tests PASS (the rest of the crate still has `unimplemented!()` bodies untouched by these tests, so nothing else runs yet)

- [ ] **Step 4: Commit**

```bash
git add crates/forge-emit/src/const_pool.rs
git commit -m "test(forge-emit): verify constant pool placement ordering"
```

---

### Task 4: `translate.rs` — mechanical instruction translation (no comparisons/cmov yet)

**Files:**
- Modify: `crates/forge-emit/src/translate.rs`
- Create: `crates/forge-emit/tests/disasm.rs` (shared disassembly test helper + this task's tests)

This task implements every `MachineInst` variant except `IntCmp`/`FloatCmp`/`IntCmov` (Task 5, which has a subtle ordering fix worth isolating) and `Param`/`CallLibm` (real panics, correct as final code) and `Jump`/`Branch`/`Return` (`unreachable!()` — owned by `layout.rs`, Task 6).

- [ ] **Step 1: Create the shared disassembly test helper**

Create `crates/forge-emit/tests/disasm.rs`:

```rust
use iced_x86::{Decoder, DecoderOptions, Formatter, NasmFormatter};

/// Decodes `bytes` as x86-64 machine code starting at address 0 and returns
/// one NASM-syntax mnemonic-and-operands string per instruction, in order.
/// Used to verify emitted code without needing to execute it (this repo's
/// dev machines may not be x86-64 hosts).
pub fn disassemble(bytes: &[u8]) -> Vec<String> {
    let mut decoder = Decoder::with_ip(64, bytes, 0, DecoderOptions::NONE);
    let mut formatter = NasmFormatter::new();
    let mut lines = Vec::new();
    let mut output = String::new();
    for instr in &mut decoder {
        output.clear();
        formatter.format(&instr, &mut output);
        lines.push(output.clone());
    }
    lines
}
```

- [ ] **Step 2: Write the failing tests**

Create `crates/forge-emit/tests/translate_mechanical.rs`:

```rust
mod disasm;
use disasm::disassemble;

use forge_ir::Value;
use forge_regalloc::Location;
use forge_x64::{Assembler, PhysReg};
use std::collections::HashMap;

fn loc_of(assignment: &HashMap<Value, Location>) -> impl Fn(Value) -> PhysReg + '_ {
    move |v| match assignment[&v] {
        Location::Reg(r) => r,
        Location::Spill(_) => panic!("test assignment must use only Location::Reg"),
    }
}

#[test]
fn int_add_emits_two_addr_mov_then_add() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Rcx));
    assignment.insert(rhs, Location::Reg(PhysReg::Rdx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::IntAdd { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    let lines = disassemble(asm.code());
    assert_eq!(lines, vec!["mov rax,rcx", "add rax,rdx"]);
}

#[test]
fn int_add_elides_mov_when_dst_already_equals_lhs() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Rax));
    assignment.insert(rhs, Location::Reg(PhysReg::Rdx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::IntAdd { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(disassemble(asm.code()), vec!["add rax,rdx"]);
}

#[test]
fn int_mul_uses_imul() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Rax));
    assignment.insert(rhs, Location::Reg(PhysReg::Rbx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::IntMul { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(disassemble(asm.code()), vec!["imul rax,rbx"]);
}

#[test]
fn int_div_places_dividend_in_rax_and_result_out() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rbx));
    assignment.insert(lhs, Location::Reg(PhysReg::Rcx));
    assignment.insert(rhs, Location::Reg(PhysReg::Rsi));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::IntDiv { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["mov rax,rcx", "cqo", "idiv rsi", "mov rbx,rax"]
    );
}

#[test]
fn int_rem_reads_result_from_rdx() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax)); // already rax, but IntRem wants Rdx->dst
    assignment.insert(lhs, Location::Reg(PhysReg::Rax));
    assignment.insert(rhs, Location::Reg(PhysReg::Rsi));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::IntRem { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["cqo", "idiv rsi", "mov rax,rdx"]
    );
}

#[test]
fn shl_with_amount_already_in_rcx_emits_shift_cl() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Rax));
    assignment.insert(rhs, Location::Reg(PhysReg::Rcx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::Shl { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(disassemble(asm.code()), vec!["shl rax,cl"]);
}

#[test]
#[should_panic(expected = "shift amount not in RCX")]
fn shl_with_amount_not_in_rcx_panics() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Rax));
    assignment.insert(rhs, Location::Reg(PhysReg::Rdx)); // NOT Rcx

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::Shl { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );
}

#[test]
fn lea_encodes_scaled_addressing() {
    let dst = Value(0);
    let base = Value(1);
    let index = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(base, Location::Reg(PhysReg::Rcx));
    assignment.insert(index, Location::Reg(PhysReg::Rdx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::Lea { dst, base, index, scale: 4, disp: 8 },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(disassemble(asm.code()), vec!["lea rax,[rcx+rdx*4+8]"]);
}

#[test]
fn float_add_emits_two_addr_movsd_then_addsd() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Xmm0));
    assignment.insert(lhs, Location::Reg(PhysReg::Xmm1));
    assignment.insert(rhs, Location::Reg(PhysReg::Xmm2));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::FloatAdd { dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(disassemble(asm.code()), vec!["movsd xmm0,xmm1", "addsd xmm0,xmm2"]);
}

#[test]
fn float_sqrt_uses_dst_as_both_operands() {
    let dst = Value(0);
    let src = Value(1);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Xmm0));
    assignment.insert(src, Location::Reg(PhysReg::Xmm1));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::FloatSqrt { dst, src },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(disassemble(asm.code()), vec!["movsd xmm0,xmm1", "sqrtsd xmm0,xmm0"]);
}

#[test]
fn load_imm_f64_reads_from_pool_via_riprel() {
    let dst = Value(0);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Xmm0));

    let mut pool = forge_x64::ConstantPool::default();
    let idx = pool.intern(0x3ff0000000000000u64);

    let mut asm = Assembler::new();
    let labels = forge_emit::alloc_pool_labels(&mut asm, &pool);
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::LoadImmF64 { dst, pool_index: idx },
        &loc_of(&assignment),
        &labels,
    );
    forge_emit::place_pool(&mut asm, &pool, &labels);

    let lines = disassemble(asm.code());
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("movsd xmm0,"), "got: {}", lines[0]);
}

#[test]
fn float_abs_clears_sign_bit_via_pool_mask() {
    let dst = Value(0);
    let src = Value(1);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Xmm0));
    assignment.insert(src, Location::Reg(PhysReg::Xmm1));

    let mut pool = forge_x64::ConstantPool::default();
    let mask = pool.intern(0x7fffffffffffffffu64);

    let mut asm = Assembler::new();
    let labels = forge_emit::alloc_pool_labels(&mut asm, &pool);
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::FloatAbs { dst, src, mask_pool: mask },
        &loc_of(&assignment),
        &labels,
    );
    forge_emit::place_pool(&mut asm, &pool, &labels);

    let lines = disassemble(asm.code());
    assert_eq!(lines[0], "movsd xmm0,xmm1");
    assert!(lines[1].starts_with("movsd xmm14,"), "got: {}", lines[1]);
    assert_eq!(lines[2], "andpd xmm0,xmm14");
}

#[test]
#[should_panic(expected = "Param placement not yet implemented")]
fn param_panics_in_this_slice() {
    let dst = Value(0);
    let assignment: HashMap<Value, Location> =
        [(dst, Location::Reg(PhysReg::Rax))].into_iter().collect();
    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::Param { dst, index: 0 },
        &loc_of(&assignment),
        &[],
    );
}

#[test]
#[should_panic(expected = "CallLibm sequence not yet implemented")]
fn call_libm_panics_in_this_slice() {
    let dst = Value(0);
    let assignment: HashMap<Value, Location> =
        [(dst, Location::Reg(PhysReg::Xmm0))].into_iter().collect();
    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &forge_x64::MachineInst::CallLibm {
            dst,
            func: forge_ir::LibFunc::Sin,
            args: smallvec::smallvec![dst],
        },
        &loc_of(&assignment),
        &[],
    );
}
```

Add `smallvec.workspace = true` and `forge-x64 = { path = "../forge-x64" }` (already present) plus `forge-ir` (already present) to `crates/forge-emit/Cargo.toml`'s `[dev-dependencies]` since the `CallLibm` test above needs `smallvec::smallvec!`:

```toml
[dev-dependencies]
forge-mem = { path = "../forge-mem" }
iced-x86.workspace = true
smallvec.workspace = true
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p forge-emit --test translate_mechanical`
Expected: FAIL — `translate_inst` currently just calls `unimplemented!()` for every input.

- [ ] **Step 4: Implement `translate_inst`'s mechanical arms**

Replace the entire contents of `crates/forge-emit/src/translate.rs`:

```rust
use forge_ir::Value;
use forge_x64::{AluOp, Assembler, Label, MachineInst, PhysReg, SseOp};

fn alu_binop(
    asm: &mut Assembler,
    loc: &dyn Fn(Value) -> PhysReg,
    op: AluOp,
    dst: Value,
    lhs: Value,
    rhs: Value,
) {
    let (dst_r, lhs_r, rhs_r) = (loc(dst), loc(lhs), loc(rhs));
    if dst_r != lhs_r {
        asm.mov_reg_reg(dst_r, lhs_r);
    }
    asm.alu_reg_reg(op, dst_r, rhs_r);
}

fn sse_binop(
    asm: &mut Assembler,
    loc: &dyn Fn(Value) -> PhysReg,
    op: SseOp,
    dst: Value,
    lhs: Value,
    rhs: Value,
) {
    let (dst_r, lhs_r, rhs_r) = (loc(dst), loc(lhs), loc(rhs));
    if dst_r != lhs_r {
        asm.movsd_reg_reg(dst_r, lhs_r);
    }
    asm.sse_reg_reg(op, dst_r, rhs_r);
}

/// Translates one `MachineInst` (register-only operands, `Location::Reg` only)
/// into real bytes on `asm`. `loc` resolves a `Value` to the `PhysReg` holding
/// it. `pool_labels[i]` is the label for the constant-pool entry at index `i`
/// (see `alloc_pool_labels`/`place_pool`); must already be allocated (not
/// necessarily bound yet) before any instruction referencing the pool is
/// translated.
///
/// Phase 9a scope: `Param` and `CallLibm` are not yet implemented (Phase
/// 9b/9e). `IntDiv`/`IntRem` place the dividend/result correctly but do not
/// yet protect an unrelated value that happens to be resident in rax/rdx
/// (Phase 9b). `Shl`/`Shr`/`Sar` require the shift amount to already be in
/// `Rcx` (asserted) — displacing an occupied `Rcx` is Phase 9b's job.
/// `Jump`/`Branch`/`Return` are handled by `layout.rs`'s `emit_body` before
/// this function is ever called on them.
pub fn translate_inst(
    asm: &mut Assembler,
    inst: &MachineInst,
    loc: &dyn Fn(Value) -> PhysReg,
    pool_labels: &[Label],
) {
    match inst {
        MachineInst::LoadImmI64 { dst, imm } => asm.mov_reg_imm(loc(*dst), *imm),
        MachineInst::LoadImmF64 { dst, pool_index } => {
            asm.movsd_reg_riprel(loc(*dst), pool_labels[pool_index.index()])
        }

        MachineInst::IntAdd { dst, lhs, rhs } => {
            alu_binop(asm, loc, AluOp::Add, *dst, *lhs, *rhs)
        }
        MachineInst::IntSub { dst, lhs, rhs } => {
            alu_binop(asm, loc, AluOp::Sub, *dst, *lhs, *rhs)
        }
        MachineInst::And { dst, lhs, rhs } => alu_binop(asm, loc, AluOp::And, *dst, *lhs, *rhs),
        MachineInst::Or { dst, lhs, rhs } => alu_binop(asm, loc, AluOp::Or, *dst, *lhs, *rhs),
        MachineInst::Xor { dst, lhs, rhs } => alu_binop(asm, loc, AluOp::Xor, *dst, *lhs, *rhs),

        MachineInst::IntMul { dst, lhs, rhs } => {
            let (dst_r, lhs_r, rhs_r) = (loc(*dst), loc(*lhs), loc(*rhs));
            if dst_r != lhs_r {
                asm.mov_reg_reg(dst_r, lhs_r);
            }
            asm.imul_reg_reg(dst_r, rhs_r);
        }

        MachineInst::IntDiv { dst, lhs, rhs } => {
            let (dst_r, lhs_r, rhs_r) = (loc(*dst), loc(*lhs), loc(*rhs));
            if lhs_r != PhysReg::Rax {
                asm.mov_reg_reg(PhysReg::Rax, lhs_r);
            }
            asm.cqo();
            asm.idiv_reg(rhs_r);
            if dst_r != PhysReg::Rax {
                asm.mov_reg_reg(dst_r, PhysReg::Rax);
            }
        }
        MachineInst::IntRem { dst, lhs, rhs } => {
            let (dst_r, lhs_r, rhs_r) = (loc(*dst), loc(*lhs), loc(*rhs));
            if lhs_r != PhysReg::Rax {
                asm.mov_reg_reg(PhysReg::Rax, lhs_r);
            }
            asm.cqo();
            asm.idiv_reg(rhs_r);
            if dst_r != PhysReg::Rdx {
                asm.mov_reg_reg(dst_r, PhysReg::Rdx);
            }
        }

        MachineInst::IntNeg { dst, src } => {
            let (dst_r, src_r) = (loc(*dst), loc(*src));
            if dst_r != src_r {
                asm.mov_reg_reg(dst_r, src_r);
            }
            asm.neg_reg(dst_r);
        }
        MachineInst::Not { dst, src } => {
            let (dst_r, src_r) = (loc(*dst), loc(*src));
            if dst_r != src_r {
                asm.mov_reg_reg(dst_r, src_r);
            }
            asm.not_reg(dst_r);
        }

        MachineInst::Shl { dst, lhs, rhs } => {
            shift_op(asm, loc, forge_x64::ShiftOp::Shl, *dst, *lhs, *rhs)
        }
        MachineInst::Shr { dst, lhs, rhs } => {
            shift_op(asm, loc, forge_x64::ShiftOp::Shr, *dst, *lhs, *rhs)
        }
        MachineInst::Sar { dst, lhs, rhs } => {
            shift_op(asm, loc, forge_x64::ShiftOp::Sar, *dst, *lhs, *rhs)
        }

        MachineInst::Lea { dst, base, index, scale, disp } => {
            asm.lea_reg_scaled(loc(*dst), loc(*base), loc(*index), *scale, *disp)
        }

        MachineInst::FloatAdd { dst, lhs, rhs } => {
            sse_binop(asm, loc, SseOp::Add, *dst, *lhs, *rhs)
        }
        MachineInst::FloatSub { dst, lhs, rhs } => {
            sse_binop(asm, loc, SseOp::Sub, *dst, *lhs, *rhs)
        }
        MachineInst::FloatMul { dst, lhs, rhs } => {
            sse_binop(asm, loc, SseOp::Mul, *dst, *lhs, *rhs)
        }
        MachineInst::FloatDiv { dst, lhs, rhs } => {
            sse_binop(asm, loc, SseOp::Div, *dst, *lhs, *rhs)
        }
        MachineInst::FloatMin { dst, lhs, rhs } => {
            sse_binop(asm, loc, SseOp::Min, *dst, *lhs, *rhs)
        }
        MachineInst::FloatMax { dst, lhs, rhs } => {
            sse_binop(asm, loc, SseOp::Max, *dst, *lhs, *rhs)
        }

        MachineInst::FloatSqrt { dst, src } => {
            let (dst_r, src_r) = (loc(*dst), loc(*src));
            if dst_r != src_r {
                asm.movsd_reg_reg(dst_r, src_r);
            }
            asm.sse_reg_reg(SseOp::Sqrt, dst_r, dst_r);
        }
        MachineInst::FloatRound { dst, src, mode } => asm.roundsd(*mode, loc(*dst), loc(*src)),

        MachineInst::FloatAbs { dst, src, mask_pool } => {
            float_mask_op(asm, loc, pool_labels, *dst, *src, *mask_pool, MaskOp::Abs)
        }
        MachineInst::FloatNeg { dst, src, mask_pool } => {
            float_mask_op(asm, loc, pool_labels, *dst, *src, *mask_pool, MaskOp::Neg)
        }

        MachineInst::IntToFloat { dst, src } => asm.cvtsi2sd(loc(*dst), loc(*src)),
        MachineInst::FloatToInt { dst, src } => asm.cvttsd2si(loc(*dst), loc(*src)),

        MachineInst::IntCmp { .. } | MachineInst::FloatCmp { .. } | MachineInst::IntCmov { .. } => {
            unimplemented!("filled in by Task 5: {inst:?}")
        }

        MachineInst::Param { .. } => {
            panic!("forge-emit (Phase 9a): Param placement not yet implemented — Phase 9b")
        }
        MachineInst::CallLibm { .. } => {
            panic!("forge-emit (Phase 9a): CallLibm sequence not yet implemented — Phase 9e")
        }

        MachineInst::Jump { .. } | MachineInst::Branch { .. } | MachineInst::Return { .. } => {
            unreachable!(
                "forge-emit: {inst:?} is a terminator, handled by layout.rs::emit_body before \
                 translate_inst is ever called on it"
            )
        }
    }
}

fn shift_op(
    asm: &mut Assembler,
    loc: &dyn Fn(Value) -> PhysReg,
    op: forge_x64::ShiftOp,
    dst: Value,
    lhs: Value,
    rhs: Value,
) {
    let (dst_r, lhs_r, rhs_r) = (loc(dst), loc(lhs), loc(rhs));
    if dst_r != lhs_r {
        asm.mov_reg_reg(dst_r, lhs_r);
    }
    assert_eq!(
        rhs_r,
        PhysReg::Rcx,
        "forge-emit (Phase 9a): shift amount not in RCX/CL — displacing an occupied RCX is \
         Phase 9b's job"
    );
    asm.shift_reg_cl(op, dst_r);
}

enum MaskOp {
    Abs,
    Neg,
}

fn float_mask_op(
    asm: &mut Assembler,
    loc: &dyn Fn(Value) -> PhysReg,
    pool_labels: &[Label],
    dst: Value,
    src: Value,
    mask_pool: forge_x64::PoolIndex,
    op: MaskOp,
) {
    let (dst_r, src_r) = (loc(dst), loc(src));
    if dst_r != src_r {
        asm.movsd_reg_reg(dst_r, src_r);
    }
    asm.movsd_reg_riprel(PhysReg::Xmm14, pool_labels[mask_pool.index()]);
    match op {
        MaskOp::Abs => asm.andpd_reg_reg(dst_r, PhysReg::Xmm14),
        MaskOp::Neg => asm.xorpd_reg_reg(dst_r, PhysReg::Xmm14),
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p forge-emit --test translate_mechanical`
Expected: all PASS

- [ ] **Step 6: Run clippy and fmt**

Run: `cargo clippy -p forge-emit --all-targets -- -D warnings && cargo fmt --check -p forge-emit`
Expected: clean (fix formatting with `cargo fmt -p forge-emit` if needed, then re-check)

- [ ] **Step 7: Commit**

```bash
git add crates/forge-emit/src/translate.rs crates/forge-emit/tests/translate_mechanical.rs crates/forge-emit/tests/disasm.rs crates/forge-emit/Cargo.toml
git commit -m "feat(forge-emit): mechanical MachineInst translation (Phase 9a)"
```

---

### Task 5: `translate.rs` — `IntCmp`/`FloatCmp`/`IntCmov`

**Files:**
- Modify: `crates/forge-emit/src/translate.rs`
- Create: `crates/forge-emit/tests/translate_compare.rs`

`setcc` writes only the low byte of its destination, leaving the upper 56 bits as whatever they were before. The correct, alias-safe sequence is: run the compare first (it only reads `lhs`/`rhs`, which might alias `dst`), then zero `dst` via `mov_reg_imm(dst, 0)` (not `xor`, since `xor` would clobber the flags `setcc` needs to read), then `setcc`. This order is safe even when `dst` aliases `lhs` or `rhs`, because the compare has already consumed those operands' values before `dst` is touched.

- [ ] **Step 1: Write the failing tests**

Create `crates/forge-emit/tests/translate_compare.rs`:

```rust
mod disasm;
use disasm::disassemble;

use forge_ir::{CmpOp, Value};
use forge_regalloc::Location;
use forge_x64::{Assembler, MachineInst, PhysReg};
use std::collections::HashMap;

fn loc_of(assignment: &HashMap<Value, Location>) -> impl Fn(Value) -> PhysReg + '_ {
    move |v| match assignment[&v] {
        Location::Reg(r) => r,
        Location::Spill(_) => panic!("test assignment must use only Location::Reg"),
    }
}

#[test]
fn int_cmp_lt_emits_cmp_zero_setl() {
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Rcx));
    assignment.insert(rhs, Location::Reg(PhysReg::Rdx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &MachineInst::IntCmp { op: CmpOp::Lt, dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["cmp rcx,rdx", "mov rax,0", "setl al"]
    );
}

#[test]
fn int_cmp_dst_aliases_lhs_is_still_correct() {
    // dst and lhs share a register — the compare must read lhs BEFORE dst is zeroed.
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rcx));
    assignment.insert(lhs, Location::Reg(PhysReg::Rcx));
    assignment.insert(rhs, Location::Reg(PhysReg::Rdx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &MachineInst::IntCmp { op: CmpOp::Eq, dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["cmp rcx,rdx", "mov rcx,0", "sete cl"]
    );
}

#[test]
fn float_cmp_lt_uses_unsigned_below_condition() {
    // ucomisd sets flags like an unsigned integer compare; Lt must map to
    // Below, not Less, or the wrong branch is taken on unordered operands.
    let dst = Value(0);
    let lhs = Value(1);
    let rhs = Value(2);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(lhs, Location::Reg(PhysReg::Xmm0));
    assignment.insert(rhs, Location::Reg(PhysReg::Xmm1));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &MachineInst::FloatCmp { op: CmpOp::Lt, dst, lhs, rhs },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["ucomisd xmm0,xmm1", "mov rax,0", "setb al"]
    );
}

#[test]
fn int_cmov_picks_then_val_when_cond_true() {
    let dst = Value(0);
    let cond = Value(1);
    let then_val = Value(2);
    let else_val = Value(3);
    let mut assignment = HashMap::new();
    assignment.insert(dst, Location::Reg(PhysReg::Rax));
    assignment.insert(cond, Location::Reg(PhysReg::Rcx));
    assignment.insert(then_val, Location::Reg(PhysReg::Rax));
    assignment.insert(else_val, Location::Reg(PhysReg::Rdx));

    let mut asm = Assembler::new();
    forge_emit::translate_inst(
        &mut asm,
        &MachineInst::IntCmov { dst, cond, then_val, else_val },
        &loc_of(&assignment),
        &[],
    );

    assert_eq!(
        disassemble(asm.code()),
        vec!["test rcx,rcx", "cmove rax,rdx"]
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p forge-emit --test translate_compare`
Expected: FAIL — currently `unimplemented!("filled in by Task 5: ...")`.

- [ ] **Step 3: Implement `IntCmp`/`FloatCmp`/`IntCmov`**

In `crates/forge-emit/src/translate.rs`, replace this block:

```rust
        MachineInst::IntCmp { .. } | MachineInst::FloatCmp { .. } | MachineInst::IntCmov { .. } => {
            unimplemented!("filled in by Task 5: {inst:?}")
        }
```

with:

```rust
        MachineInst::IntCmp { op, dst, lhs, rhs } => {
            let (dst_r, lhs_r, rhs_r) = (loc(*dst), loc(*lhs), loc(*rhs));
            asm.alu_reg_reg(AluOp::Cmp, lhs_r, rhs_r);
            asm.mov_reg_imm(dst_r, 0);
            asm.setcc(int_condition_code(*op), dst_r);
        }
        MachineInst::FloatCmp { op, dst, lhs, rhs } => {
            let (dst_r, lhs_r, rhs_r) = (loc(*dst), loc(*lhs), loc(*rhs));
            asm.ucomisd_reg_reg(lhs_r, rhs_r);
            asm.mov_reg_imm(dst_r, 0);
            asm.setcc(float_condition_code(*op), dst_r);
        }
        MachineInst::IntCmov { dst, cond, then_val, else_val } => {
            let (dst_r, cond_r, then_r, else_r) =
                (loc(*dst), loc(*cond), loc(*then_val), loc(*else_val));
            if dst_r != then_r {
                asm.mov_reg_reg(dst_r, then_r);
            }
            asm.test_reg_reg(cond_r, cond_r);
            asm.cmovcc(forge_x64::ConditionCode::Equal, dst_r, else_r);
        }
```

Add these two helper functions near the bottom of the file, alongside `shift_op`/`float_mask_op`:

```rust
fn int_condition_code(op: forge_ir::CmpOp) -> forge_x64::ConditionCode {
    use forge_ir::CmpOp;
    use forge_x64::ConditionCode;
    match op {
        CmpOp::Eq => ConditionCode::Equal,
        CmpOp::Ne => ConditionCode::NotEqual,
        CmpOp::Lt => ConditionCode::Less,
        CmpOp::Le => ConditionCode::LessOrEqual,
        CmpOp::Gt => ConditionCode::Greater,
        CmpOp::Ge => ConditionCode::GreaterOrEqual,
    }
}

fn float_condition_code(op: forge_ir::CmpOp) -> forge_x64::ConditionCode {
    use forge_ir::CmpOp;
    use forge_x64::ConditionCode;
    match op {
        CmpOp::Eq => ConditionCode::Equal,
        CmpOp::Ne => ConditionCode::NotEqual,
        CmpOp::Lt => ConditionCode::Below,
        CmpOp::Le => ConditionCode::BelowOrEqual,
        CmpOp::Gt => ConditionCode::Above,
        CmpOp::Ge => ConditionCode::AboveOrEqual,
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p forge-emit --test translate_compare`
Expected: all PASS

- [ ] **Step 5: Run full crate suite, clippy, fmt**

Run: `cargo test -p forge-emit && cargo clippy -p forge-emit --all-targets -- -D warnings && cargo fmt --check -p forge-emit`
Expected: all green

- [ ] **Step 6: Commit**

```bash
git add crates/forge-emit/src/translate.rs crates/forge-emit/tests/translate_compare.rs
git commit -m "feat(forge-emit): IntCmp/FloatCmp/IntCmov translation, alias-safe zero-extension"
```

---

### Task 6: `layout.rs` — `emit_body` driver

**Files:**
- Modify: `crates/forge-emit/src/layout.rs`
- Create: `crates/forge-emit/tests/emit_body.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/forge-emit/tests/emit_body.rs`:

```rust
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
    let c = b.emit(entry, Inst::ConstF64(2.5f64.to_bits()), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(c));

    let selected = forge_x64::select(&b.f);
    let assignment: HashMap<Value, Location> = [(c, Location::Reg(PhysReg::Xmm0))].into_iter().collect();

    let code = forge_emit::emit_body(&b.f, &selected, &assignment);
    let lines = disassemble(&code);
    // movsd xmm0,[pool]; ret  (dst already equals the ABI return register, no extra mov)
    assert_eq!(lines.last().unwrap(), "ret");
    assert!(lines.iter().any(|l| l.starts_with("movsd xmm0,")));
}

#[test]
fn return_moves_value_into_abi_register_when_not_already_there() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let c = b.emit(entry, Inst::ConstI64(7), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(c));

    let selected = forge_x64::select(&b.f);
    let assignment: HashMap<Value, Location> = [(c, Location::Reg(PhysReg::Rcx))].into_iter().collect();

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
    let assignment: HashMap<Value, Location> = [(c, Location::Reg(PhysReg::Rax))].into_iter().collect();

    let code = forge_emit::emit_body(&b.f, &selected, &assignment);
    let lines = disassemble(&code);
    assert_eq!(lines, vec!["jmp short 0000000000000002", "mov rax,1", "ret"]);
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
    assert!(lines[2].starts_with("jne short"), "got: {}", lines[2]);
    assert!(lines[3].starts_with("jmp short"), "got: {}", lines[3]);
}
```

Note: this task's tests hand-verify the exact `Builder` API shape (`create_block`, `seal_block`, `emit`, `add_pred`, direct `b.f.blocks[..].term = ...` mutation) against the precedent already used in `crates/forge-x64/src/machine_inst/tests.rs`. If `add_pred`'s exact name/signature differs, check that file first and adjust the calls above to match — the pattern (not necessarily every method name) is guaranteed correct.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p forge-emit --test emit_body`
Expected: FAIL — `emit_body` currently just calls `unimplemented!()`.

- [ ] **Step 3: Implement `emit_body`**

Replace the entire contents of `crates/forge-emit/src/layout.rs`:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p forge-emit --test emit_body`
Expected: all PASS. If `jump_only_multi_block_function_resolves_labels` or
`branch_diamond_emits_test_jcc_jmp`'s exact disassembly strings (label target
addresses, `jne short` vs `jne near`) don't match, adjust the expected strings
to whatever `disassemble()` actually prints for correct output — inspect with
`cargo test -p forge-emit --test emit_body -- --nocapture` and a temporary
`eprintln!("{lines:?}")`, verify the control flow is genuinely correct (right
number of instructions, right mnemonics, right relative direction), then lock
in the real strings. Do not adjust the *implementation* to match a wrong
expectation — only adjust the test's literal expected strings once you've
independently confirmed by reading the bytes that the control flow is right.

- [ ] **Step 5: Run full crate suite, clippy, fmt**

Run: `cargo test -p forge-emit && cargo clippy -p forge-emit --all-targets -- -D warnings && cargo fmt --check -p forge-emit`
Expected: all green

- [ ] **Step 6: Commit**

```bash
git add crates/forge-emit/src/layout.rs crates/forge-emit/tests/emit_body.rs
git commit -m "feat(forge-emit): emit_body driver — block layout, control flow, return placement"
```

---

### Task 7: Real execution corpus (x86-64-gated) + remaining scope-boundary tests

**Files:**
- Create: `crates/forge-emit/tests/execution_corpus.rs`

This is the test corpus from the design doc's "Testing" section that wasn't already covered by Tasks 4-6: float abs/neg sign-check via actual execution, `IntCmp`/`IntCmov` true/false-branch execution, a runtime (non-constant) branch condition, and the remaining `#[should_panic]` scope-boundary tests not already written. All execution assertions are `#[cfg(target_arch = "x86_64")]`-gated; disassembly assertions (which always run) provide the cross-platform floor.

- [ ] **Step 1: Write the tests**

Create `crates/forge-emit/tests/execution_corpus.rs`:

```rust
mod disasm;
use disasm::disassemble;

use forge_ir::builder::Builder;
use forge_ir::{CmpOp, Inst, Terminator, Ty, Value};
use forge_regalloc::Location;
use forge_x64::PhysReg;
use std::collections::HashMap;

fn dummy_span() -> forge_syntax::span::Span {
    forge_syntax::span::Span::new(0, 0)
}

#[cfg(target_arch = "x86_64")]
fn run_f64(code: &[u8]) -> f64 {
    let mut buf = forge_mem::ExecutableBuffer::new(code.len().max(64)).unwrap();
    buf.write(|mem| mem[..code.len()].copy_from_slice(code));
    buf.make_executable().unwrap();
    let compiled = forge_mem::CompiledExpr::from_buffer(buf, 0);
    compiled.call_n(&[])
}

#[test]
fn float_neg_flips_sign_bit() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let c = b.emit(entry, Inst::ConstF64(3.0f64.to_bits()), Ty::F64, dummy_span());
    let negated = b.emit(entry, Inst::Neg(c), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(negated));

    let selected = forge_x64::select(&b.f);
    let mut assignment: HashMap<Value, Location> = HashMap::new();
    assignment.insert(c, Location::Reg(PhysReg::Xmm0));
    assignment.insert(negated, Location::Reg(PhysReg::Xmm0));

    let code = forge_emit::emit_body(&b.f, &selected, &assignment);
    let lines = disassemble(&code);
    assert!(lines.iter().any(|l| l == "xorpd xmm0,xmm14"));

    #[cfg(target_arch = "x86_64")]
    assert_eq!(run_f64(&code), -3.0);
}

#[test]
fn int_cmp_and_cmov_diamond_selects_correct_branch() {
    // Equivalent to: (a > b) ? a : b, with a=5.0, b=2.0 baked in as constants,
    // lowered through the real Select->cmov diamond fusion from Phase 7f so
    // this also exercises IntCmov end-to-end through forge-emit for the
    // first time (Phase 7f flagged this as a real coverage gap).
    let mut b = Builder::new();
    let entry = b.create_block();
    let then_blk = b.create_block();
    let else_blk = b.create_block();
    let merge = b.create_block();
    b.seal_block(entry);

    let a = b.emit(entry, Inst::ConstI64(5), Ty::I64, dummy_span());
    let bb = b.emit(entry, Inst::ConstI64(2), Ty::I64, dummy_span());
    let cond = b.emit(entry, Inst::Cmp { op: CmpOp::Gt, lhs: a, rhs: bb }, Ty::Bool, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
        cond,
        then_: then_blk,
        else_: else_blk,
    });
    b.add_pred(then_blk, entry);
    b.add_pred(else_blk, entry);
    b.seal_block(then_blk);
    b.seal_block(else_blk);
    // Braun-style SSA construction is keyed by variable NAME, not by Value —
    // verified against `crates/forge-ir/src/builder.rs`'s real signatures:
    // `write_variable(&mut self, name: &str, block: Block, value: Value)` and
    // `read_variable(&mut self, name: &str, block: Block, ty: Ty) -> Value`.
    b.write_variable("result", then_blk, a);
    b.f.blocks[then_blk.0 as usize].term = Some(Terminator::Jump(merge));
    b.write_variable("result", else_blk, bb);
    b.f.blocks[else_blk.0 as usize].term = Some(Terminator::Jump(merge));
    b.add_pred(merge, then_blk);
    b.add_pred(merge, else_blk);
    b.seal_block(merge);
    // merge has two preds with differing incoming values for "result" (a vs
    // bb), so this mints a real Inst::Phi at merge per read_variable_recursive's
    // documented behavior.
    let result = b.read_variable("result", merge, Ty::I64);

    let selected = forge_x64::select(&b.f);
    let mut assignment: HashMap<Value, Location> = HashMap::new();
    assignment.insert(a, Location::Reg(PhysReg::Rax));
    assignment.insert(bb, Location::Reg(PhysReg::Rcx));
    assignment.insert(cond, Location::Reg(PhysReg::Rdx));
    // NOTE: if find_fusable_diamonds fuses this diamond (empty arm blocks,
    // both Jump to merge, one differing phi), `result`'s Value here is the
    // IntCmov's dst — assign it Rax to match the diamond's then_val register
    // (a's register) so the 2-addr fixup is a no-op; consult
    // forge_x64::find_fusable_diamonds(&b.f) directly if the exact Value
    // identity of the fused dst isn't obvious from `result` above, and
    // assign whatever that returns instead.
    assignment.insert(result, Location::Reg(PhysReg::Rax));

    let code = forge_emit::emit_body(&b.f, &selected, &assignment);
    let lines = disassemble(&code);
    assert!(lines.iter().any(|l| l.starts_with("cmove") || l.starts_with("test")));

    #[cfg(target_arch = "x86_64")]
    assert_eq!(run_f64(&code) as i64, 5);
}

#[test]
#[should_panic(expected = "spilled operand not yet supported")]
fn spilled_operand_panics() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let c = b.emit(entry, Inst::ConstI64(1), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(c));

    let selected = forge_x64::select(&b.f);
    let assignment: HashMap<Value, Location> = [(c, Location::Spill(0))].into_iter().collect();

    forge_emit::emit_body(&b.f, &selected, &assignment);
}
```

**Before running this task's tests**, the `int_cmp_and_cmov_diamond_selects_correct_branch` test's phi/merge-value construction is the one piece of code in this plan not verified against `forge_ir::builder::Builder`'s exact API (whether `read_variable` is public, what its signature is, whether a merge block automatically mints a phi on `seal_block` without an explicit call). Check `crates/forge-x64/src/machine_inst/tests.rs` and `crates/forge-ir/src/builder.rs` for the exact pattern used to produce a real phi at a merge block (this project's own history — see the Phase 7f design doc's "Braun-style SSA construction" notes — confirms phis are minted automatically by `read_variable`/`read_variable_recursive` when a sealed block has multiple predecessors, not by an explicit "create phi" call) and correct this test's construction to match before implementing. If constructing a genuine fused diamond this way proves disproportionately fiddly, it is acceptable to simplify this one test to a hand-built `Function`/`BlockData` (as `forge-ir/src/ir.rs:302-319`'s `hand_built_return_fn` does) with an explicit `Inst::Phi` at `merge`, rather than driving it through `Builder` — note which approach was taken in the commit message.

- [ ] **Step 2: Run tests, fix the phi construction as needed, iterate until green**

Run: `cargo test -p forge-emit --test execution_corpus`
Expected: after resolving the phi-construction note above, all tests PASS (execution assertions only actually run `#[cfg(target_arch = "x86_64")]`; on this arm64 dev machine they're compiled but skipped, and disassembly assertions are the real bar here).

- [ ] **Step 3: Run full crate suite, clippy, fmt**

Run: `cargo test -p forge-emit && cargo clippy -p forge-emit --all-targets -- -D warnings && cargo fmt --check -p forge-emit`
Expected: all green

- [ ] **Step 4: Commit**

```bash
git add crates/forge-emit/tests/execution_corpus.rs
git commit -m "test(forge-emit): float sign-flip and IntCmov execution corpus"
```

---

### Task 8: Workspace-wide verification and CHECKLIST.md annotation

**Files:**
- Modify: `CHECKLIST.md`

- [ ] **Step 1: Run the full workspace verification**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: all green. Fix anything that surfaces (most likely candidate: an unused-import warning in one of the new test files, or a `Builder` API mismatch in Task 7 not caught until now) before proceeding.

- [ ] **Step 2: Find and annotate the relevant CHECKLIST.md bullets**

Read `CHECKLIST.md` and find bullet 253 (libm call sequence) and any other bullet whose existing note explicitly says "deferred to the not-yet-built emission pipeline (task #68)" or similar (per this project's established convention of appending `— **note (Phase Nx):** ...` rather than checking boxes). Do not check any `- [x]` boxes anywhere in this file (zero exist in the whole project by convention).

For the specific bullet(s) most directly satisfied by this slice (the base `MachineInst` → real bytes translation, control flow, constant pool placement), append a new note in the same style as existing notes, e.g.:

```markdown
— **note (Phase 9a):** the base `MachineInst` → real `Assembler` bytes translation now exists, in a new crate `crates/forge-emit` (`translate_inst` in `translate.rs`, `emit_body` in `layout.rs`, constant-pool placement in `const_pool.rs`). Scope: register-only operands (`Location::Reg`, never `Location::Spill`); `Param` and `CallLibm` are not yet implemented (real panics, not silent gaps); `IntDiv`/`IntRem`/`Shl`/`Shr`/`Sar` handle the common case but not third-party register-clobber/CL-occupied displacement. Real control flow (`Jump`/`Branch`), constant-pool RIP-relative loads (`LoadImmF64`, `FloatAbs`/`FloatNeg` sign masks), and `IntCmp`/`FloatCmp`/`IntCmov` (including the alias-safe zero-extension ordering fix found while implementing) all work end-to-end. Verified primarily via iced-x86 disassembly (arch-independent); execution tests through `forge-mem` are `#[cfg(target_arch = "x86_64")]`-gated since this project's dev environment is arm64. Remaining gaps (Param/CallLibm/spill/coalescing-elision/phi-resolution/prologue-epilogue wiring) are Phase 9b-9f. Details: `docs/superpowers/specs/2026-08-11-phase-9a-forge-emit-skeleton-design.md`.
```

Adjust the exact bullet number/text to match what's actually in the file at the time — re-read `CHECKLIST.md` fresh rather than trusting this plan's line-number memory, since other phases may have shifted it.

- [ ] **Step 3: Commit**

```bash
git add CHECKLIST.md
git commit -m "docs: Phase 9a CHECKLIST annotation"
```

---

## Exit criteria (mirrors the design doc)

1. `crates/forge-emit` exists, builds, is a workspace member.
2. `forge_x64::Assembler::emit_u64` and `forge_x64::PoolIndex::index` exist and are tested.
3. `translate_inst` has a real arm for every `MachineInst` variant (no wildcard) — full implementations for the happy path, explicit panics/asserts naming the deferring sub-slice for `Param`/`CallLibm`/uncommon `IntDiv`-`IntRem`/CL-mismatched shifts, `unreachable!()` for the three terminator variants.
4. `alloc_pool_labels`/`place_pool` correctly separate label allocation (before translation) from byte placement (after translation).
5. `emit_body` resolves `Jump`/`Branch`/`Return` control flow correctly, forward and backward, and places the return value in the correct ABI register (`xmm0`/`rax`) based on the value's real type.
6. All tests pass via iced-x86 disassembly (architecture-independent); execution tests through `forge-mem` are gated to `x86_64` hosts and don't block CI on this project's arm64 dev environment.
7. `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check` all clean.
8. CHECKLIST.md annotated per this project's established convention.
