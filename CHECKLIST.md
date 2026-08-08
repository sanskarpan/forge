# CHECKLIST.md — `forge`: A JIT Compiler for Expression Evaluation

> Priority: 🔴 blocking · 🟡 important · 🟢 enhancement · 🔵 stretch
> **Differential testing (Phase 11) is the spine. A JIT that computes wrong answers silently is worse than no JIT. Wire up interpreter-vs-JIT comparison in Phase 6, the moment the first instruction executes.**
> **Every encoder function gets a disassembler round-trip test in the same commit. No exceptions.**

---

## Phase 0 — Bootstrap (14 tasks)

- [ ] 🔴 `cargo new --lib forge`; workspace with 13 member crates per SPEC §17
- [ ] 🔴 `cargo add libc nix region raw-cpuid smallvec bitvec rustc-hash thiserror anyhow`
- [ ] 🔴 `cargo add --dev iced-x86 capstone criterion proptest` — **`iced-x86` is a test oracle only, never used for encoding**
- [ ] 🔴 CI matrix: `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `aarch64-unknown-linux-gnu` (QEMU), `wasm32-unknown-unknown`
- [ ] 🔴 `forge-mem` platform detection: Linux / macOS-x64 / macOS-arm64 / Windows, with a clear error if none matches
- [ ] 🔴 macOS: `Info.plist` + entitlements file with `com.apple.security.cs.allow-jit`; codesign step in the Makefile
- [ ] 🔴 `Makefile`: `test`, `test-differential`, `test-encoding`, `bench`, `qemu-aarch64`, `wasm`, `workbench`
- [ ] 🔴 `#![deny(clippy::undocumented_unsafe_blocks)]` workspace-wide — every `unsafe` needs a `// SAFETY:` comment
- [ ] 🔴 `Span { start: u32, end: u32 }` and a diagnostic type with primary/secondary labels
- [ ] 🔴 `cd workbench && bun create vite . --template react-ts`
- [ ] 🔴 `bun add @codemirror/state @codemirror/view @codemirror/language @codemirror/lint d3 @dagrejs/dagre recharts zustand clsx lucide-react`
- [ ] 🔴 `bun add -d tailwindcss postcss autoprefixer @types/d3 vite-plugin-wasm vite-plugin-top-level-await`
- [ ] 🔴 `bunx shadcn@latest init`; add `button card tabs select badge tooltip separator scroll-area resizable slider switch table`
- [ ] 🔴 Hello-world JIT spike: `mmap` → emit `48 89 F8 C3` (`mov rax,rdi; ret`) → `mprotect` → `transmute` → call. **Do this on day one** — if executable memory doesn't work on your platform, nothing else matters.

---

## Phase 1 — Frontend: Lexer, Parser, Types (20 tasks)

- [ ] 🔴 `TokenKind`: number literals, identifiers, `+ - * / % ( ) , < <= > >= == != && || ! & | ^ << >> ~`, `if then else`, `let in`, `@vectorize` — `!` is logical not, `~` is bitwise not, kept as distinct tokens (SPEC §3 "Operators & precedence")
- [ ] 🔴 Hand-written lexer producing `(Vec<Token>, Vec<Diagnostic>)`, never `Result`
- [ ] 🔴 Float literals incl. exponent form; integer literals with `_` separators
- [ ] 🔴 Pratt parser: precedence table per SPEC §3 (`||` → `&&` → `|` → `^` → `&` → `==`/`!=` → relational → `<<`/`>>` → `+`/`-` → `*`/`/`/`%` → unary), left-assoc for arithmetic, right-assoc for none (no `**`)
- [ ] 🔴 Bitwise/shift operators (`& | ^ << >>`) type-checked as i64-only — using them on f64 is a type error with a span
- [ ] 🔴 Prefix: literal, identifier, `(`, unary `-`, unary `!`, `if`, `let`
- [ ] 🔴 Infix: all binary ops, `(` call
- [ ] 🔴 `if cond then a else b` as an **expression**, not a statement
- [ ] 🔴 `let x = e1 in e2` — creates CSE opportunities and a scope
- [ ] 🔴 Intrinsic call parsing with arity validation against a table
- [ ] 🔴 AST arena with `Idx<Expr>`, parallel `spans` vec
- [ ] 🔴 Type checker: `f64`, `i64`, `bool`; implicit `i64 → f64` widening where unambiguous
- [ ] 🔴 Type errors with both operand spans labeled
- [ ] 🔴 Comparison operators produce `bool`; `if` requires a `bool` condition and matching branch types
- [ ] 🔴 Parameter binding: free identifiers become parameters in declaration order
- [ ] 🔴 Constant environment: `--x 3.0` binds `x` as a compile-time constant, enabling folding
- [ ] 🔴 Test: precedence, associativity, unary binding
- [ ] 🔴 Test: type error on `1 + true`
- [ ] 🔴 Test: `if` branch type mismatch
- [ ] 🔴 Test: intrinsic arity mismatch
- [ ] 🟡 Property test: parse(print(ast)) == ast

---

## Phase 2 — SSA IR (26 tasks)

