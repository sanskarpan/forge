# CLAUDE CODE PROMPT — `forge`: A JIT Compiler for Expression Evaluation

## Project Mission

Build a real JIT compiler from scratch:

- **Backend: Rust** — Pratt parser, SSA IR with φ-nodes, a full optimizer (folding, algebraic simplification, strength reduction with magic-number division, GVN/CSE, DCE, reassociation, FMA contraction), linear-scan register allocation with spilling, **hand-written x86-64 and AArch64 encoders** (REX/ModRM/SIB/VEX/EVEX by hand), W^X executable memory across Linux/macOS-ARM/Windows, SIMD vectorization with runtime CPU feature detection, and a tiered runtime
- **Frontend: React + TypeScript + Vite + CodeMirror 6 + Tailwind + shadcn/ui + D3 + Recharts** — a workbench showing IR, CFG, register lifetimes, assembly, and the actual encoded bytes annotated field by field

**Read `jit-SPEC.md` and `jit-CHECKLIST.md` before writing any code.**

### Four rules that override everything

1. **Write the interpreter first (Phase 3), before any code generation.** It is the correctness oracle. Every subsequent phase is validated by "does the JIT agree with the interpreter, bit for bit."

2. **Every encoder function gets a disassembler round-trip test in the same commit.** Assemble → disassemble with `iced-x86` → compare to the intended mnemonic. Wrong encodings do not crash; they produce plausible-looking instructions that corrupt a *different* register, and you will find out three hours later.

3. **Do the executable-memory spike on day one.** Before the parser, before the IR: `mmap` → emit `48 89 F8 C3` → `mprotect` → `transmute` → call it and get your argument back. If W^X doesn't work on your platform (Apple Silicon entitlements, hardened runtime, SELinux), nothing else in the project matters.

4. **`iced-x86` is a test oracle only.** We encode by hand. Encoding x86-64 — ModRM, SIB, REX, VEX — *is* the project.

---

## Phase 0 — The Day One Spike

```bash
cargo new --lib forge && cd forge
cargo add libc nix region raw-cpuid smallvec bitvec rustc-hash thiserror anyhow
cargo add --dev iced-x86 capstone criterion proptest
```

```rust
// Do this FIRST. If it doesn't run, stop and fix your platform setup.
fn main() {
    unsafe {
        let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;

        // NOTE: RW only — never map RWX. We flip to RX below.
        let mem = libc::mmap(std::ptr::null_mut(), page,
                             libc::PROT_READ | libc::PROT_WRITE,
                             libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0) as *mut u8;
        assert_ne!(mem as isize, -1, "mmap failed: {}", std::io::Error::last_os_error());

        // mov rax, rdi   (48 89 F8)
        // ret            (C3)
        let code = [0x48u8, 0x89, 0xF8, 0xC3];
        std::ptr::copy_nonoverlapping(code.as_ptr(), mem, code.len());

        assert_eq!(libc::mprotect(mem as _, page, libc::PROT_READ | libc::PROT_EXEC), 0);

        let f: extern "C" fn(i64) -> i64 = std::mem::transmute(mem);
        assert_eq!(f(42), 42);
        println!("JIT works: f(42) = {}", f(42));
    }
}
```

On **Apple Silicon** this will fail without `MAP_JIT` and entitlements — see Phase 5. Find that out now, not in week three.

---

## Phase 3 — The Interpreter (the oracle)

Build this before any encoder. Everything downstream is validated against it.