- [ ] 🔴 `Value(u32)`, `Block(u32)` — Copy, indices not pointers
- [ ] 🔴 `Inst` enum: all ~40 variants from SPEC §5
- [ ] 🔴 **`ConstF64(u64)` storing the bit pattern**, not `f64` — `f64` is neither `Hash` nor `Eq`, and GVN needs both
- [ ] 🔴 `Terminator::{ Return, Jump, Branch }`
- [ ] 🔴 `Function { insts, types, spans, blocks, entry, params }` with parallel vecs
- [ ] 🔴 `BlockData { insts: Vec<Value>, term, preds }`
- [ ] 🔴 Builder API: `add()`, `mul()`, `sqrt()`, … each returning a fresh `Value`
- [ ] 🔴 AST → IR lowering for straight-line expressions
- [ ] 🔴 `if` lowering: create then/else/merge blocks, emit `Branch`, insert a φ at the merge
- [ ] 🔴 `let` lowering: bind the name, lower the body — no instruction emitted
- [ ] 🔴 Braun et al. SSA construction: `read_variable` / `write_variable` / `read_variable_recursive`
- [ ] 🔴 **Incomplete-φ handling** to break cycles — without it a back-edge causes infinite recursion
- [ ] 🔴 Trivial-φ removal: a φ whose operands are all the same value, or all the same plus itself
- [ ] 🔴 `preds` maintained incrementally as blocks are linked
- [ ] 🔴 Reverse-postorder traversal for pass ordering
- [ ] 🔴 Dominance tree (Cooper-Harvey-Kennedy iterative algorithm)
- [ ] 🔴 `use_count` / `users` index, maintained by all passes
- [ ] 🔴 `replace_all_uses(old, new)` used by every rewriting pass
- [ ] 🔴 **IR verifier**: every value defined once; every use dominated by its def; φ operand count equals pred count; types consistent per opcode
- [ ] 🔴 **Run the verifier after every pass in debug builds** — catches optimizer bugs at the pass that caused them, not three passes later
- [ ] 🔴 Textual IR printer (the workbench's IR panel and `forge ir` both use it)
- [ ] 🔴 Test: `sqrt(x*x+y*y)` produces exactly 6 instructions
- [ ] 🔴 Test: `if` produces 4 blocks with a correct φ
- [ ] 🔴 Test: verifier rejects a hand-constructed use-before-def
- [ ] 🔴 Test: verifier rejects a φ with the wrong operand count
- [ ] 🟡 Test: dominance tree correct for nested `if`

---

## Phase 3 — Interpreter (Tier 0) (10 tasks)

**Build this early. It is the correctness oracle for everything that follows.**

- [ ] 🔴 `RtValue` enum (`F64(f64) | I64(i64) | Bool(bool)`) defined in `forge-ir`, matching `Function.params`' real per-parameter types (SPEC §3 "Runtime value representation") — not a bare `f64`, since params can be i64 or bool
- [ ] 🔴 `interpret(f: &Function, args: &[RtValue]) -> RtValue` walking the IR
- [ ] 🔴 Every `Inst` variant handled — a missing arm must be a compile error, not a panic
- [ ] 🔴 Block traversal following terminators, with φ resolution based on the incoming block
- [ ] 🔴 Intrinsics via Rust's `f64` methods, matching libm semantics exactly
- [ ] 🔴 `Call` to libm functions
- [ ] 🔴 IEEE-754 correctness: NaN propagation, ±0, ±Inf, subnormals — **no shortcuts**, this is the oracle
- [ ] 🔴 Integer overflow follows wrapping semantics consistently
- [ ] 🔴 Test: known values for every intrinsic
- [ ] 🔴 Test: NaN and Inf propagation through every arithmetic op
- [ ] 🔴 Test: `if` with a NaN comparison (all comparisons with NaN are false)

---

## Phase 4 — Optimizer (32 tasks)

- [ ] 🔴 Pass trait: `fn run(&mut self, f: &mut Function) -> bool` (returns "changed")
- [ ] 🔴 Driver running to a fixed point, capped at 10 iterations, with per-pass stats

**Constant folding**
- [ ] 🔴 Fold all arithmetic, comparison, and intrinsic ops on constants
- [ ] 🔴 **Do not fold in ways that change FP semantics.** Folding `0.0/0.0 → NaN` is correct; folding `x*0.0 → 0.0` is not (x may be NaN/Inf)
- [ ] 🔴 Integer folding uses wrapping arithmetic consistently with the interpreter

**Algebraic simplification**
- [ ] 🔴 Rule table with a `Validity` field: `Always` / `IntOnly` / `FastMathOnly`
- [ ] 🔴 `x+0`, `x-0`, `x*1`, `x/1`, `-(-x)` → always valid
- [ ] 🔴 `x*0→0`, `x-x→0`, `x/x→1`, `x&x→x`, `x^x→0` → **integer only** (NaN breaks all of them for floats)
- [ ] 🔴 `sqrt(x*x)→abs(x)`, `a*b+c→fma` → **fast-math only**
- [ ] 🔴 Commutative canonicalization: lower `Value` index first, so `a+b` and `b+a` unify

**Strength reduction**
- [ ] 🔴 `x * 2^k → x << k`; `x / 2^k → x >> k` with the signed rounding fixup
- [ ] 🔴 `x % 2^k → x & (2^k-1)`
- [ ] 🔴 **Magic-number division** (Granlund-Montgomery) for `x / C` — the most dramatic win, since `idiv` is 20–40 cycles and unpipelined
- [ ] 🔴 `x*3`, `x*5`, `x*9` → `lea` forms
- [ ] 🔴 `pow(x,2) → x*x`; `pow(x,0.5) → sqrt(x)`; `pow(x,-1) → 1/x`

**GVN / CSE**
- [ ] 🔴 Hash-cons on `(opcode, canonical operands)` in reverse-postorder
- [ ] 🔴 Commutative ops canonicalized before hashing — forgetting this halves the hit rate
- [ ] 🔴 Only CSE within a dominating region (a value must dominate its replacement's uses)

**Others**
- [ ] 🔴 Copy propagation
- [ ] 🔴 Dead code elimination: mark from the terminator backward, sweep unmarked
- [ ] 🟡 Reassociation: rebalance associative chains to minimize dependency depth (`((a+b)+c)+d` → `(a+b)+(c+d)`)
- [ ] 🟡 FMA contraction (fast-math only), emitting `Inst::Fma`
- [ ] 🟡 LICM for array mode: hoist loop-invariant computation out of the vectorized loop
- [ ] 🟡 Per-pass statistics: instructions removed, rules fired, dependency depth before/after

**Tests**
- [ ] 🔴 **Test: `-O0` and `-O2` produce bit-identical results** across the whole expression corpus (no fast-math)
- [ ] 🔴 Test: `x*0` is NOT folded for f64; IS folded for i64
- [ ] 🔴 Test: `(a+b)*(a+b)` CSEs to one add
- [ ] 🔴 Test: `a+b` and `b+a` CSE together
- [ ] 🔴 Test: magic division matches `idiv` for all of a large random sample, including `i64::MIN`
- [ ] 🔴 Test: DCE removes an unused subexpression entirely
- [ ] 🟡 Test: reassociation reduces dependency depth on a 8-term sum

---

## Phase 5 — Executable Memory (18 tasks)

**Get this right before writing a single encoder function.**

- [ ] 🔴 `ExecutableBuffer { ptr, len, state: ProtState }`
- [ ] 🔴 Linux/macOS-x64: `mmap(PROT_READ|PROT_WRITE)` → write → `mprotect(PROT_READ|PROT_EXEC)`
- [ ] 🔴 **Never map RWX.** W^X is not optional — an RWX page is exactly the primitive an attacker wants
- [ ] 🔴 Page-size rounding via `sysconf(_SC_PAGESIZE)`
- [ ] 🔴 macOS AArch64: `MAP_JIT` flag + `com.apple.security.cs.allow-jit` entitlement
- [ ] 🔴 macOS AArch64: **`pthread_jit_write_protect_np(0)` before writing, `(1)` after** — do NOT use `mprotect` on `MAP_JIT` pages, it fails
- [ ] 🔴 macOS AArch64: **`sys_icache_invalidate()` after every write.** The i-cache is not coherent with the d-cache on Apple Silicon; skipping this produces intermittent, unreproducible wrong behavior
- [ ] 🔴 Linux AArch64: `__builtin___clear_cache` equivalent (`core::arch::asm!("dc cvau"/"ic ivau"/"dsb ish"/"isb")`)
- [ ] 🟡 Windows: `VirtualAlloc(MEM_COMMIT, PAGE_READWRITE)` → `VirtualProtect(PAGE_EXECUTE_READ)` → `FlushInstructionCache`
- [ ] 🔴 `Drop` impl calling `munmap` / `VirtualFree`
- [ ] 🔴 `write<F: FnOnce(&mut [u8])>(f)` API making the protection dance impossible to skip
- [ ] 🔴 Type-state or runtime assert that `state == Executable` before any call
- [ ] 🔴 `CompiledExpr::call1/call2/callN` with an **arity assert** and a single documented `transmute`
- [ ] 🔴 A code cache: reuse buffers across compilations, with a free-list
- [ ] 🔴 Test: allocate, write `mov rax, 42; ret`, execute, get 42
- [ ] 🔴 Test: buffer is not writable after `make_executable` (expect SIGSEGV in a forked child) — **correction (Phase 5):** on this project's macOS AArch64 dev machine, a `MAP_JIT` write-protect violation empirically raises `SIGBUS`, not `SIGSEGV`; `crates/forge-mem/tests/wx_enforcement.rs` accepts either signal, since both are valid hardware protection-fault signals and which one a given OS/kernel raises isn't part of the portable contract being tested (only that W^X is enforced at all)
- [ ] 🔴 Test: no leaks — allocate/free 10,000 buffers, RSS stays flat
- [ ] 🔴 Test under Miri where possible; valgrind for the rest — **resolved as a documented non-goal (Phase 5), not silently skipped:** Miri cannot model raw `mmap`/`mprotect`/`sysconf` syscalls or a transmute-to-function-pointer call, so it cannot meaningfully run this crate's tests at all (see `forge-mem`'s crate-level doc comment); valgrind was never run either (no Apple Silicon/macOS ARM64 support to exercise it on this machine) and remains an open gap for a future Linux CI leg. Leak-freedom instead rests on `crates/forge-mem/tests/no_leaks.rs`'s empirical high-water-mark RSS check.

---

## Phase 6 — x86-64 Encoder (40 tasks)

**Every task here gets a disassembler round-trip test in the same commit.**

**Infrastructure**
- [ ] 🔴 `Assembler { code: Vec<u8>, labels, fixups }`
- [ ] 🔴 `PhysReg` enum for GPRs (RAX..R15) and XMM (XMM0..XMM31) with encoding numbers
- [ ] 🔴 Label/fixup machinery for forward jumps; `bind(label)` patches all pending fixups
- [ ] 🔴 Rel8 vs rel32 jump selection with automatic promotion when the offset doesn't fit — **correction (Phase 6a):** implemented as rel8-if-it-fits-else-rel32 for *backward* jumps (whose distance is known immediately at emit time) and unconditionally-rel32 for *forward* jumps (whose distance isn't known until the target label binds); there is no in-place "promote rel8 to rel32" byte-shifting/reflow step anywhere, since that would require adjusting every later label position and pending fixup for a JIT compiling small expressions — see `docs/superpowers/specs/2026-08-05-phase-6a-x64-encoder-infra-design.md`'s "Jump policy" section for the full justification

**Prefixes — the trap zone**
- [ ] 🔴 `rex(w, reg, index, rm)` emitting only when needed
- [ ] 🔴 **REX.W for 64-bit ops** — without it the op is 32-bit and *zeroes the upper 32 bits*
- [ ] 🔴 **REX.R/X/B for r8-r15** — without it you silently address rax-rdi instead
- [ ] 🔴 **Any REX makes spl/bpl/sil/dil replace ah/ch/dh/bh** — a silent different-register bug
- [ ] 🔴 Operand-size prefix `0x66`, address-size `0x67` where needed

**ModRM / SIB — three mandatory special cases**
- [ ] 🔴 `modrm_reg(reg, rm)` for register-direct (mod=11)
- [ ] 🔴 `modrm_mem(reg, base, disp)` for memory
- [ ] 🔴 **`base == RSP (4)` requires a SIB byte** — ModRM.rm=100 means "SIB follows", so `[rsp]` cannot be encoded directly
- [ ] 🔴 **`base == RBP (5)` with disp=0 must use mod=01 disp8=0** — mod=00 rm=101 means RIP-relative, not `[rbp]`
- [ ] 🔴 **R12 and R13 hit the same two cases via REX.B** — very easy to handle rsp/rbp and forget their twins
- [ ] 🔴 SIB with scale/index/base for `[base + index*scale + disp]`
- [ ] 🔴 RIP-relative addressing for constant pool loads