```rust
// crates/forge-ir/src/interp.rs

/// Runtime value carried through the interpreter, and later into JIT calling
/// conventions. `Function.params: Vec<(Symbol, Ty)>` allows real,
/// independently-typed f64 / i64 / bool parameters, so a single `f64` slot
/// can't represent every argument or result — hence the enum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RtValue {
    F64(f64),
    I64(i64),
    Bool(bool),
}

impl RtValue {
    fn as_f64(self)  -> f64  { match self { RtValue::F64(x)  => x, _ => panic!("expected f64")  } }
    fn as_i64(self)  -> i64  { match self { RtValue::I64(x)  => x, _ => panic!("expected i64")  } }
    fn as_bool(self) -> bool { match self { RtValue::Bool(x) => x, _ => panic!("expected bool") } }
}

/// The correctness oracle for the entire project.
///
/// This must implement IEEE-754 semantics EXACTLY — NaN propagation, signed
/// zeros, infinities, subnormals. No shortcuts, no "close enough". Every
/// differential test compares the JIT to this, bit for bit, so any sloppiness
/// here becomes a false failure (or worse, masks a real JIT bug). Integer ops
/// use Rust's wrapping arithmetic throughout, matching the JIT's raw machine
/// `add`/`sub`/`imul`, which wrap on overflow with no trap.
pub fn interpret(f: &Function, args: &[RtValue]) -> RtValue {
    let mut vals: Vec<Option<RtValue>> = vec![None; f.insts.len()];
    let mut block = f.entry;
    let mut prev_block: Option<Block> = None;

    loop {
        for &v in &f.blocks[block].insts {
            let result = match &f.insts[v.0 as usize] {
                Inst::ConstF64(bits)      => RtValue::F64(f64::from_bits(*bits)),
                Inst::ConstI64(n)         => RtValue::I64(*n),
                Inst::ConstBool(b)        => RtValue::Bool(*b),
                Inst::Param { index, .. } => args[*index as usize],

                // Arithmetic dispatches on the operands' own runtime type
                // rather than a separate `is_int` flag threaded in from the
                // type checker — exactly one arm ever matches, because the
                // type checker already guaranteed matching operand types.
                Inst::Add(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::F64(x), RtValue::F64(y)) => RtValue::F64(x + y),
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x.wrapping_add(y)),
                    _ => unreachable!("type checker guarantees matching operand types"),
                },
                Inst::Mul(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::F64(x), RtValue::F64(y)) => RtValue::F64(x * y),
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x.wrapping_mul(y)),
                    _ => unreachable!(),
                },
                Inst::Div(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::F64(x), RtValue::F64(y)) => RtValue::F64(x / y),
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x.wrapping_div(y)),
                    _ => unreachable!(),
                },

                // f64::sqrt lowers to the same hardware instruction the JIT
                // will emit (sqrtsd), so results are bit-identical by
                // construction rather than by luck.
                Inst::Sqrt(a) => RtValue::F64(get(&vals, *a).as_f64().sqrt()),

                // CAREFUL: f64::min/max have DIFFERENT NaN semantics from
                // x86's minsd/maxsd. Rust's f64::min returns the non-NaN
                // operand; minsd returns its SECOND operand if either is NaN.
                // We pick Rust's semantics here, which means codegen must emit
                // an extra compare rather than a bare minsd. Choosing the
                // other way is also fine — but the two MUST agree, and this is
                // the exact spot where they silently diverge.
                Inst::Min(a, b) => RtValue::F64(get(&vals, *a).as_f64().min(get(&vals, *b).as_f64())),
                Inst::Max(a, b) => RtValue::F64(get(&vals, *a).as_f64().max(get(&vals, *b).as_f64())),

                Inst::Fma { a, b, c } => RtValue::F64(
                    get(&vals, *a).as_f64().mul_add(get(&vals, *b).as_f64(), get(&vals, *c).as_f64())),

                // Bitwise/shift — i64 only, enforced by the type checker
                // (SPEC §3 "Operators & precedence").
                Inst::Shl(a, b) => RtValue::I64(get(&vals, *a).as_i64().wrapping_shl(get(&vals, *b).as_i64() as u32)),
                Inst::And(a, b) => RtValue::I64(get(&vals, *a).as_i64() & get(&vals, *b).as_i64()),

                Inst::Cmp { op, lhs, rhs } => {
                    let (l, r) = (get(&vals, *lhs), get(&vals, *rhs));
                    RtValue::Bool(match (op, l, r) {
                        // NaN comparisons fall through to `false` for every
                        // op here because `<`/`==` on f64 already do that —
                        // no special-casing needed.
                        (CmpOp::Lt, RtValue::F64(x), RtValue::F64(y)) => x < y,
                        (CmpOp::Lt, RtValue::I64(x), RtValue::I64(y)) => x < y,
                        (CmpOp::Eq, RtValue::F64(x), RtValue::F64(y)) => x == y,
                        (CmpOp::Eq, RtValue::I64(x), RtValue::I64(y)) => x == y,
                        (CmpOp::Eq, RtValue::Bool(x), RtValue::Bool(y)) => x == y,
                        // … Le, Gt, Ge, Ne follow the same shape.
                        _ => unreachable!("type checker guarantees comparable operand types"),
                    })
                }

                Inst::Phi { incoming } => {
                    let from = prev_block.expect("phi in entry block");
                    let (_, val) = incoming.iter().find(|(b, _)| *b == from)
                        .expect("phi missing operand for predecessor");
                    get(&vals, *val)
                }
                // … every variant. A missing arm must be a COMPILE error,
                // which is why Inst is an enum and this is an exhaustive match.
            };
            vals[v.0 as usize] = Some(result);
        }

        match &f.blocks[block].term {
            Terminator::Return(v) => return get(&vals, *v),
            Terminator::Jump(b)   => { prev_block = Some(block); block = *b; }
            Terminator::Branch { cond, then_, else_ } => {
                prev_block = Some(block);
                // Cond is always bool-typed (produced by Cmp or a bool
                // param) — no float truthiness coercion to get wrong.
                block = if get(&vals, *cond).as_bool() { *then_ } else { *else_ };
            }
        }
    }
}
```

---

## Phase 5 — Executable Memory

### The W^X rule

```rust
// crates/forge-mem/src/lib.rs

/// NEVER map RWX.
///
/// Every JIT tutorial maps PROT_READ|PROT_WRITE|PROT_EXEC because it's one
/// line shorter. It also means any bug anywhere in the process can write into
/// a page that is about to be executed — precisely the primitive an attacker
/// needs. Modern OSes are moving to reject RWX outright (macOS already does on
/// Apple Silicon), so the "shorter" version is also the less portable one.
///
/// Allocate RW → write → flip to RX. Two extra lines, no downside.
#[cfg(all(unix, not(all(target_os = "macos", target_arch = "aarch64"))))]
impl ExecutableBuffer {
    pub fn new(size: usize) -> io::Result<Self> {
        // SAFETY: sysconf(_SC_PAGESIZE) has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let len = (size + page - 1) & !(page - 1);

        // SAFETY: null hint, non-zero page-multiple length, valid flag
        // combination. Returns MAP_FAILED on error, which we check.
        let ptr = unsafe {
            libc::mmap(ptr::null_mut(), len,
                       libc::PROT_READ | libc::PROT_WRITE,   // deliberately NOT EXEC
                       libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0)
        };
        if ptr == libc::MAP_FAILED { return Err(io::Error::last_os_error()); }
        Ok(Self { ptr: ptr as *mut u8, len, state: ProtState::Writable })
    }

    pub fn make_executable(&mut self) -> io::Result<()> {
        // SAFETY: ptr/len came from a successful mmap and are page-aligned.
        let rc = unsafe {
            libc::mprotect(self.ptr as _, self.len, libc::PROT_READ | libc::PROT_EXEC)
        };
        if rc != 0 { return Err(io::Error::last_os_error()); }
        self.state = ProtState::Executable;
        Ok(())
    }
}
```

### Apple Silicon is completely different