**Scalar integer instructions**
- [ ] 🔴 `mov` r/r, r/imm32, r/imm64 (`movabs`), r/m, m/r
- [ ] 🔴 `add` `sub` `imul` `and` `or` `xor` — r/r and r/imm forms
- [ ] 🔴 `neg` `not` `inc` `dec`
- [ ] 🔴 `shl` `shr` `sar` — imm8 and CL forms
- [ ] 🔴 `lea` — including the 3-operand `lea r, [a + b*k]` used by strength reduction
- [ ] 🔴 `cmp` `test`; `setcc`; `cmovcc` — **note (Phase 6c):** all four built (`cmp` as a new `AluOp` variant, `test` via its own `test_reg_reg`/`test_reg_imm`), plus a shared `ConditionCode` enum covering all 16 x86-64 condition codes (not just the 6 forge's current i64 comparisons need) reused by `setcc`/`cmovcc` and by `jcc` below. `jcc` itself shipped in this same slice, not bundled with `push`/`pop`/`call`/`ret` per this bullet's original wording — see the next bullet's correction. Details: `docs/superpowers/specs/2026-08-08-phase-6c-x64-comparisons-design.md`
- [ ] 🔴 `imul` 128-bit form for magic division; `idiv` — **note (Phase 6d):** both built (`imul128_reg`/`idiv_reg`), plus `cqo` alongside them even though it isn't literally in this bullet's wording — `idiv`'s RDX:RAX dividend pair is close to unusable without a way to sign-extend RAX into it first. Also delivered in this slice: `neg`/`not`/`inc`/`dec` and `shl`/`shr`/`sar` (previous two bullets) and `lea` including the 3-operand scaled-index form (bullet above), all via the same golden-byte + `iced-x86` round-trip discipline as 6a-6c. Details: `docs/superpowers/specs/2026-08-08-phase-6d-x64-shifts-lea-idiv-design.md`
- [ ] 🔴 `push` `pop` `call` `ret` `jmp` `jcc` — **correction (Phase 6c):** `jmp` (Phase 6a) and `jcc` (Phase 6c) are both implemented; `push`/`pop`/`call`/`ret` are not. This bullet's original grouping doesn't reflect how the work was actually split: `jcc` was deliberately pulled out and built alongside `cmp`/`test`/`setcc`/`cmovcc` instead, since a conditional branch plus a comparison is the coherent unit forge's `if`/`else` needs to compile at all, whereas `push`/`pop`/`call`/`ret` are real calling-convention work closer in spirit to Phase 7 ("Instruction Selection & Prologue"). See `docs/superpowers/specs/2026-08-08-phase-6c-x64-comparisons-design.md`'s scope note.

**SSE2 scalar float**
- [ ] 🔴 `movsd` `movapd` `movq` (xmm↔gpr)
- [ ] 🔴 `addsd` `subsd` `mulsd` `divsd` `sqrtsd`
- [ ] 🔴 `minsd` `maxsd` — note these are **not** commutative w.r.t. NaN, matching the interpreter matters
- [ ] 🔴 `andpd`/`xorpd` for `abs` and `neg` via sign-mask constants
- [ ] 🔴 `ucomisd` + `setcc` for comparisons
- [ ] 🔴 `cvtsi2sd` `cvttsd2si`
- [ ] 🔴 `roundsd` (SSE4.1) for floor/ceil/round/trunc — **note (Phase 6e):** all of this section built: `movsd_reg_reg`/`movsd_reg_mem`/`movsd_mem_reg`, `movapd_reg_reg`, `movq_gpr_to_xmm`/`movq_xmm_to_gpr` (bullet above); the `SseOp`-driven `addsd`/`subsd`/`mulsd`/`divsd`/`sqrtsd`/`minsd`/`maxsd` family via `sse_reg_reg`; `andpd_reg_reg`/`xorpd_reg_reg` as raw 2-operand bitwise primitives ONLY — composing them into actual `abs`/`neg` (materializing a sign-mask constant via `mov_reg_imm` + `movq_gpr_to_xmm`, or eventually a RIP-relative constant pool) is deliberately deferred to Phase 7's instruction-selection layer, not built in this slice; `ucomisd_reg_reg` (reusing 6c's `ConditionCode`/`setcc`/`jcc`/`cmovcc` machinery entirely unmodified, documented for use with the unsigned condition codes float comparisons require); and `cvtsi2sd`/`cvttsd2si`. `roundsd` itself is genuinely SSE4.1 not SSE2, as this bullet's own parenthetical already says; the encoder was still built here since CHECKLIST groups it with the SSE2 bullets — runtime CPUID feature detection gating its availability is a separate, later concern (Phase 10's `CpuFeatures`), not addressed by this slice. Details: `docs/superpowers/specs/2026-08-09-phase-6e-x64-sse2-scalar-float-design.md`