```rust
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod apple {
    extern "C" {
        fn pthread_jit_write_protect_np(enabled: libc::c_int);
        fn sys_icache_invalidate(start: *mut libc::c_void, len: libc::size_t);
    }

    /// THREE things differ on Apple Silicon, and every JIT port trips on all
    /// three:
    ///
    /// 1. MAP_JIT is REQUIRED, and so is the com.apple.security.cs.allow-jit
    ///    entitlement on a signed binary. Without both, making the page
    ///    executable simply fails.
    ///
    /// 2. You must NOT use mprotect on MAP_JIT pages — it returns EACCES,
    ///    which is confusing to debug because the mmap succeeded. Instead
    ///    toggle per-THREAD write protection with pthread_jit_write_protect_np().
    ///    This is hardware-backed (APRR) and essentially free.
    ///
    /// 3. sys_icache_invalidate() is MANDATORY. On Apple Silicon the
    ///    instruction cache is NOT coherent with the data cache, so freshly
    ///    written bytes may not be visible to the fetch unit — the CPU
    ///    executes whatever was in that cache line before.
    ///
    ///    The symptom is INTERMITTENT, UNREPRODUCIBLE wrong behavior: passes
    ///    every test, fails once in ten thousand runs in production. The worst
    ///    possible bug class, and it is one missing function call.
    impl ExecutableBuffer {
        pub fn new(size: usize) -> io::Result<Self> {
            let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
            let len = (size + page - 1) & !(page - 1);
            // SAFETY: standard mmap contract; MAP_JIT requires the entitlement.
            let ptr = unsafe {
                libc::mmap(ptr::null_mut(), len,
                           libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                           libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_JIT,
                           -1, 0)
            };
            if ptr == libc::MAP_FAILED {
                return Err(io::Error::new(io::ErrorKind::Other,
                    "mmap MAP_JIT failed — is com.apple.security.cs.allow-jit \
                     present in the entitlements, and is the binary signed?"));
            }
            Ok(Self { ptr: ptr as *mut u8, len, state: ProtState::Executable })
        }

        /// The ONLY way to write into the buffer. Making this the sole entry
        /// point means the protect/invalidate dance cannot be forgotten.
        pub fn write<F: FnOnce(&mut [u8])>(&mut self, f: F) {
            // SAFETY: ptr/len from a successful MAP_JIT mmap; the closure only
            // sees a correctly-bounded slice; protection is restored on exit.
            unsafe {
                pthread_jit_write_protect_np(0);                 // this thread may write
                let slice = slice::from_raw_parts_mut(self.ptr, self.len);
                f(slice);
                pthread_jit_write_protect_np(1);                 // back to executable
                sys_icache_invalidate(self.ptr as _, self.len);  // NON-OPTIONAL
            }
        }
    }
}
```

`entitlements.plist`:
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>com.apple.security.cs.allow-jit</key><true/>
</dict></plist>
```
```makefile
codesign: 
	codesign --entitlements entitlements.plist -s - target/debug/forge
```

### Calling into generated code

```rust
impl CompiledExpr {
    /// The single most dangerous operation in the project, isolated to one
    /// place behind a checked API.
    ///
    /// SAFETY: three preconditions, all enforced by construction:
    ///   1. The buffer is in Executable state (asserted).
    ///   2. It contains a complete function with a valid prologue and epilogue
    ///      (guaranteed — the compiler always emits both, and the IR verifier
    ///      requires a terminator).
    ///   3. The arity matches the compiled signature (recorded at compile time
    ///      and asserted here).
    pub fn call2(&self, x: f64, y: f64) -> f64 {
        assert_eq!(self.arity, 2, "arity mismatch: compiled for {}", self.arity);
        debug_assert_eq!(self.buf.state, ProtState::Executable);
        let f: unsafe extern "C" fn(f64, f64) -> f64 =
            unsafe { mem::transmute(self.buf.as_ptr()) };
        unsafe { f(x, y) }
    }
}
```

---

## Phase 6 — x86-64 Encoder

### REX — the #1 source of silent JIT bugs

```rust
// crates/forge-x64/src/encode.rs

impl Assembler {
    /// The REX prefix is where most JIT bugs live, because omitting it
    /// produces a VALID instruction that does the wrong thing.
    ///
    /// Three traps:
    ///
    ///   1. Without REX.W the operation is 32-BIT, and 32-bit x86-64 ops ZERO
    ///      the upper 32 bits of the destination. `add eax, 1` wipes the top
    ///      half of rax. Your pointer or f64 bit pattern becomes garbage.
    ///
    ///   2. Without REX.R/X/B you address rax-rdi instead of r8-r15. The
    ///      instruction encodes fine, executes fine, and corrupts a register
    ///      you weren't thinking about — usually one holding a live value.
    ///
    ///   3. The presence of ANY REX prefix changes byte-register encoding:
    ///      spl/bpl/sil/dil replace ah/ch/dh/bh. Silently different registers.
    fn rex(&mut self, w: bool, reg: u8, index: u8, rm: u8) {
        let byte = 0x40
            | ((w as u8) << 3)              // REX.W — 64-bit operand size
            | (((reg   >> 3) & 1) << 2)     // REX.R — extends ModRM.reg
            | (((index >> 3) & 1) << 1)     // REX.X — extends SIB.index
            |  ((rm    >> 3) & 1);          // REX.B — extends ModRM.rm / SIB.base
        if byte != 0x40 { self.code.push(byte); }
    }

    fn modrm_reg(&mut self, reg: u8, rm: u8) {
        self.code.push((0b11 << 6) | ((reg & 7) << 3) | (rm & 7));
    }
```

### ModRM — three mandatory special cases

```rust
    /// Memory operand encoding. THREE cases MUST be special-cased or you emit
    /// something completely different from what you meant:
    ///
    ///   • base == RSP (4): ModRM.rm = 100 is an ESCAPE CODE meaning "a SIB
    ///     byte follows". You therefore CANNOT encode [rsp] with ModRM alone;
    ///     you must emit a SIB byte with index=100 ("no index").
    ///
    ///   • base == RBP (5) with disp == 0: mod=00, rm=101 means RIP-RELATIVE
    ///     addressing, not [rbp]. You must force mod=01 with an explicit disp8
    ///     of zero. Skip this and your [rbp] load reads from somewhere
    ///     relative to the instruction pointer — a wild read that usually
    ///     returns plausible garbage rather than faulting.
    ///
    ///   • R12 and R13 hit exactly these two cases via REX.B, because their
    ///     low three bits are 100 and 101. It is very easy to handle rsp/rbp
    ///     and forget the extended twins — and since r12/r13 are prime
    ///     allocation candidates, the bug only appears under register
    ///     pressure, i.e. on complex expressions, i.e. in production.
    fn modrm_mem(&mut self, reg: u8, base: u8, disp: i32) {
        let base_low = base & 7;

        if base_low == 0b100 {
            // RSP or R12 → SIB byte required
            let mode = disp_mode(disp);
            self.code.push((mode << 6) | ((reg & 7) << 3) | 0b100);
            self.code.push(0b00_100_100);   // scale=1, index=none(100), base=rsp/r12
            self.emit_disp(mode, disp);
        } else if base_low == 0b101 && disp == 0 {
            // RBP or R13 with zero displacement → must use the disp8 form
            self.code.push((0b01 << 6) | ((reg & 7) << 3) | base_low);
            self.code.push(0);              // explicit zero disp8
        } else {
            let mode = disp_mode(disp);
            self.code.push((mode << 6) | ((reg & 7) << 3) | base_low);
            self.emit_disp(mode, disp);
        }
    }