**VEX / AVX**
- [ ] 🟡 2-byte and 3-byte VEX emitters
- [ ] 🟡 **`vvvv` field is INVERTED (`!reg & 0xF`)** — the classic VEX bug
- [ ] 🟡 `vaddsd` `vmulsd` `vsqrtsd` etc. — 3-operand, non-destructive
- [ ] 🟡 `vfmadd213sd` / `vfmadd231sd` (FMA3)
- [ ] 🟡 Packed forms `vaddpd` `vmulpd` for 128/256-bit
- [ ] 🔵 EVEX prefix + AVX-512 packed ops with k-mask registers

**Verification**
- [ ] 🔴 `disassemble(&[u8]) -> Vec<String>` via `iced-x86`
- [ ] 🔴 **Round-trip test for every single instruction emitter** — assemble, disassemble, compare to intended text
- [ ] 🔴 Golden-file tests: expression → expected exact hex bytes
- [ ] 🔴 Test the rsp/rbp/r12/r13 ModRM cases explicitly, all four

---

## Phase 7 — Instruction Selection & Prologue (22 tasks)

- [ ] 🔴 `MachineInst` enum sitting between IR and encoding
- [ ] 🔴 Tree-tiling selection: maximal munch over the IR DAG
- [ ] 🔴 **Two-address fixup**: x86 `add` is `dst += src`, but SSA is 3-address. Insert `mov dst, a` before `add dst, b`, unless a coalescing hint already put `a` in `dst`
- [ ] 🔴 Addressing-mode folding: `Load{base, offset}` folds into the memory operand of the consuming instruction
- [ ] 🔴 `lea` synthesis for `a + b*k + c`
- [ ] 🔴 `Select` → `cmov` (integer) or `vblendvpd` / `minsd`+`maxsd` idioms (float) — branchless where profitable
- [ ] 🔴 Constant pool: f64 constants placed after the code, loaded RIP-relative
- [ ] 🔴 Sign-mask constants for `abs`/`neg`
- [ ] 🔴 Prologue: `push rbp; mov rbp, rsp; sub rsp, N` with N = spill-slot bytes
- [ ] 🔴 **Stack alignment: rsp must be 16-byte aligned at every `call`.** The return address pushed by `call` makes rsp ≡ 8 mod 16 on entry, so the frame size must account for that. Getting this wrong crashes inside libm with a `movaps` fault
- [ ] 🔴 Callee-saved register save/restore, only for registers actually used
- [ ] 🔴 Epilogue: `mov rsp, rbp; pop rbp; ret` (or `leave; ret`)
- [ ] 🔴 Red zone (System V): 128 bytes below rsp usable without adjustment in leaf functions
- [ ] 🔴 **Win64 shadow space: 32 bytes allocated by the caller** before any `call`. Omitting it corrupts the callee's frame
- [ ] 🔴 libm call sequence: spill live caller-saved registers, align, `call`, restore
- [ ] 🔴 **All XMM registers are caller-saved on System V** — any `sin`/`cos` call clobbers every float register, which is why `sin(x)+cos(y)` spills and `sqrt(x)+sqrt(y)` doesn't
- [ ] 🔴 Argument marshalling per ABI: SysV (rdi/rsi/…, xmm0-7) vs Win64 (rcx/rdx/r8/r9, xmm0-3)
- [ ] 🔴 Return value in `xmm0` (float) or `rax` (int)
- [ ] 🔴 Test: generated function callable from Rust via `extern "C"`
- [ ] 🔴 Test: callee-saved registers unchanged across a call (assert with inline asm)
- [ ] 🔴 Test: stack alignment holds at every call site (checked with a probe function that faults on misalignment)
- [ ] 🔴 Test: an expression calling `sin` and `cos` produces the correct value

---

## Phase 8 — Register Allocation (24 tasks)

- [ ] 🔴 Linearize the IR: assign a sequential number to every instruction in RPO
- [ ] 🔴 Liveness analysis: backward dataflow, `live_in`/`live_out` per block
- [ ] 🔴 Build `Interval { value, start, end, reg_class, hint, fixed, spill_weight }`
- [ ] 🔴 Intervals must extend across the whole loop body for values live around a back-edge
- [ ] 🔴 φ handling: an interval spans from the φ to all its incoming definitions
- [ ] 🔴 Sort intervals by start point
- [ ] 🔴 `active` list kept **sorted by END point** — the invariant that makes expiry a cheap prefix scan
- [ ] 🔴 `expire_old_intervals(current_start)` freeing registers
- [ ] 🔴 `pick_register` preferring the hint (coalescing) then any free register
- [ ] 🔴 **`fixed` registers are non-negotiable** — ABI argument positions and `idiv`'s implicit rax/rdx force eviction of whoever holds them
- [ ] 🔴 `spill_at_interval`: pick the victim
- [ ] 🔴 **Spill heuristic: furthest endpoint, weighted by use density.** The textbook picks furthest endpoint; weighting by `uses/length` measurably beats it on expression trees, where a value used 4× in a tight window must not be spilled
- [ ] 🔴 Spill slot allocation on the stack frame, with slot reuse after an interval ends
- [ ] 🔴 Insert reload before each use of a spilled value, store after its definition
- [ ] 🔴 Register hints from two-address fixups and from φ operands (coalescing)
- [ ] 🔴 Separate allocation for GPR and XMM classes
- [ ] 🔴 **Independent allocation verifier**: no two overlapping intervals share a register. Written separately from the allocator so it can't share a bug with it
- [ ] 🔴 Report register pressure per program point (drives the workbench panel)
- [ ] 🔴 Test: 3 values, 16 registers → no spills
- [ ] 🔴 Test: 40 simultaneously live values, 16 registers → correct results with spills
- [ ] 🔴 Test: verifier catches a deliberately broken allocation
- [ ] 🔴 Test: expression calling libm → caller-saved values are spilled around the call
- [ ] 🔴 Test: coalescing eliminates redundant `mov` for a two-address chain
- [ ] 🟡 Benchmark: allocation of 1000 values < 50 µs