    /// vaddsd xmm_dst, xmm_src1, xmm_src2   (VEX.LIG.F2.0F.WIG 58 /r)
    ///
    /// VEX's third operand is what makes AVX non-destructive: `c = a + b` is
    /// ONE instruction instead of `movapd c,a; addsd c,b`. That halves the
    /// instruction count on expression-heavy code and removes a dependency.
    pub fn vaddsd(&mut self, dst: XmmReg, src1: XmmReg, src2: XmmReg) {
        self.vex(dst as u8, 0, src2 as u8,
                 0b00001,      // mmmmm: 0F escape
                 false,        // W
                 src1 as u8,   // vvvv ← the extra operand
                 false,        // L: 128-bit
                 0b11);        // pp: F2 prefix
        self.code.push(0x58);
        self.modrm_reg(dst as u8, src2 as u8);
    }

    /// THE VEX TRAP: the vvvv field is stored INVERTED.
    ///
    /// You write `!reg & 0xF`, not `reg`. Forget it and you address a
    /// completely different register — xmm0 becomes xmm15. The result
    /// disassembles as a perfectly valid instruction, so nothing complains
    /// and the only symptom is a wrong number.
    fn vex(&mut self, r: u8, x: u8, b: u8, mmmmm: u8, w: bool, vvvv: u8, l: bool, pp: u8) {
        let two_byte_ok = x == 0 && (b >> 3) == 0 && !w && mmmmm == 0b00001;
        if two_byte_ok {
            self.code.push(0xC5);
            self.code.push((((!(r >> 3)) & 1) << 7)
                           | ((!vvvv & 0xF) << 3)      // ← INVERTED
                           | ((l as u8) << 2) | pp);
            return;
        }
        self.code.push(0xC4);
        self.code.push((((!(r >> 3)) & 1) << 7)
                       | (((!(x >> 3)) & 1) << 6)
                       | (((!(b >> 3)) & 1) << 5) | mmmmm);
        self.code.push(((w as u8) << 7)
                       | ((!vvvv & 0xF) << 3)          // ← INVERTED
                       | ((l as u8) << 2) | pp);
    }
}
```

### The round-trip test — write one for every emitter

```rust
// crates/forge-x64/tests/encoding.rs

/// This test class catches the entire family of "plausible but wrong"
/// encodings. A missing REX or a non-inverted vvvv produces a VALID
/// instruction touching a different register, so the only way to find it is to
/// disassemble what you emitted and compare the text.
///
/// iced-x86 appears ONLY here, as an oracle. Never in a codegen path.
fn assert_encodes(f: impl FnOnce(&mut Assembler), expected: &str) {
    let mut a = Assembler::new();
    f(&mut a);
    let got = disassemble_one(&a.code);
    assert_eq!(got, expected, "\n  emitted bytes: {:02X?}", a.code);
}

#[test]
fn extended_registers_need_rex() {
    assert_encodes(|a| a.mov_reg_reg(RAX, RCX), "mov rax,rcx");
    // Without REX.R/B these silently become rax/rcx.
    assert_encodes(|a| a.mov_reg_reg(R12, RCX), "mov r12,rcx");
    assert_encodes(|a| a.mov_reg_reg(RAX, R13), "mov rax,r13");
    assert_encodes(|a| a.mov_reg_reg(R14, R15), "mov r14,r15");
}

#[test]
fn modrm_special_cases() {
    // RSP needs a SIB byte — cannot be encoded with ModRM alone
    assert_encodes(|a| a.mov_reg_mem(RAX, RSP, 0),  "mov rax,[rsp]");
    // RBP with disp=0 must use disp8, or it becomes RIP-relative
    assert_encodes(|a| a.mov_reg_mem(RAX, RBP, 0),  "mov rax,[rbp]");
    // R12/R13 hit the same cases via REX.B — the ones everyone forgets
    assert_encodes(|a| a.mov_reg_mem(RAX, R12, 0),  "mov rax,[r12]");
    assert_encodes(|a| a.mov_reg_mem(RAX, R13, 0),  "mov rax,[r13]");
    assert_encodes(|a| a.mov_reg_mem(RAX, RSP, 16), "mov rax,[rsp+10h]");
    assert_encodes(|a| a.mov_reg_mem(RAX, R13, 16), "mov rax,[r13+10h]");
}

#[test]
fn vex_vvvv_is_inverted() {
    assert_encodes(|a| a.vaddsd(XMM0,  XMM1,  XMM2),  "vaddsd xmm0,xmm1,xmm2");
    // High register numbers are where a non-inverted vvvv shows up.
    assert_encodes(|a| a.vaddsd(XMM15, XMM14, XMM13), "vaddsd xmm15,xmm14,xmm13");
}
```

---

## Phase 7 — Prologue, Stack Alignment, and libm Calls

```rust
// crates/forge-x64/src/frame.rs

impl Compiler {
    /// STACK ALIGNMENT IS NOT OPTIONAL.
    ///
    /// System V requires rsp to be 16-byte aligned AT THE POINT OF EVERY
    /// `call`. On entry to our function, `call` has already pushed an 8-byte
    /// return address, so rsp ≡ 8 (mod 16).
    ///
    ///   push rbp     → rsp ≡ 0 (mod 16)   ✓
    ///   sub rsp, N   → still aligned iff N ≡ 0 (mod 16)
    ///
    /// Get this wrong and libm faults on its first `movaps`, which requires
    /// 16-byte alignment. The crash is inside libm with no useful backtrace,
    /// and it only occurs for expressions that call libm — so `sqrt(x)` works
    /// and `sin(x)` segfaults, which is baffling until you know why.
    fn emit_prologue(&mut self, spill_bytes: u32, used_callee_saved: &[PhysReg]) {
        self.asm.push_reg(RBP);
        self.asm.mov_reg_reg(RBP, RSP);

        // Save only the callee-saved registers we actually allocated.
        for &r in used_callee_saved { self.asm.push_reg(r); }

        let pushed = 8 * used_callee_saved.len() as u32;   // after `push rbp`
        let mut frame = spill_bytes;
        if self.calls_libm && cfg!(windows) {
            frame += 32;      // Win64 SHADOW SPACE — caller-allocated, mandatory
        }
        let pad = (16 - ((pushed + frame) % 16)) % 16;
        let frame = frame + pad;

        if frame > 0 { self.asm.sub_reg_imm32(RSP, frame as i32); }
        self.frame_size = frame;
        debug_assert_eq!((pushed + frame) % 16, 0, "frame misaligned");
    }

    /// Calling libm.
    ///
    /// The critical fact on System V: ALL XMM REGISTERS ARE CALLER-SAVED.
    /// A single `sin()` call clobbers xmm0-xmm15 entirely.
    ///
    /// This is why `sin(x) + cos(y)` requires spilling and `sqrt(x) + sqrt(y)`
    /// does not: sqrt is one instruction, sin is a call that destroys the whole
    /// float register file. Swapping one intrinsic for another doesn't just add
    /// a call — it changes the entire register allocation. Worth surfacing in
    /// the workbench, because it's genuinely surprising.
    fn emit_libm_call(&mut self, func: LibFunc, arg: PhysReg, live: &RegSet) {
        let to_spill: Vec<_> = live.iter().filter(|r| r.is_caller_saved()).collect();
        for (i, &r) in to_spill.iter().enumerate() {
            self.asm.movsd_mem_xmm(RSP, (i * 8) as i32, r);
        }

        if arg != XMM0 { self.asm.movapd(XMM0, arg); }

        // Load the absolute address and call through a register. A rel32 call
        // cannot reach libm from a JIT page — the mapping may be arbitrarily
        // far from libc in the address space, and ±2GB is not guaranteed.
        self.asm.mov_reg_imm64(RAX, func.address() as i64);
        self.asm.call_reg(RAX);

        for (i, &r) in to_spill.iter().enumerate() {
            self.asm.movsd_xmm_mem(r, RSP, (i * 8) as i32);
        }
    }
}
```

---

## Phase 8 — Linear Scan Register Allocation

```rust
// crates/forge-regalloc/src/linear_scan.rs

impl LinearScan {
    /// Poletto & Sarkar (1999) with the Mössenböck & Pfeiffer SSA refinements.
    ///
    /// Graph coloring produces better allocations. Linear scan is still the
    /// right choice for a JIT, because compile time is on the critical path:
    /// linear scan is near-linear where coloring is quadratic, and the
    /// code-quality gap is small. This is exactly why HotSpot's client compiler
    /// switched from coloring to linear scan.
    pub fn run(&mut self) -> Allocation {
        self.intervals.sort_by_key(|i| i.start);

        for i in 0..self.intervals.len() {
            self.expire_old_intervals(self.intervals[i].start);

            // ABI-fixed registers are non-negotiable: argument positions, and
            // idiv's implicit rax/rdx. Evict whoever holds them.
            if let Some(phys) = self.intervals[i].fixed {
                self.evict_and_assign(i, phys);
                continue;
            }

            match self.pick_register(i) {
                Some(reg) => self.assign(i, reg),
                None      => self.spill_at_interval(i),
            }
        }
        self.finish()
    }

    /// `active` is kept sorted by END point. That invariant is what makes
    /// expiry a cheap prefix scan; sorting by start (the intuitive choice)
    /// makes this O(n) per interval and the whole allocator quadratic.
    fn expire_old_intervals(&mut self, current_start: u32) {
        while let Some(&j) = self.active.first() {
            if self.intervals[j].end > current_start { break; }
            self.active.remove(0);
            self.free_regs.insert(self.assigned_reg(j));
        }
    }

    /// THE SPILL HEURISTIC.
    ///
    /// The textbook spills the active interval with the FURTHEST endpoint, on
    /// the theory that it blocks a register longest. But this is only a
    /// heuristic — you may spill ANY active interval — and on expression trees
    /// it chooses badly surprisingly often.
    ///
    /// Weighting by use density (uses/length) measurably beats it: a value
    /// used four times in a tight window must not be spilled just because its
    /// interval happens to extend furthest. We combine both signals.
    fn spill_at_interval(&mut self, i: usize) {
        let class = self.intervals[i].reg_class;
        let victim = *self.active.iter()
            .filter(|&&a| self.intervals[a].reg_class == class)
            .max_by(|&&a, &&b| {
                let score = |k: usize| {
                    let iv = &self.intervals[k];
                    iv.end as f32 / iv.spill_weight.max(0.01)
                };
                score(a).partial_cmp(&score(b)).unwrap()
            })
            .expect("no spillable interval — class exhausted by fixed registers");

        if self.intervals[victim].end > self.intervals[i].end {
            let reg = self.assigned_reg(victim);
            self.assign(i, reg);
            self.spill(victim);
        } else {
            self.spill(i);
        }
    }
}

/// An INDEPENDENT verifier, deliberately written without reference to the
/// allocator's internals so it cannot share a bug with it.
///
/// Register allocation bugs are catastrophic and silent: two live values in
/// one register means one gets the wrong answer, data-dependently. This runs
/// on every compilation in debug builds.
pub fn verify_allocation(intervals: &[Interval], alloc: &Allocation) -> Result<(), String> {
    for (i, a) in intervals.iter().enumerate() {
        for b in &intervals[i + 1..] {
            if !(a.start < b.end && b.start < a.end) { continue; }   // no overlap
            if let (Location::Reg(ra), Location::Reg(rb)) =
                (alloc.of(a.value), alloc.of(b.value))
            {
                if ra == rb {
                    return Err(format!(
                        "overlapping values {:?} [{},{}) and {:?} [{},{}) \
                         both assigned {:?}",
                        a.value, a.start, a.end, b.value, b.start, b.end, ra));
                }
            }
        }
    }
    Ok(())
}
```

---

## Phase 4 — The Optimizer's Floating-Point Trap

```rust
// crates/forge-opt/src/simplify.rs