---

## Phase 9 — AArch64 Backend (20 tasks)

- [ ] 🟡 `PhysReg` for X0-X30, SP, and V0-V31
- [ ] 🟡 Fixed-width 32-bit instruction emitter: `Vec<u32>`, `to_le_bytes` at the end
- [ ] 🟡 `add`/`sub` shifted-register and immediate forms (12-bit imm with optional LSL 12)
- [ ] 🟡 `mul` `sdiv` `madd` `msub`
- [ ] 🟡 `and` `orr` `eor` `lsl` `lsr` `asr`
- [ ] 🟡 `fadd` `fsub` `fmul` `fdiv` `fsqrt` `fabs` `fneg` (double)
- [ ] 🟡 **`fmadd` / `fmsub` — 3-operand FMA in the base ISA**, where x86 needed a whole new instruction set
- [ ] 🟡 `fcmp` + `fcsel` (branchless select, cleaner than x86's `cmov`+`ucomisd` dance)
- [ ] 🟡 `fmin` `fmax` `frintm`/`frintp`/`frintn`/`frintz` (floor/ceil/round/trunc)
- [ ] 🟡 `scvtf` `fcvtzs`
- [ ] 🟡 `ldr` `str` with the scaled-immediate offset encoding
- [ ] 🟡 `b` `b.cond` `bl` `ret` with 26-bit / 19-bit offsets
- [ ] 🟡 **Immediate encoding: the one genuine pain.** Integers use the bitmask-immediate form (rotated run of ones) or a MOVZ/MOVK sequence of up to 4 instructions
- [ ] 🟡 **Float immediates: only 64 specific values are encodable.** Everything else needs a literal pool load
- [ ] 🟡 `encode_logical_imm(value) -> Option<(N, immr, imms)>`
- [ ] 🟡 AAPCS64: X0-X7 / V0-V7 args, X19-X28 callee-saved, 16-byte stack alignment
- [ ] 🟡 Prologue/epilogue with `stp`/`ldp` pairs
- [ ] 🟡 Round-trip verification via `capstone`
- [ ] 🟡 Test under QEMU on x86 CI
- [ ] 🟡 **Test: same expression, same result on x86-64 and AArch64** (bit-identical)

---

## Phase 10 — SIMD Vectorization (22 tasks)

- [ ] 🟡 `CpuFeatures` via `raw-cpuid`: SSE2, SSE4.1, AVX, AVX2, FMA, AVX-512F/DQ, BMI2
- [ ] 🟡 AArch64: NEON always present; detect SVE if available
- [ ] 🟡 `best_width(ty)` selecting 2/4/8 lanes for f64
- [ ] 🟡 **Runtime selection is a genuine JIT advantage over AOT** — surface this in the workbench
- [ ] 🟡 Array-mode IR: `Load`/`Store` with an induction variable, loop blocks
- [ ] 🟡 Vectorizer: scalar op → lane-wise op, scalar load → `VecLoad`, unroll by width
- [ ] 🟡 `Splat` for loop-invariant scalars
- [ ] 🟡 **Tail handling**: `N % width` leftover elements via a scalar epilogue
- [ ] 🔵 AVX-512 masked tail using k-registers — no epilogue needed at all
- [ ] 🟡 Alignment: use `vmovupd` by default; emit `vmovapd` only when alignment is provable
- [ ] 🟡 Reductions: horizontal sum via `vextractf128` + `vaddpd` + `vhaddpd`
- [ ] 🟡 Packed encoders: `vaddpd` `vmulpd` `vsqrtpd` `vfmadd231pd` at 128/256 bit
- [ ] 🔵 EVEX 512-bit forms
- [ ] 🟡 NEON: `fadd.2d` `fmul.2d` `fmla.2d`
- [ ] 🟡 Loop prologue/epilogue with the induction variable and bounds check
- [ ] 🟡 **Test: SIMD result == scalar result, element for element, including the tail** — run for N = 1..100 to hit every tail length
- [ ] 🟡 Test: unaligned input produces correct results
- [ ] 🟡 Test: reduction matches a sequential sum (note: FP addition isn't associative, so document the expected difference and use an epsilon *only here*)
- [ ] 🟡 Benchmark: 1M-element `a*b+c` at each width
- [ ] 🟡 Benchmark: demonstrate memory-bandwidth saturation — 8-wide is *not* 2× faster than 4-wide, and that's the lesson
- [ ] 🟡 Fallback path when a feature is absent, verified by masking features off
- [ ] 🟡 Test on a CPU without AVX2 (or with features masked) → correct scalar fallback

---

## Phase 11 — Differential Testing & Verification (18 tasks)

**The spine. Wire the first three tasks up in Phase 6.**

- [ ] 🔴 `arb_expression(depth)` proptest strategy generating well-typed random expressions
- [ ] 🔴 **`jit_matches_interpreter`: bit-exact comparison via `to_bits()`**, not approximate equality — without fast-math the JIT must produce *identical* results and any drift is a real bug
- [ ] 🔴 NaN handling: `is_nan()` on both sides rather than bit comparison (NaN payloads may differ legitimately)
- [ ] 🔴 Input generation covering: normal values, ±0, ±Inf, NaN, subnormals, `f64::MIN`/`MAX`
- [ ] 🔴 **Optimization-level equivalence: `-O0` == `-O1` == `-O2`** across the corpus
- [ ] 🔴 Fast-math mode compared with a documented, bounded epsilon
- [ ] 🔴 **Encoding round-trip for every instruction emitter**
- [ ] 🔴 Golden hex tests: expression → exact expected bytes, so encoding changes are deliberate
- [ ] 🟡 Cross-architecture: x86-64 vs AArch64 (QEMU) vs WASM produce identical results
- [ ] 🔴 Register allocation verifier run on every compilation in debug builds
- [ ] 🔴 IR verifier run after every pass in debug builds
- [ ] 🟡 Fuzz target: random bytes → parser → never panics
- [ ] 🟡 Fuzz target: random IR → optimizer → verifier still passes
- [ ] 🔴 Stress test: compile and execute 100,000 random expressions, assert no crashes and no leaks
- [ ] 🔴 Miri run over everything except the actual `transmute`-and-call (which Miri cannot model)
- [ ] 🟡 valgrind for the executable-memory paths
- [ ] 🔴 Test: expression with 500 live values (forces heavy spilling) still correct
- [ ] 🔴 Test: deeply nested expression (depth 100) compiles and runs

---

## Phase 12 — Tiered Runtime (12 tasks)

- [ ] 🟡 `TieredExpr { ir, tier: AtomicU8, invocations: AtomicU64, baseline: OnceCell, optimized: OnceCell }`
- [ ] 🟡 Thresholds: baseline at 10 invocations, optimized at 1000
- [ ] 🟡 `eval()` incrementing the counter and dispatching to the current tier
- [ ] 🟡 Compilation triggered exactly once per tier via `OnceCell`
- [ ] 🟡 Baseline JIT: skip the optimizer entirely, naive register allocation, fast to compile
- [ ] 🟡 Optimizing JIT: full pipeline + SIMD
- [ ] 🟡 Thread safety: multiple threads may call concurrently; compilation happens once
- [ ] 🟡 Compile-time and per-tier execution-time instrumentation
- [ ] 🟡 **Break-even analysis**: compile cost / (per-call saving) = invocations to profit. Surface this number
- [ ] 🟡 Test: results identical regardless of tier
- [ ] 🟡 Test: tier transitions happen at the right invocation counts
- [ ] 🔵 On-stack replacement for long-running array loops

---

## Phase 13 — CLI (14 tasks)

- [ ] 🔴 `clap` derive with all subcommands from SPEC §16
- [ ] 🔴 `eval EXPR --x 3 --y 4` — parse, compile, run, print
- [ ] 🔴 `ir EXPR [--after PASS]` — textual SSA IR
- [ ] 🔴 **`asm EXPR`** — offset / hex bytes / disassembly, three aligned columns
- [ ] 🔴 `--annotate` on `asm`: per-byte field breakdown (REX bits, ModRM mod/reg/rm decoded)
- [ ] 🔴 `cfg EXPR --dot` — graphviz output
- [ ] 🔴 `regalloc EXPR` — intervals, assignments, spills, peak pressure
- [ ] 🔴 `bench EXPR --sizes 1,10,100,1K,1M` — interpreter vs tiers vs SIMD widths
- [ ] 🔴 `verify EXPR --iters 100000` — differential run with a summary
- [ ] 🔴 `cpuinfo` — detected features and chosen vector width
- [ ] 🔴 `compile EXPR --arch --opt --features` with an `--emit` flag
- [ ] 🟡 REPL with history, persistent variable bindings, `:asm` / `:ir` / `:bench` commands
- [ ] 🔴 Colored output honoring `NO_COLOR`
- [ ] 🔴 Exit codes: 0 ok, 1 runtime error, 2 compile error, 3 verification failure

---

## Phase 14 — WASM Backend & Bindings (14 tasks)

- [ ] 🟡 `forge-wasm`: IR → WASM bytes
- [ ] 🟡 Stack-machine emission via post-order traversal — no register allocation needed
- [ ] 🟡 f64 opcodes: `f64.const/add/sub/mul/div/sqrt/abs/neg/min/max/floor/ceil/nearest/trunc`
- [ ] 🟡 Comparison + `select` for conditionals
- [ ] 🟡 Locals for `let` bindings and spilled values
- [ ] 🟡 Full module encoding: type/function/memory/export sections
- [ ] 🟡 Runtime instantiation via `WebAssembly.instantiate` — a real JIT, in the browser
- [ ] 🔴 `forge-wasm-api` with `wasm-bindgen`
- [ ] 🔴 `parse_and_check(src) -> { ast, diagnostics }`
- [ ] 🔴 `compile(src, opts) -> { ir_stages, cfg, intervals, asm, hex, wasm_bytes }`
- [ ] 🔴 `run(src, args) -> f64` executing the WASM backend
- [ ] 🔴 `benchmark(src, sizes) -> BenchResult` — real timings in the browser
- [ ] 🔴 `cpu_features()` (reports the *simulated* target, since the browser can't see host features)
- [ ] 🔴 `wasm-pack build --target web --release`; `wasm-opt -Oz`; bundle < 1.1 MB gzipped

---

## Phase 15 — Workbench Frontend (34 tasks)

**Foundation**
- [ ] 🔴 WASM init with a loading state; zustand store holding source + all compilation artifacts
- [ ] 🔴 Debounced recompile (200 ms); one pipeline run fans out to all panels
- [ ] 🔴 Resizable panel grid, persisted to localStorage

**Panel 1 — Editor**
- [ ] 🔴 CodeMirror 6 with the expression language; error squiggles from our diagnostics
- [ ] 🔴 Variable binding panel: set values for free variables
- [ ] 🔴 Mode toggle: scalar / array (SIMD)
- [ ] 🟡 Example gallery; share-via-URL

**Panel 2 — AST + SSA IR ⭐**
- [ ] 🔴 D3 tree for the AST
- [ ] 🔴 Textual SSA IR with per-value hover
- [ ] 🔴 **Three-way linking**: hover an IR value → highlights the AST node *and* the source span
- [ ] 🟡 Type annotation on every value

**Panel 3 — Optimization Pipeline ⭐**
- [ ] 🔴 Stepper through every pass, with before/after IR
- [ ] 🔴 **Diff view**: removed instructions red, added green
- [ ] 🔴 Per-pass: which rules fired, instruction count delta, dependency depth delta
- [ ] 🟡 Rule table with `Validity` annotation (Always / IntOnly / FastMathOnly) and an explanation of why
- [ ] 🟡 Fast-math toggle showing which extra rules unlock

**Panel 4 — CFG**
- [ ] 🟡 D3 + dagre graph; blocks with instruction lists
- [ ] 🟡 Edges labeled with branch conditions
- [ ] 🟡 φ-nodes highlighted with edges to incoming blocks

**Panel 5 — Register Allocation ⭐**
- [ ] 🔴 **Live interval chart**: X = instruction index, one bar per value, colored by physical register
- [ ] 🔴 Overlapping bars can never share a color — the constraint made visible
- [ ] 🔴 Spilled values with a hatched pattern; spill/reload points marked
- [ ] 🔴 **Register pressure curve** overlaid, with a red line at the register count — where it crosses is exactly where spills occur
- [ ] 🟡 Hover an interval → highlight the IR value and its uses

**Panel 6 — Assembly + Hex ⭐**
- [ ] 🔴 Three synchronized columns: offset / hex bytes / disassembly
- [ ] 🔴 **Hover a byte → field breakdown**: VEX prefix, opcode, ModRM with mod/reg/rm decoded, displacement, immediate
- [ ] 🔴 Click an instruction → highlight the originating IR value
- [ ] 🟡 Color-code byte roles (prefix / opcode / modrm / sib / disp / imm)
- [ ] 🟡 Total code size, instruction count

**Panel 7 — Benchmark & Tiering ⭐**
- [ ] 🔴 Bar chart: interpreter vs baseline vs optimized vs each SIMD width
- [ ] 🟡 Line chart: time vs input size, showing bandwidth saturation
- [ ] 🟡 **Tier-up timeline**: invocations on X, tier as a step function, compile spikes marked
- [ ] 🟡 Break-even callout: "compiling pays off after N calls"

**Panel 8 — Target Selector**
- [ ] 🟡 Toggle SSE2 / AVX2 / FMA / AVX-512 / NEON; toggle x86-64 / AArch64 / WASM
- [ ] 🟡 Panels 5–7 regenerate on change
- [ ] 🟡 **Side-by-side x86 vs AArch64** of the same function — 21 bytes of variable-length CISC vs 8 fixed 32-bit words

---

## Phase 16 — Docs & Polish (12 tasks)

- [ ] 🟢 `README.md` — what it is, the Rust rationale, quickstart, workbench link, headline benchmark
- [ ] 🟢 `docs/ENCODING.md` — x86-64 instruction format walkthrough with worked examples
- [ ] 🟢 `docs/REGALLOC.md` — linear scan explained with the interval diagrams
- [ ] 🟢 `docs/OPTIMIZATION.md` — every pass, every rule, and why FP validity differs
- [ ] 🟢 `docs/PLATFORMS.md` — W^X, MAP_JIT, icache, entitlements, Windows differences
- [ ] 🟡 Error messages naming the exact encoding constraint violated
- [ ] 🟡 `--verbose` tracing each compilation phase with timing
- [ ] 🟡 Benchmark table in the README with real machine numbers
- [ ] 🟡 `cargo clippy -- -D warnings`; `cargo fmt --check`; `bun run tsc --noEmit`
- [ ] 🔵 Loop support (`while`) with LICM and OSR
- [ ] 🔵 Instruction scheduling to shorten dependency chains
- [ ] 🔵 Profile-guided specialization: record observed value ranges, recompile with them as constants

---

## Summary

| Phase | Tasks |
|---|---|
| 0. Bootstrap | 14 |
| 1. Frontend | 20 |
| 2. SSA IR | 26 |
| 3. Interpreter (oracle) | 10 |
| 4. Optimizer | 32 |
| 5. Executable Memory | 18 |
| 6. x86-64 Encoder | 40 |
| 7. Instruction Selection & Prologue | 22 |
| 8. Register Allocation | 24 |
| 9. AArch64 Backend | 20 |
| 10. SIMD Vectorization | 22 |
| 11. Differential Testing | 18 |
| 12. Tiered Runtime | 12 |
| 13. CLI | 14 |
| 14. WASM Backend & Bindings | 14 |
| 15. Workbench | 34 |
| 16. Docs & Polish | 12 |
| **TOTAL** | **352** |