/// This table is where "the optimizer broke my numerics" bugs come from.
/// Every rule is annotated with when it is legal.
///
/// The float cases are counterintuitive and worth internalizing:
///   • x * 0.0  is NOT 0.0 when x is NaN or ±Inf — it is NaN.
///   • x - x    is NOT 0.0 when x is NaN or ±Inf — it is NaN.
///   • x / x    is NOT 1.0 when x is 0.0 or NaN  — it is NaN.
///
/// Each is a legal integer optimization and an ILLEGAL float optimization. A
/// compiler that gets it wrong produces wrong answers only on edge-case
/// inputs — the hardest possible class of bug to notice.
pub fn simplify(f: &mut Function, fast_math: bool) -> bool {
    let mut changed = false;
    for v in f.all_values() {
        let is_int = f.types[v.0 as usize] == Ty::I64;

        let new = match &f.insts[v.0 as usize] {
            // ── Always valid, both domains ───────────────────────────────
            Inst::Add(a, b) if f.is_zero(*b) => Some(Inst::Copy(*a)),   // x + 0 → x
            Inst::Sub(a, b) if f.is_zero(*b) => Some(Inst::Copy(*a)),   // x - 0 → x
            Inst::Mul(a, b) if f.is_one(*b)  => Some(Inst::Copy(*a)),   // x * 1 → x
            Inst::Div(a, b) if f.is_one(*b)  => Some(Inst::Copy(*a)),   // x / 1 → x
            Inst::Neg(a) if f.is_neg(*a)     => Some(Inst::Copy(f.inner_of_neg(*a))),

            // ── INTEGER ONLY — NaN/Inf make all of these wrong for f64 ────
            Inst::Mul(_, b) if is_int && f.is_zero(*b) => Some(Inst::ConstI64(0)),
            Inst::Sub(a, b) if is_int && a == b        => Some(Inst::ConstI64(0)),
            Inst::Div(a, b) if is_int && a == b        => Some(Inst::ConstI64(1)),
            Inst::And(a, b) if is_int && a == b        => Some(Inst::Copy(*a)),
            Inst::Xor(a, b) if is_int && a == b        => Some(Inst::ConstI64(0)),

            // ── FAST-MATH ONLY — these change results in the last ulp ─────
            Inst::Sqrt(a) if fast_math && f.is_square(*a) =>
                Some(Inst::Abs(f.base_of(*a))),

            _ => None,
        };

        if let Some(inst) = new {
            f.insts[v.0 as usize] = inst;
            changed = true;
        }
    }
    changed
}
```

### Magic-number division

```rust
// crates/forge-opt/src/strength.rs

/// The single most dramatic optimization to demonstrate.
///
/// `idiv` has 20-40 cycle latency on modern x86 and is NOT pipelined — the
/// whole divider stalls. The magic-number sequence is 3 instructions, ~5
/// cycles, fully pipelined. A 5-10× win on one instruction.
///
/// Granlund & Montgomery, "Division by Invariant Integers using
/// Multiplication" (PLDI '94).
pub fn magic_signed(d: i64) -> MagicNumber {
    assert!(d != 0 && d != 1 && d != -1);
    let ad = d.unsigned_abs();
    let t = (1u64 << 63) + if d > 0 { 0 } else { 1 };
    let anc = t - 1 - t % ad;

    let mut p = 63u32;
    let (mut q1, mut r1) = ((1u64 << 63) / anc, (1u64 << 63) % anc);
    let (mut q2, mut r2) = ((1u64 << 63) / ad,  (1u64 << 63) % ad);

    loop {
        p += 1;
        q1 = 2 * q1; r1 = 2 * r1;
        if r1 >= anc { q1 += 1; r1 -= anc; }
        q2 = 2 * q2; r2 = 2 * r2;
        if r2 >= ad { q2 += 1; r2 -= ad; }
        let delta = ad - r2;
        if !(q1 < delta || (q1 == delta && r1 == 0)) { break; }
    }

    let mut m = (q2 + 1) as i64;
    if d < 0 { m = -m; }
    MagicNumber { multiplier: m, shift: p - 64 }
}

/// `apply_magic` needs `d` itself, not just the derived `MagicNumber` — this
/// is easy to get wrong (an earlier draft of this doc omitted it). The
/// classic sign-correction step ("add n back if d>0 and the multiplier came
/// out negative; subtract n if d<0 and it came out positive") depends on
/// d's OWN sign, which `m.multiplier`'s sign alone cannot always recover —
/// concretely, `magic_signed(100)` and `magic_signed(-7)` both produce a
/// negative multiplier, yet only the d=100 case needs the `+n` correction.
/// Forgetting `d` here produces answers that are right for most divisors and
/// silently wrong for specific others — exactly the "looks fine until a
/// particular edge case" bug class this project is built around avoiding.
fn apply_magic(n: i64, d: i64, m: &MagicNumber) -> i64 {
    let q = ((n as i128 * m.multiplier as i128) >> (64 + m.shift)) as i64;
    let q = if m.multiplier < 0 { q.wrapping_add(n) } else { q };
    let q = if m.shift > 0 { q + (q >> 63) } else { q }; // round toward zero
    if d > 0 { q } else { -q }
}

#[test]
fn magic_division_is_exact() {
    // Must be exact for EVERY input, including i64::MIN — the case that breaks
    // naive implementations, because |i64::MIN| is not representable.
    for d in [3i64, 5, 7, 10, 100, 1000, -3, -7, -100] {
        let m = magic_signed(d);
        for n in [0i64, 1, -1, 42, -42, i64::MAX, i64::MIN, i64::MAX - 1] {
            assert_eq!(apply_magic(n, d, &m), n.wrapping_div(d),
                       "magic division wrong for {n} / {d}");
        }
        let mut rng = StdRng::seed_from_u64(0xC0FFEE);
        for _ in 0..100_000 {
            let n: i64 = rng.gen();
            assert_eq!(apply_magic(n, d, &m), n.wrapping_div(d));
        }
    }
}
```

---

## Phase 11 — Differential Testing

```rust
// tests/differential.rs

/// THE core correctness test for the entire project.
///
/// A JIT that produces wrong answers is worse than no JIT, because the failure
/// is silent and data-dependent — it works on your test inputs and fails on a
/// customer's. The only defense is comparison against a trusted oracle across
/// a large random input space.
///
/// Comparison is BIT-EXACT via to_bits(), not approximate. Without fast-math
/// the JIT must produce IDENTICAL results; any drift means an optimization is
/// unsound or an instruction was mis-encoded. Using an epsilon here would hide
/// exactly the bugs this test exists to find.
///
/// Shown below for the all-f64 subset for brevity — `interpret`'s real
/// signature is `(f: &Function, args: &[RtValue]) -> RtValue` (see Phase 3),
/// and the real generator produces `Vec<RtValue>` matching each generated
/// expression's actual `f.params` types, so i64/bool parameters get exercised
/// too, not just f64.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(50_000))]

    #[test]
    fn jit_matches_interpreter(
        expr   in arb_expression(1..8),
        inputs in prop::collection::vec(arb_interesting_f64(), 0..8),
    ) {
        let ir = lower(&expr);
        let expected = interpret(&ir, &inputs);

        for opt in [OptLevel::None, OptLevel::Basic, OptLevel::Full] {
            let compiled = compile(&ir, opt, /* fast_math */ false).unwrap();
            let actual = compiled.call(&inputs);

            if expected.is_nan() {
                // NaN payloads can legitimately differ between an FPU result
                // and a constant-folded one, so compare NaN-ness, not bits.
                prop_assert!(actual.is_nan(), "expected NaN at {opt:?}, got {actual}");
            } else {
                prop_assert_eq!(expected.to_bits(), actual.to_bits(),
                    "MISMATCH at {opt:?}\n  expr: {expr}\n  in:   {inputs:?}\n  \
                     want: {expected} ({:#018x})\n  got:  {actual} ({:#018x})",
                    expected.to_bits(), actual.to_bits());
            }
        }
    }
}

/// Input generator biased toward the values that break things.
/// Uniform random f64 almost never produces zero, NaN, or Inf, so a naive
/// generator exercises none of the interesting cases.
fn arb_interesting_f64() -> impl Strategy<Value = f64> {
    prop_oneof![
        4 => (-1e6f64..1e6),                    // ordinary values
        1 => Just(0.0),  1 => Just(-0.0),
        1 => Just(f64::INFINITY), 1 => Just(f64::NEG_INFINITY),
        1 => Just(f64::NAN),
        1 => Just(f64::MIN_POSITIVE),           // smallest normal
        1 => Just(f64::MIN_POSITIVE / 2.0),     // subnormal
        1 => Just(f64::MAX), 1 => Just(f64::MIN),
        1 => Just(1.0), 1 => Just(-1.0),
    ]
}

/// SIMD must match scalar element for element, INCLUDING THE TAIL.
/// Running N = 1..100 hits every possible tail length for every vector width,
/// which is exactly where tail-handling bugs live.
#[test]
fn simd_matches_scalar_including_tail() {
    let expr = parse("a[i] * b[i] + c[i]").unwrap();
    for n in 1..=100usize {
        let (a, b, c) = random_arrays(n);
        let mut scalar = vec![0.0; n];
        let mut simd   = vec![0.0; n];

        compile_array(&expr, VectorWidth::One).call(&a, &b, &c, &mut scalar);
        compile_array(&expr, VectorWidth::Best).call(&a, &b, &c, &mut simd);

        for i in 0..n {
            assert_eq!(scalar[i].to_bits(), simd[i].to_bits(),
                       "n={n} i={i}: tail handling bug");
        }
    }
}
```

---

## Frontend — the panel that justifies the workbench

```tsx
// workbench/src/components/asm/HexPanel.tsx

/// The payoff of the entire project.
///
/// Most people have never seen the actual bytes their code becomes. Showing
/// them, with each byte's role decoded on hover — "this 0x48 is a REX prefix
/// with W=1 meaning 64-bit operand size" — is the single most illuminating
/// thing this workbench does.
export function HexPanel({ result }: { result: CompileResult }) {
  const [hovered, setHovered] = useState<number | null>(null);

  return (
    <div className="font-mono text-sm">
      {result.instructions.map((inst) => (
        <div key={inst.offset}
             className="grid grid-cols-[5rem_20rem_1fr] gap-4 hover:bg-muted/50"
             onClick={() => selectIrValue(inst.irValue)}>
          <span className="text-muted-foreground">
            {inst.offset.toString(16).padStart(4, '0')}
          </span>

          <span className="space-x-1">
            {inst.bytes.map((b, i) => (
              <span key={i}
                    className={cn('px-0.5 rounded cursor-help', BYTE_ROLE_COLOR[b.role])}
                    onMouseEnter={() => setHovered(inst.offset + i)}
                    onMouseLeave={() => setHovered(null)}>
                {b.value.toString(16).padStart(2, '0').toUpperCase()}
              </span>
            ))}
          </span>

          <span>{inst.disasm}</span>

          {hovered !== null && inst.containsOffset(hovered) && (
            <ByteTooltip byte={inst.byteAt(hovered)} />
          )}
        </div>
      ))}
    </div>
  );
}

const BYTE_ROLE_COLOR: Record<ByteRole, string> = {
  prefix: 'bg-purple-500/20 text-purple-300',   // REX / VEX / EVEX
  opcode: 'bg-blue-500/20 text-blue-300',
  modrm:  'bg-green-500/20 text-green-300',
  sib:    'bg-teal-500/20 text-teal-300',
  disp:   'bg-amber-500/20 text-amber-300',
  imm:    'bg-red-500/20 text-red-300',
};

/// Hovering 0x48 renders:
///   REX prefix — 0100 WRXB
///     W=1  64-bit operand size
///     R=0  ModRM.reg not extended
///     X=0  SIB.index not extended
///     B=0  ModRM.rm not extended
function ByteTooltip({ byte }: { byte: DecodedByte }) { /* … */ }
```

```tsx
// workbench/src/components/regalloc/IntervalChart.tsx

/// Live interval chart — makes register allocation comprehensible in one image.
///
/// Each bar is one SSA value spanning [start, end), colored by its assigned
/// physical register. Two bars that overlap horizontally can NEVER share a
/// color — that single constraint is the entire problem the allocator solves,
/// and seeing it as a picture beats any amount of prose.
///
/// The register pressure curve overlaid on top, with a red line at the machine
/// register count, shows exactly where spills become unavoidable.
export function IntervalChart({ alloc }: { alloc: Allocation }) {
  const x = d3.scaleLinear().domain([0, alloc.instCount]).range([0, width]);
  const y = d3.scaleBand<number>()
    .domain(alloc.intervals.map((_, i) => i))
    .range([0, height]).padding(0.15);

  return (
    <svg width={width} height={height + 60}>
      {alloc.intervals.map((iv, i) => (
        <rect key={i}
              x={x(iv.start)} y={y(i)}
              width={x(iv.end) - x(iv.start)} height={y.bandwidth()}
              fill={iv.spilled ? 'url(#hatch)' : REG_COLOR[iv.reg]}
              className="hover:stroke-white hover:stroke-2" />
      ))}

      <path d={pressureLine(alloc.pressure)} stroke="#f59e0b" fill="none" strokeWidth={2} />
      <line x1={0} x2={width}
            y1={pressureY(alloc.regCount)} y2={pressureY(alloc.regCount)}
            stroke="#ef4444" strokeDasharray="4 4" />
      <text x={width - 4} y={pressureY(alloc.regCount) - 4}
            textAnchor="end" fill="#ef4444" fontSize={11}>
        {alloc.regCount} registers — spills begin above this line
      </text>
    </svg>
  );
}
```

---

## Correctness Invariants

1. **Semantic equivalence** — `jit_matches_interpreter`, bit-exact, 50k random cases
2. **Optimization safety** — `-O0` == `-O1` == `-O2` without fast-math
3. **Encoding correctness** — every emitter round-trips through the disassembler
4. **ModRM special cases** — rsp, rbp, r12, r13 all tested explicitly
5. **VEX vvvv inversion** — tested with high-numbered registers, where the bug shows
6. **Register allocation soundness** — independent verifier on every compilation
7. **Stack alignment** — 16-byte at every call, verified with a probe that faults on misalignment
8. **W^X maintained** — no page is ever both writable and executable
9. **icache invalidated** — mandatory on Apple Silicon before any execution
10. **SIMD equivalence** — matches scalar for N = 1..100, covering every tail length
11. **Cross-architecture** — x86-64, AArch64 (QEMU), and WASM agree
12. **No leaks** — 10,000 allocate/free cycles leave RSS flat

---

## Code Standards

**Rust**
- **We encode by hand.** `iced-x86` and `capstone` are test oracles; they never appear in a non-test path.
- Every `unsafe` block has a `// SAFETY:` comment naming its preconditions. `#![deny(clippy::undocumented_unsafe_blocks)]`.
- `transmute` to a function pointer happens in exactly **one** place, behind an arity-checked API.
- **Never map RWX.** Allocate RW, write, flip to RX.
- On Apple Silicon, all writes go through `ExecutableBuffer::write()`, which performs the `pthread_jit_write_protect_np` dance and calls `sys_icache_invalidate`. There is no other way to write bytes.
- Run the IR verifier and the register-allocation verifier after every pass in debug builds. An optimizer bug caught at the pass that caused it is a ten-minute fix; caught three passes later it is a day.
- Every algebraic rule carries a `Validity` annotation. A float-unsafe rule must be unreachable without `--fast-math`.
- Bit-exact comparison in tests (`to_bits()`), never `assert!((a-b).abs() < eps)` — approximate comparison hides the encoding bugs the tests exist to catch.

**Frontend**
- The workbench runs the real compiler via WASM, never a reimplementation.
- Debounce recompilation at 200 ms; one pipeline run feeds every panel.
- D3 owns its SVG subtree; React owns everything around it.

---

## Startup

```bash
# Day one — before anything else
cargo run --example spike        # mmap → emit → mprotect → call

# On Apple Silicon
make codesign                    # entitlements are required

cargo test                       # unit tests
cargo test --test encoding       # round-trip through the disassembler
cargo test --test differential   # THE test — 50k random expressions
cargo bench

cargo run -- asm "sqrt(x*x + y*y)"
cargo run -- bench "a*b + c" --sizes 1,1K,1M

wasm-pack build crates/forge-wasm-api --target web --release
cd workbench && bun run dev
```

**The first command to run, and the one that sells the project:**

```
$ forge asm "sqrt(x*x + y*y)"

  offset  bytes                    assembly
  0000    55                       push rbp
  0001    48 89 E5                 mov  rbp, rsp
  0004    C5 FB 59 C0              vmulsd xmm0, xmm0, xmm0
  0008    C5 F3 59 C9              vmulsd xmm1, xmm1, xmm1
  000C    C5 FB 58 C1              vaddsd xmm0, xmm0, xmm1
  0010    C5 FB 51 C0              vsqrtsd xmm0, xmm0, xmm0
  0014    5D                       pop  rbp
  0015    C3                       ret

  21 bytes, 8 instructions, 0 spills, peak pressure 2/16
```

Twenty-one bytes. That's the whole function, and you wrote every one of those bytes by hand.

**Then the benchmark:**

```
$ forge bench "a[i]*b[i] + c[i]" --sizes 1M

                      time      ops/sec      vs interp
  interpreter       4200 ms      238 M/s          1.0×
  baseline JIT       8.4 ms      119 G/s        500.0×
  optimized JIT      3.1 ms      323 G/s       1354.8×
  SSE2   (2-wide)    1.7 ms      588 G/s       2470.6×
  AVX2+FMA (4-wide)  0.62 ms    1613 G/s       6774.2×
  AVX-512  (8-wide)  0.38 ms    2632 G/s      11052.6×

  note: 8-wide is only 1.6× faster than 4-wide — memory bandwidth bound
```

That last note is the real lesson. The interpreter→JIT jump is 500×; SIMD then scales cleanly until it doesn't, and understanding *why* 8-wide isn't 2× faster than 4-wide is worth more than the number itself.

**Then open the workbench** and hover a byte in Panel 6. Watch `0xC5` decompose into "2-byte VEX prefix, R=1 (inverted), vvvv=1111 (inverted → xmm0), L=0 (128-bit), pp=11 (F2 prefix)". Most people writing software have never seen this layer, and it's right there underneath everything they run.
