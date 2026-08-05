# SPEC.md — `forge`: A JIT Compiler for Expression Evaluation

> **Backend: Rust 2021 (MSRV 1.80+)** — parser, SSA IR, optimizer, x86-64 + AArch64 assemblers, register allocator, executable-memory manager, profiling tiered runtime
> **Frontend: React 18 + TypeScript + Vite + CodeMirror 6 + Tailwind + shadcn/ui + D3 + Recharts** — a browser workbench that shows the IR, the CFG, register lifetimes, and the actual encoded bytes
> **Targets: x86-64 (SSE2/AVX2/AVX-512) and AArch64 (NEON)** — same IR, two real backends

---

## §1 Language Decision — Rust

### The hard requirements

A JIT compiler has to do four things that most languages make painful:

| Requirement | Why it's hard | Rust |
|---|---|---|
| **Allocate W^X memory and jump into it** | `mmap(PROT_EXEC)`, `mprotect`, `MAP_JIT`, `pthread_jit_write_protect_np`, instruction-cache invalidation | `libc` + `nix` give direct syscall access; `unsafe` blocks make every dangerous transition explicit and auditable |
| **Emit exact byte sequences** | Off-by-one in a REX prefix produces a plausible-looking instruction that corrupts a different register | `Vec<u8>`, `#[repr(u8)]` enums, `to_le_bytes()` — zero abstraction between your intent and the bytes |
| **Cast a data pointer to a function pointer and call it** | The single most `unsafe` operation in systems programming | `std::mem::transmute` at exactly one place, wrapped in a safe API |
| **Represent an SSA IR with sum types** | Instructions are an ADT; a missing case is a silent bug | `enum` + exhaustive `match` |

### Why not the alternatives

- **C/C++** — the traditional choice, and you get memory-safety bugs in a component whose failure mode is *"jump to arbitrary bytes"*. A JIT bug in C is a segfault at best and an exploitable primitive at worst. The debugging experience is genuinely awful because the corruption happens in generated code with no source-level debugger.
- **Go** — no sum types for the IR, a GC that can move things under you, and cgo overhead on every call into generated code. Go's runtime also assumes it owns the stack in ways that make custom calling conventions fragile.
- **Zig** — genuinely excellent for this, arguably the closest competitor. Loses on ecosystem: no `wasm-bindgen` equivalent for the workbench, weaker testing infrastructure, and no `criterion`.
- **Python/JS** — you can write a toy JIT (minijit does it in 500 lines of Python), but you can't write a register allocator, a real optimizer, or measure anything meaningful.

**Rust is the only language where the dangerous parts are explicitly marked, the IR is expressible as a sum type, and the whole thing still compiles to WASM for the workbench.**

### Crates

| Crate | Role |
|---|---|
| `libc` / `nix` | `mmap`, `mprotect`, `sysconf`, `pthread_jit_write_protect_np` |
| `region` | cross-platform page protection (fallback path) |
| `iced-x86` | **disassembler only**, for verification and the workbench — never for encoding |
| `capstone` | AArch64 disassembly |
| `raw-cpuid` | runtime CPU feature detection (AVX2, AVX-512, FMA, BMI2) |
| `criterion` | benchmarks with statistical rigor |
| `proptest` | differential testing: interpreter vs JIT over random expressions |
| `wasm-bindgen` | workbench build |
| `rustc-hash`, `smallvec`, `bitvec` | compiler data structures |
| `clap`, `rustyline` | CLI + REPL |

**We write our own assembler.** No `dynasm-rs`, no `cranelift`, no LLVM. Encoding x86-64 by hand — ModRM, SIB, REX, displacement — is the point of the project. `iced-x86` appears only to *disassemble* what we produced, as a test oracle.

### Frontend: React + CodeMirror 6 + D3

The workbench must show, side by side and live: source → AST → SSA IR → optimized IR → CFG → register lifetimes → assembly → **raw hex bytes** → benchmark results. That's a dense multi-panel technical UI with two graph visualizations (CFG, interference/lifetime chart) and time-series charts.

- **CodeMirror 6** — the expression editor, plus a *read-only* view for IR and assembly with custom highlighting and hover linking back to source spans
- **D3** — CFG as a dagre-laid-out graph; register lifetime chart as a Gantt-style interval plot
- **Recharts** — throughput vs input size, tier-up timeline
- **shadcn/ui + Tailwind** — the dense panel chrome

The whole compiler compiles to WASM, so **the workbench runs the real compiler**, not a reimplementation. The one thing it can't do is *execute* the generated x86 — so the workbench also ships a WASM backend (§10) that actually runs, plus an emulator for stepping through x86 semantics.

---

## §2 What This Project Covers

| Area | Concepts |
|---|---|
| Frontend | Expression grammar, Pratt parsing, constant/variable/call binding, type checking (f64/i64/bool/vec) |
| IR | SSA construction, basic blocks, φ-nodes, dominance frontiers, use-def chains |
| Optimization | Constant folding, algebraic simplification, strength reduction, CSE (value numbering), DCE, copy propagation, LICM, reassociation, common subexpression elimination across blocks |
| Instruction selection | Tree tiling / maximal munch, addressing-mode folding, two-address fixups, FMA contraction |
| Register allocation | Linear scan over live intervals, spilling with the furthest-endpoint heuristic, register hints, coalescing, ABI constraints (caller/callee-saved) |
| Encoding | x86-64: REX, ModRM, SIB, displacement, immediates, VEX (AVX), EVEX (AVX-512). AArch64: fixed-width 32-bit encodings, immediate encoding constraints |
| Calling conventions | System V AMD64, Microsoft x64, AAPCS64 — argument registers, stack alignment, red zone, shadow space |
| Executable memory | `mmap`/`mprotect`, W^X, `MAP_JIT` + `pthread_jit_write_protect_np` on Apple Silicon, `sys_icache_invalidate`, Windows `VirtualAlloc` |
| SIMD | Runtime feature detection, vectorized evaluation over arrays, 128/256/512-bit widths, masked tails |
| Tiered compilation | Interpreter → baseline JIT → optimizing JIT, invocation counters, on-stack replacement (stretch) |
| Performance | Instruction scheduling, dependency chains, throughput vs latency, `perf` counters, branch prediction |
| Verification | Differential testing vs interpreter, round-trip encode→disassemble→compare, fuzzing |

---

## §3 The Expression Language

Deliberately small enough to implement completely, large enough that every optimization has something to bite on.

```
# scalars
3.14159 * r * r
(a + b) * (a - b)
x*x + 2*x + 1

# variables bound at compile time to slots
f(x, y) = sqrt(x*x + y*y)

# conditionals compile to branches or cmov
max(a, b) = if a > b then a else b
clamp(x, lo, hi) = min(max(x, lo), hi)

# calls to a fixed intrinsic set
sin(x) + cos(y) * exp(-z)

# let-bindings create CSE opportunities
let t = x*x + y*y in sqrt(t) / (1 + t)

# array mode: the SAME expression, evaluated over N elements → SIMD
@vectorize
result[i] = a[i] * b[i] + c[i]          # → vfmadd231pd

# integer domain
(n * 2654435761) >> 16                  # strength reduction: * → shift+lea
n / 7                                   # → magic-number multiply
```

### Type system

Deliberately minimal: `f64`, `i64`, `bool`, plus `vec<f64, N>` / `vec<i64, N>` introduced by the vectorizer. Implicit widening `i64 → f64` where unambiguous; everything else is a type error with a span.

"Unambiguous" is scoped narrowly, not "anywhere an f64 and an i64 meet": it applies to **arithmetic operators** (`+ - * / %`) and **intrinsic call arguments** (every intrinsic is f64-only, so an i64 argument always widens rather than errors). It deliberately does **not** apply to comparisons (`== != < <= > >=`), bitwise/shift/logical operators (already i64-or-bool-only by design, §3 "Operators & precedence"), or `if`/`else` branch matching — those keep requiring an exact type match. The reason arithmetic gets widening and comparisons/branches don't: arithmetic is where the surface language actually produces the mismatch in practice (an integer literal like `1` next to an `f64` variable, e.g. `x + 1`), and widening there has one obvious meaning (compute in `f64`). Extending the same silent-coercion behavior to comparisons or branch unification is a different, unification-like design question — with no current example in this spec motivating it — so it's left as a deliberate non-goal rather than an oversight to "finish" later.

### Intrinsics

`sqrt` `abs` `min` `max` `floor` `ceil` `round` `trunc` `sin` `cos` `tan` `exp` `log` `pow` `fma`

- `sqrt`, `abs`, `min`, `max`, `floor`, `ceil`, `round`, `trunc` → **single instructions** (`vsqrtsd`, `vandpd`, `vminsd`, `vroundsd`)
- `sin`, `cos`, `exp`, `log`, `pow` → **calls into libm**, which forces the project to handle a real call sequence: caller-saved spilling, stack alignment, and the difference between the System V and Win64 ABIs
- `fma` → `vfmadd213sd` when FMA is available, otherwise `mul` + `add` (and the workbench shows the precision difference)

### Operators & precedence

Full token set, lowest to highest precedence — this is what the Pratt parser's binding powers encode:

```
||                              logical or
&&                              logical and
|                               bitwise or        (i64 only)
^                               bitwise xor       (i64 only)
&                               bitwise and       (i64 only)
== !=                           equality
< <= > >=                       relational
<< >>                           shift             (i64 only)
+ -                             additive
* / %                           multiplicative
unary - ! ~                     prefix: negate, logical not, bitwise not
```

`!` is logical not (`bool → bool`); `~` is bitwise not (`i64 → i64`) — kept as
distinct tokens so `!x` and `~x` are never ambiguous. The bitwise/shift row
requires both operands to be `i64`; using them on `f64` is a type error with
a span, exactly like any other operator misuse.

### Runtime value representation

`Function.params: Vec<(Symbol, Ty)>` allows real, independently-typed `f64` /
`i64` / `bool` parameters — not just `f64`. So every value that crosses an
API boundary (interpreter arguments and results, and eventually compiled-
function calls) is represented as:

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RtValue {
    F64(f64),
    I64(i64),
    Bool(bool),
}
```

`interpret(f: &Function, args: &[RtValue]) -> RtValue` is the real signature
(§14.1 and Phase 3's oracle both use it). Fixed-arity convenience wrappers
like `CompiledExpr::call1`/`call2` (§11) stay `f64`-specific shortcuts for
the common all-float case; the general entry point is
`CompiledExpr::call(&self, args: &[RtValue]) -> RtValue`.

---

## §4 Architecture

```
   source: "sqrt(x*x + y*y)"
        │
   ┌────▼─────┐
   │  Lexer   │
   │  Parser  │  Pratt, precedence climbing
   └────┬─────┘
        │  AST
   ┌────▼─────┐
   │ TypeCheck│  f64 / i64 / bool, implicit widening
   └────┬─────┘
        │
   ┌────▼─────────────────────┐
   │  SSA IR Construction     │  basic blocks, φ-nodes, dominance
   └────┬─────────────────────┘
        │  Ir { blocks, insts, values }
   ┌────▼─────────────────────┐
   │  Optimizer (pass pipeline)│
   │  ├ constant folding       │
   │  ├ algebraic simplify     │
   │  ├ strength reduction     │
   │  ├ GVN / CSE              │
   │  ├ copy propagation       │
   │  ├ DCE                    │
   │  ├ reassociation          │
   │  └ FMA contraction        │
   └────┬─────────────────────┘
        │  optimized IR
   ┌────▼─────────────────────┐
   │  Liveness Analysis       │  live intervals per SSA value
   └────┬─────────────────────┘
        │
   ┌────▼─────────────────────┐
   │  Linear Scan RegAlloc    │  assign phys regs, insert spills
   └────┬─────────────────────┘
        │  IR + RegAssignment
   ┌────▼─────────────────────┐
   │  Instruction Selection   │  tree tiling, addressing modes,
   │                          │  two-address fixups
   └────┬─────────────────────┘
        │  MachineInst
   ┌────▼──────────┬──────────┬────────────┐
   │ x86-64 Encoder│ AArch64  │ WASM       │
   │ REX/ModRM/SIB │ Encoder  │ Encoder    │
   │ VEX/EVEX      │ 32-bit   │ (workbench)│
   └────┬──────────┴─────┬────┴──────┬─────┘
        │  Vec<u8>       │           │
   ┌────▼────────────────▼───────────▼─────┐
   │  ExecutableBuffer                     │
   │  mmap → write → mprotect(RX) →        │
   │  icache invalidate → transmute → CALL │
   └───────────────────────────────────────┘
```

**Tiered execution** wraps the whole thing:

```
   Tier 0: tree-walking interpreter      — instant, ~200 ns/eval
   Tier 1: baseline JIT (no optimizer)   — compile in ~10 µs, ~8 ns/eval
   Tier 2: optimizing JIT + SIMD         — compile in ~80 µs, ~1.5 ns/eval

   Promotion driven by an invocation counter, exactly like a real VM.
```

---

## §5 SSA Intermediate Representation

```rust
// crates/forge-ir/src/lib.rs

/// Values are SSA: each is defined exactly once. Referenced by index, not
/// pointer — the whole IR lives in flat Vecs for cache locality and to avoid
/// fighting the borrow checker during transformation passes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Value(u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Block(u32);

#[derive(Clone, Debug)]
pub enum Inst {
    // Constants
    ConstF64(u64),                 // bit pattern — f64 isn't Hash/Eq
    ConstI64(i64),
    ConstBool(bool),

    // Parameters bound at compile time to argument registers or memory slots
    Param { index: u32, ty: Ty },

    // Arithmetic
    Add(Value, Value), Sub(Value, Value), Mul(Value, Value),
    Div(Value, Value), Rem(Value, Value), Neg(Value),

    // Fused multiply-add, one correctly-rounded operation. Two origins:
    // (1) `fma(a,b,c)` is a real intrinsic (§3) and the parser lowers it
    // here directly; (2) fast-math FMA contraction (§6.5) rewrites `a*b + c`
    // patterns into this same instruction. Same opcode either way — codegen
    // never needs to know which origin produced it.
    Fma { a: Value, b: Value, c: Value },   // a*b + c in one instruction

    // Bitwise / shifts (integer domain)
    And(Value, Value), Or(Value, Value), Xor(Value, Value), Not(Value),
    Shl(Value, Value), Shr(Value, Value), Sar(Value, Value),

    // Comparison → bool
    Cmp { op: CmpOp, lhs: Value, rhs: Value },

    // Selection — becomes cmov / vblendvpd / csel, avoiding a branch
    Select { cond: Value, then_: Value, else_: Value },

    // Intrinsics
    Sqrt(Value), Abs(Value), Min(Value, Value), Max(Value, Value),
    Floor(Value), Ceil(Value), Round(Value), Trunc(Value),

    // Library call — forces real ABI handling
    Call { func: LibFunc, args: SmallVec<[Value; 2]> },

    // Conversion
    IToF(Value), FToI(Value),

    // Memory (array mode)
    Load  { base: Value, offset: i32, ty: Ty },
    Store { base: Value, offset: i32, value: Value },

    // SSA merge
    Phi { incoming: SmallVec<[(Block, Value); 2]> },

    // SIMD (introduced by the vectorizer, never by the parser)
    Splat  { scalar: Value, lanes: u8 },
    VecAdd(Value, Value), VecMul(Value, Value), VecFma { a: Value, b: Value, c: Value },
    VecLoad  { base: Value, offset: i32, lanes: u8 },
    VecStore { base: Value, offset: i32, value: Value, lanes: u8 },
    VecReduce { op: ReduceOp, vec: Value },
}

#[derive(Clone, Debug)]
pub enum Terminator {
    Return(Value),
    Jump(Block),
    Branch { cond: Value, then_: Block, else_: Block },
}

pub struct Function {
    pub insts: Vec<Inst>,
    pub types: Vec<Ty>,             // parallel to insts
    pub spans: Vec<Span>,           // parallel to insts — every value traces to source
    pub blocks: Vec<BlockData>,
    pub entry: Block,
    pub params: Vec<(Symbol, Ty)>,
}

pub struct BlockData {
    pub insts: Vec<Value>,          // in order
    pub term: Terminator,
    pub preds: SmallVec<[Block; 2]>,
}
```

### SSA construction

For straight-line expressions SSA is trivial — every subexpression is a new value. `if` introduces the only real work:

```rust
/// Braun et al., "Simple and Efficient Construction of Static Single
/// Assignment Form" (2013). We don't need full dominance-frontier φ placement
/// because expressions produce reducible, shallow CFGs: at most one merge per
/// `if`. But the algorithm is implemented properly so `while` (a stretch goal)
/// works without a rewrite.
impl Builder {
    fn read_variable(&mut self, var: Symbol, block: Block) -> Value {
        if let Some(&v) = self.current_def[&block].get(&var) { return v; }
        self.read_variable_recursive(var, block)
    }

    fn read_variable_recursive(&mut self, var: Symbol, block: Block) -> Value {
        let preds = &self.func.blocks[block].preds;
        if preds.len() == 1 {
            // Single predecessor: no φ needed, just look through.
            return self.read_variable(var, preds[0]);
        }
        // Multiple predecessors: insert an INCOMPLETE φ first to break cycles,
        // then fill its operands. Without the incomplete-φ trick, a loop
        // back-edge causes infinite recursion.
        let phi = self.new_phi(block);
        self.write_variable(var, block, phi);
        self.add_phi_operands(var, phi)
    }
}
```

---

## §6 Optimization Passes

Each pass is a `fn(&mut Function) -> bool` returning whether it changed anything; the driver runs to a fixed point (capped at 10 iterations).

### 6.1 Constant folding

```rust
/// Straightforward, with one non-obvious rule: NEVER fold operations whose
/// result depends on floating-point environment or produces NaN/Inf, unless
/// --ffast-math is on. Folding 0.0/0.0 to NaN at compile time is correct;
/// folding x*0.0 to 0.0 is NOT (x may be NaN or Inf).
fn fold(inst: &Inst, consts: &FxHashMap<Value, Const>) -> Option<Inst> {
    match inst {
        Inst::Add(a, b) => match (consts.get(a)?, consts.get(b)?) {
            (Const::F64(x), Const::F64(y)) => Some(Inst::ConstF64((x + y).to_bits())),
            (Const::I64(x), Const::I64(y)) => Some(Inst::ConstI64(x.wrapping_add(*y))),
            _ => None,
        },
        // …
    }
}
```

### 6.2 Algebraic simplification

```rust
/// Each rule is annotated with whether it is valid for floats. This table is
/// where most "optimizing compiler broke my numerics" bugs come from.
const RULES: &[(&str, Validity)] = &[
    // ⚠ "x + 0 → x" is NOT unconditionally valid for f64: IEEE-754 defines
    // (-0.0) + (+0.0) = +0.0, so if x is -0.0, adding a literal +0.0 flips
    // its sign — the rule silently changes the answer. The direction that
    // IS always safe is adding NEGATIVE zero: x + (-0.0) → x holds for
    // every x, including x = -0.0 and x = +0.0. (i64 has no signed zero, so
    // "x + 0 → x" is unconditionally fine there — this caveat is f64-only.)
    ("x + (-0.0) → x",      Validity::Always),      // f64: the only always-safe add-zero direction
    ("x + 0     → x",       Validity::IntOnly),      // f64: unsafe when x = -0.0 (see above)
    // x - 0 → x IS always safe for f64 (subtracting +0.0 is the same as
    // adding -0.0), unlike the addition case above — don't "fix" this one
    // to match; the asymmetry is real and IEEE-754-mandated.
    ("x - 0     → x",       Validity::Always),
    ("x * 1     → x",       Validity::Always),
    ("x * 0     → 0",       Validity::IntOnly),     // ⚠ f64: NaN*0 = NaN, Inf*0 = NaN
    ("x / 1     → x",       Validity::Always),
    ("x - x     → 0",       Validity::IntOnly),     // ⚠ f64: NaN - NaN = NaN
    // ⚠ i64: this is only safe when x is KNOWN non-zero (e.g. a literal).
    // For an arbitrary SSA value, "x / x → 1" would silently erase a
    // runtime division-by-zero trap the unsimplified program would have
    // hit at x = 0 — the optimizer deliberately does NOT implement this
    // direction for that reason (see forge-opt's simplify.rs).
    ("x / x     → 1",       Validity::IntOnly),     // ⚠ f64: 0/0 = NaN
    ("x * 2     → x + x",   Validity::Always),
    ("x + x     → x * 2",   Validity::Always),
    ("-(-x)     → x",       Validity::Always),
    ("x & x     → x",       Validity::IntOnly),
    ("x ^ x     → 0",       Validity::IntOnly),
    ("sqrt(x*x) → abs(x)",  Validity::FastMathOnly),// ⚠ overflow differs
    ("a*b + c   → fma",     Validity::FastMathOnly),// ⚠ changes rounding
];
```

The workbench surfaces this table with each rule's validity, because "why did my compiler change my answer in the 15th decimal place" is a real and educational question.

### 6.3 Strength reduction

```rust
/// Integer division by a constant → multiply by a magic number + shift.
/// This is the single most dramatic optimization to demonstrate: `idiv` has
/// 20–40 cycle latency and is not pipelined; the magic-number sequence is
/// 3 instructions with ~5 cycles total.
///
/// Granlund & Montgomery, "Division by Invariant Integers using Multiplication"
fn magic_divide(d: i64) -> MagicNumber {
    // Compute (M, s) such that n/d == (n * M) >> (64 + s) for all n
    // …
}

const REDUCTIONS: &[&str] = &[
    "x * 2^k        → x << k",
    "x / 2^k        → x >> k        (signed: needs a rounding fixup)",
    // ⚠ "x % 2^k → x & (2^k - 1)" as written is ONLY correct for
    // non-negative x. Our `%` (like Rust's) is truncating — the remainder's
    // sign follows the dividend — but `x & (2^k-1)` computes the EUCLIDEAN
    // remainder, which is always non-negative. These disagree for negative
    // x: -7 % 4 == -3 (truncating), but -7 & 3 == 1 (masking). The masked
    // form is only a valid strength reduction when x is provably
    // non-negative; the general signed case needs the same sign-fixup
    // machinery as the division rule above it (reuse the corrected quotient
    // q and compute the remainder as x - (q << k), which is correct by
    // construction from q rather than a separately-derived bit trick).
    "x % 2^k        → x & (2^k - 1)   (unsigned/non-negative x only — see caveat)",
    "x / C          → magic multiply + shift",
    "x * 3          → lea (x + x*2)",
    "x * 5          → lea (x + x*4)",
    "x * 9          → lea (x + x*8)",
    // ⚠ Investigated during Phase 4 and NOT implemented: `pow(x, 2)`,
    // `pow(x, 0.5)`, and `pow(x, -1)` were all empirically checked against
    // this platform's libm across a large random sample (see forge-opt's
    // strength.rs) and NONE are bit-exact against `x*x`/`sqrt(x)`/`1/x`
    // respectively — general-purpose `pow()` is not guaranteed
    // correctly-rounded the way IEEE multiply/sqrt/divide are, so this
    // "obvious" rewrite silently changes answers by 1 ULP (or differs in
    // sign/NaN-ness at special values) on real hardware. Listed here as the
    // rule you'd expect a compiler to have, specifically so a reader knows
    // it was considered and rejected, not overlooked.
    "pow(x, 2)      → x * x           (rejected — not bit-exact, see caveat)",
    "pow(x, 0.5)    → sqrt(x)         (rejected — not bit-exact, see caveat)",
    "pow(x, -1)     → 1 / x           (rejected — not bit-exact, see caveat)",
    "exp(x) * exp(y)→ exp(x + y)     (fast-math only)",
];
```

### 6.4 Global value numbering (CSE)

```rust
/// Hash-consing on (opcode, operands). Because the IR is SSA, a value's
/// number never changes, so this is exact and needs no dataflow iteration.
///
/// Commutative operations canonicalize operand order (lower Value index first)
/// so `a + b` and `b + a` get the same number. Forgetting this halves the CSE
/// hit rate on real expressions.
fn gvn(f: &mut Function) -> bool {
    let mut table: FxHashMap<InstKey, Value> = FxHashMap::default();
    // Visit in reverse-postorder so definitions precede uses.
    for block in f.rpo() {
        for &v in &f.blocks[block].insts {
            let key = canonical_key(&f.insts[v]);
            match table.entry(key) {
                Occupied(e) => f.replace_all_uses(v, *e.get()),
                Vacant(e)   => { e.insert(v); }
            }
        }
    }
}
```

### 6.5 The full pipeline

```
  1. constant folding
  2. algebraic simplification
  3. strength reduction
  4. copy propagation
  5. GVN / CSE
  6. reassociation          (regroup to expose more CSE + shorten dep chains)
  7. FMA contraction        (only with --fast-math)
  8. dead code elimination
  → repeat until fixed point, max 10 rounds
```

**Reassociation** deserves special mention: `(a+b)+(c+d)` has a dependency chain of depth 2, while `((a+b)+c)+d` has depth 3. On a superscalar CPU the first form is measurably faster because the two additions issue in parallel. The workbench visualizes the dependency DAG depth before and after.

---

## §7 Register Allocation — Linear Scan

Graph coloring produces better allocations; **linear scan is the right choice for a JIT** because compile time is on the critical path and it's near-linear rather than quadratic. This is exactly why HotSpot's client compiler switched to it.

```rust
// crates/forge-regalloc/src/linear_scan.rs

/// A live interval is [start, end) in linearized instruction numbering.
/// Because our IR is SSA, intervals are naturally short and mostly contiguous.
#[derive(Clone, Debug)]
pub struct Interval {
    pub value: Value,
    pub start: u32,
    pub end: u32,
    pub reg_class: RegClass,        // Gpr | Xmm
    pub hint: Option<PhysReg>,      // prefer this register (coalescing)
    pub fixed: Option<PhysReg>,     // ABI-mandated (e.g. call argument)
    pub spill_weight: f32,          // uses / length — spill the cheapest
}

pub struct LinearScan {
    intervals: Vec<Interval>,       // sorted by start
    active: Vec<usize>,             // sorted by END — the key invariant
    free_regs: RegSet,
    assignment: FxHashMap<Value, Location>,
    spill_slots: u32,
}

impl LinearScan {
    /// Poletto & Sarkar (1999), with the Mössenböck & Pfeiffer SSA refinements.
    pub fn run(&mut self) {
        for i in 0..self.intervals.len() {
            self.expire_old_intervals(self.intervals[i].start);

            // Fixed ABI registers are non-negotiable — evict whoever holds it.
            if let Some(phys) = self.intervals[i].fixed {
                self.evict_and_assign(i, phys);
                continue;
            }

            if let Some(reg) = self.pick_register(i) {
                self.assign(i, reg);
            } else {
                self.spill_at_interval(i);
            }
        }
    }

    /// `active` is kept sorted by END so this is a cheap prefix scan.
    fn expire_old_intervals(&mut self, current_start: u32) {
        while let Some(&j) = self.active.first() {
            if self.intervals[j].end > current_start { break; }
            self.active.remove(0);
            self.free_regs.insert(self.reg_of(j));
        }
    }

    /// THE SPILL HEURISTIC.
    /// Standard linear scan spills the active interval with the FURTHEST
    /// endpoint, on the theory that it blocks a register for longest. But
    /// this is only a heuristic — you may spill any active interval — and
    /// weighting by use density (uses/length) measurably beats it on
    /// expression trees, where a value used four times in a tight window
    /// should never be the one spilled.
    fn spill_at_interval(&mut self, i: usize) {
        let victim = *self.active.iter()
            .max_by(|&&a, &&b| {
                let sa = self.intervals[a].end as f32 / self.intervals[a].spill_weight.max(0.01);
                let sb = self.intervals[b].end as f32 / self.intervals[b].spill_weight.max(0.01);
                sa.partial_cmp(&sb).unwrap()
            })
            .expect("no active interval to spill");

        if self.intervals[victim].end > self.intervals[i].end {
            let reg = self.reg_of(victim);
            self.assign(i, reg);
            self.spill(victim);
        } else {
            self.spill(i);
        }
    }
}
```

### ABI constraints

```rust
/// System V AMD64 (Linux, macOS)
pub const SYSV_INT_ARGS:  &[PhysReg] = &[RDI, RSI, RDX, RCX, R8, R9];
pub const SYSV_FLOAT_ARGS:&[PhysReg] = &[XMM0, XMM1, XMM2, XMM3, XMM4, XMM5, XMM6, XMM7];
pub const SYSV_CALLEE_SAVED: &[PhysReg] = &[RBX, RBP, R12, R13, R14, R15];
/// All XMM registers are caller-saved in System V — so ANY libm call
/// clobbers every float register. This is why `sin(x) + cos(y)` needs spills
/// and `sqrt(x) + sqrt(y)` doesn't.

/// Microsoft x64 (Windows)
pub const WIN64_INT_ARGS:  &[PhysReg] = &[RCX, RDX, R8, R9];
pub const WIN64_FLOAT_ARGS:&[PhysReg] = &[XMM0, XMM1, XMM2, XMM3];
pub const WIN64_CALLEE_SAVED: &[PhysReg] =
    &[RBX, RBP, RDI, RSI, R12, R13, R14, R15, XMM6, XMM7, /* … XMM15 */];
/// Win64 additionally requires 32 bytes of SHADOW SPACE allocated by the
/// caller, and XMM6-15 are callee-saved. Forgetting the shadow space produces
/// a crash inside libm that is very hard to trace back.

/// AAPCS64 (AArch64)
pub const AAPCS_INT_ARGS:  &[PhysReg] = &[X0, X1, X2, X3, X4, X5, X6, X7];
pub const AAPCS_FLOAT_ARGS:&[PhysReg] = &[V0, V1, V2, V3, V4, V5, V6, V7];
```

---

## §8 x86-64 Encoder

The heart of the project. **We encode by hand.**

### 8.1 Instruction anatomy

```
┌────────┬─────┬────────┬────────┬─────┬───────────────┬──────────┐
│ Legacy │ REX │ Opcode │ ModRM  │ SIB │ Displacement  │Immediate │
│Prefixes│     │        │        │     │               │          │
│ 0-4 B  │0-1 B│ 1-3 B  │ 0-1 B  │0-1 B│  0,1,2,4 B    │0,1,2,4,8B│
└────────┴─────┴────────┴────────┴─────┴───────────────┴──────────┘

REX:   0100 W R X B
       W = 1 → 64-bit operand size
       R = extends ModRM.reg  (access r8-r15 as the reg operand)
       X = extends SIB.index
       B = extends ModRM.rm / SIB.base / opcode reg

ModRM: mm rrr bbb
       mod: 00 = [rm]            (no displacement)
            01 = [rm + disp8]
            10 = [rm + disp32]
            11 = rm is a REGISTER, not memory
       reg: register operand (or opcode extension for /0../7)
       rm:  register or memory operand

SIB:   ss iii bbb            (present when ModRM.rm == 100 in memory mode)
       scale = 1 << ss, index, base  →  [base + index*scale + disp]
```

### 8.2 Encoder

```rust
// crates/forge-x64/src/encode.rs

pub struct Assembler {
    code: Vec<u8>,
    labels: Vec<Option<usize>>,
    fixups: Vec<Fixup>,
}

impl Assembler {
    /// The REX prefix is the #1 source of subtle JIT bugs, because omitting it
    /// silently changes which register you addressed rather than failing.
    ///
    /// Three traps, all of which produce working-looking wrong code:
    ///   1. Without REX.W the operation is 32-bit and ZEROES the upper 32 bits.
    ///   2. Without REX.R/B you address rax-rdi instead of r8-r15.
    ///   3. With ANY REX prefix, byte registers spl/bpl/sil/dil replace
    ///      ah/ch/dh/bh — silently different registers.
    fn rex(&mut self, w: bool, reg: u8, index: u8, rm: u8) {
        let byte = 0x40
            | ((w as u8) << 3)
            | (((reg   >> 3) & 1) << 2)   // REX.R
            | (((index >> 3) & 1) << 1)   // REX.X
            |  ((rm    >> 3) & 1);        // REX.B
        // Emit only when needed — but ALWAYS when W, or when any register
        // index is >= 8, or when addressing spl/bpl/sil/dil.
        if byte != 0x40 { self.code.push(byte); }
    }

    fn modrm_reg(&mut self, reg: u8, rm: u8) {
        self.code.push(0b11 << 6 | ((reg & 7) << 3) | (rm & 7));
    }

    /// Memory operand encoding, with three cases that MUST be special-cased:
    ///
    ///   • rm == RSP (4): ModRM.rm = 100 means "SIB follows", so you cannot
    ///     encode [rsp] directly — a SIB byte with index=100 (none) is required.
    ///   • rm == RBP (5) with disp == 0: mod=00, rm=101 means RIP-relative,
    ///     NOT [rbp]. You must force mod=01 with disp8 = 0.
    ///   • r12/r13 hit the same cases via REX.B, and it is very easy to handle
    ///     rsp/rbp but forget their extended twins.
    fn modrm_mem(&mut self, reg: u8, base: u8, disp: i32) {
        let base_low = base & 7;

        if base_low == 4 {                       // RSP or R12 → SIB required
            let mode = disp_mode(disp);
            self.code.push(mode << 6 | ((reg & 7) << 3) | 0b100);
            self.code.push(0b00_100_100);        // scale=1, index=none, base=rsp/r12
            self.emit_disp(mode, disp);
        } else if base_low == 5 && disp == 0 {   // RBP or R13 → must use disp8
            self.code.push(0b01 << 6 | ((reg & 7) << 3) | base_low);
            self.code.push(0);                   // explicit zero displacement
        } else {
            let mode = disp_mode(disp);
            self.code.push(mode << 6 | ((reg & 7) << 3) | base_low);
            self.emit_disp(mode, disp);
        }
    }
}
```

### 8.3 VEX and EVEX

```rust
/// VEX (AVX/AVX2): 2-byte or 3-byte prefix replacing REX + legacy prefixes,
/// and crucially adding a THIRD operand, so `c = a + b` becomes one
/// non-destructive instruction instead of `mov c,a; add c,b`.
///
/// 3-byte VEX:  C4  RXB.mmmmm  W.vvvv.L.pp
///   vvvv is INVERTED (~reg & 0xF) — a classic encoding trap
///   L: 0 = 128-bit (xmm), 1 = 256-bit (ymm)
///   pp: mandatory prefix (00=none 01=66 10=F3 11=F2)
fn vex3(&mut self, r: u8, x: u8, b: u8, mmmmm: u8, w: bool, vvvv: u8, l: bool, pp: u8) {
    self.code.push(0xC4);
    self.code.push((!r & 1) << 7 | (!x & 1) << 6 | (!b & 1) << 5 | mmmmm);
    self.code.push((w as u8) << 7 | ((!vvvv & 0xF) << 3) | (l as u8) << 2 | pp);
    //                                ^^^^^^^^^^^ INVERTED
}

/// EVEX (AVX-512): 4-byte prefix adding 512-bit width, 32 registers,
/// per-lane masking (k0-k7), embedded broadcast, and rounding control.
fn evex(&mut self, /* … */) { /* … */ }
```

### 8.4 Verification: round-trip through a disassembler

```rust
/// EVERY encoding function has a test that assembles an instruction,
/// disassembles it with iced-x86, and compares the text to what we intended.
///
/// This catches the entire class of "plausible but wrong" encodings that are
/// otherwise invisible until the generated code silently computes garbage.
#[test]
fn encoding_round_trip() {
    let mut a = Assembler::new();
    a.mov_reg_reg(R12, RAX);
    a.add_reg_imm32(R13, 42);
    a.vaddsd(XMM15, XMM0, XMM1);

    let text = disassemble(&a.code);
    assert_eq!(text, vec![
        "mov r12, rax",
        "add r13, 42",
        "vaddsd xmm15, xmm0, xmm1",
    ]);
}
```

---

## §9 AArch64 Encoder

Refreshingly simple after x86: **every instruction is exactly 32 bits**, no prefixes, no variable length.

```rust
/// ADD (shifted register):
///  sf 0 0 01011 shift(2) 0 Rm(5) imm6 Rn(5) Rd(5)
///  sf = 1 for 64-bit
pub fn add_reg(rd: u8, rn: u8, rm: u8) -> u32 {
    0x8B00_0000 | ((rm as u32) << 16) | ((rn as u32) << 5) | (rd as u32)
}

/// FADD (scalar double):
///  0001 1110 011 Rm(5) 0010 10 Rn(5) Rd(5)
pub fn fadd_d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x1E60_2800 | ((rm as u32) << 16) | ((rn as u32) << 5) | (rd as u32)
}

/// FMADD — a THREE-operand fused multiply-add, natively. AArch64 has this in
/// the base ISA; x86 needed a whole new instruction set (FMA3) for it.
pub fn fmadd_d(rd: u8, rn: u8, rm: u8, ra: u8) -> u32 {
    0x1F40_0000 | ((rm as u32) << 16) | ((ra as u32) << 10)
                | ((rn as u32) << 5)  | (rd as u32)
}

/// The one genuine pain: immediates. AArch64 cannot encode an arbitrary
/// 64-bit constant. Integers use the "bitmask immediate" encoding (a
/// rotated run of ones) or need a MOVZ/MOVK sequence of up to 4 instructions.
/// Floats have an 8-bit encoding covering only 64 specific values; anything
/// else must be materialized from a literal pool.
pub fn encode_logical_imm(value: u64, sf: bool) -> Option<(u8, u8, u8)> { /* N, immr, imms */ }
```

The comparison is genuinely instructive: the x86 encoder is ~1500 lines with a dozen special cases; the AArch64 encoder is ~600 lines that are almost all straight-line bit packing. And AArch64 gets a 3-operand FMA for free where x86 needed VEX.

---

## §10 WASM Backend (for the workbench)

The workbench runs in a browser, where we obviously cannot execute generated x86. So there's a third backend emitting **WebAssembly bytes**, assembled at runtime via `WebAssembly.instantiate`. Same IR, same optimizer, same register allocation *(skipped — WASM is a stack machine)*, real measurable speedup over the interpreter.

```rust
/// WASM is a stack machine, so instruction selection is trivial: post-order
/// traversal, push operands, emit the operator. No register allocation.
///
/// This backend is genuinely useful, not a toy: it lets the workbench
/// demonstrate real JIT compilation with real timings in the browser, and it
/// makes the "IR is target-independent" claim concrete by having a third
/// target with a completely different execution model.
fn emit_wasm(f: &Function, out: &mut Vec<u8>) {
    for &v in f.postorder() {
        match &f.insts[v] {
            Inst::ConstF64(bits) => { out.push(0x44); out.extend(bits.to_le_bytes()); }
            Inst::Add(_, _)      => out.push(0xA0),   // f64.add
            Inst::Mul(_, _)      => out.push(0xA2),   // f64.mul
            Inst::Sqrt(_)        => out.push(0x9F),   // f64.sqrt
            // …
        }
    }
}
```

---

## §11 Executable Memory — The Dangerous Part

```rust
// crates/forge-mem/src/lib.rs

pub struct ExecutableBuffer {
    ptr: *mut u8,
    len: usize,
    state: ProtState,     // Writable | Executable
}

#[cfg(target_os = "linux")]
impl ExecutableBuffer {
    /// W^X: never map RWX. Allocate RW, write, then flip to RX.
    /// Mapping RWX works and is what every tutorial does, but it means a bug
    /// anywhere in the process can write into a page that is about to be
    /// executed — the exact primitive attackers want.
    pub fn new(size: usize) -> io::Result<Self> {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let len = (size + page - 1) & !(page - 1);
        let ptr = unsafe {
            libc::mmap(ptr::null_mut(), len,
                       libc::PROT_READ | libc::PROT_WRITE,     // NOT EXEC yet
                       libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0)
        };
        if ptr == libc::MAP_FAILED { return Err(io::Error::last_os_error()); }
        Ok(Self { ptr: ptr as *mut u8, len, state: ProtState::Writable })
    }

    pub fn make_executable(&mut self) -> io::Result<()> {
        let rc = unsafe { libc::mprotect(self.ptr as _, self.len,
                                         libc::PROT_READ | libc::PROT_EXEC) };
        if rc != 0 { return Err(io::Error::last_os_error()); }
        self.state = ProtState::Executable;
        Ok(())
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl ExecutableBuffer {
    /// APPLE SILICON IS COMPLETELY DIFFERENT and this trips up every JIT port.
    ///
    /// 1. You MUST pass MAP_JIT and hold the com.apple.security.cs.allow-jit
    ///    entitlement. Without it, mprotect(PROT_EXEC) fails outright.
    /// 2. You must NOT use mprotect on MAP_JIT pages. Instead, toggle
    ///    per-THREAD write protection with pthread_jit_write_protect_np().
    ///    This is a hardware feature (APRR) and is ~free.
    /// 3. You MUST call sys_icache_invalidate(). On Apple Silicon the
    ///    instruction cache is NOT coherent with the data cache, so freshly
    ///    written bytes may not be visible to the fetch unit. Skipping this
    ///    produces intermittent, unreproducible wrong behavior — the worst
    ///    possible bug class.
    pub fn new(size: usize) -> io::Result<Self> {
        let ptr = unsafe {
            libc::mmap(ptr::null_mut(), len,
                       libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                       libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_JIT, -1, 0)
        };
        // …
    }

    pub fn write<F: FnOnce(&mut [u8])>(&mut self, f: F) {
        unsafe { pthread_jit_write_protect_np(0); }   // this THREAD may write
        f(unsafe { slice::from_raw_parts_mut(self.ptr, self.len) });
        unsafe { pthread_jit_write_protect_np(1); }   // back to executable
        unsafe { sys_icache_invalidate(self.ptr as _, self.len); }  // MANDATORY
    }
}
```

### Calling into generated code

```rust
pub type JitFn1 = unsafe extern "C" fn(f64) -> f64;
pub type JitFn2 = unsafe extern "C" fn(f64, f64) -> f64;
pub type JitFnN = unsafe extern "C" fn(*const f64) -> f64;
pub type JitFnVec = unsafe extern "C" fn(*const f64, *const f64, *mut f64, usize);

impl CompiledExpr {
    /// The single most unsafe operation in the project, isolated to one place
    /// behind a checked API.
    ///
    /// SAFETY: the buffer must be in Executable state, must contain a complete
    /// function with a correct prologue/epilogue, and the arity must match the
    /// signature we compiled for. All three are enforced by construction:
    /// `arity` is recorded at compile time, and `state` is a type-level flag.
    pub fn call1(&self, x: f64) -> f64 {
        assert_eq!(self.arity, 1, "arity mismatch");
        debug_assert_eq!(self.buf.state, ProtState::Executable);
        let f: JitFn1 = unsafe { mem::transmute(self.buf.as_ptr()) };
        unsafe { f(x) }
    }
}
```

`call1`/`call2` above are `f64`-only conveniences for the common case. The
general entry point, used for functions with mixed `f64`/`i64`/`bool`
parameters, is `call(&self, args: &[RtValue]) -> RtValue` (§3, "Runtime
value representation") — it marshals each `RtValue` into the right register
class (GPR for `i64`/`bool`, XMM for `f64`) per the ABI table in §7.

---

## §12 SIMD Vectorization

Array mode is where the JIT stops being a curiosity and starts being 8× faster than anything you'd write by hand.

```rust
/// Runtime feature detection — you cannot compile AVX-512 into a binary that
/// must run on older CPUs, so the JIT picks its width when it compiles.
/// This is a genuine advantage of JIT over AOT and worth demonstrating.
pub struct CpuFeatures {
    pub sse2: bool, pub sse41: bool,
    pub avx: bool, pub avx2: bool, pub fma: bool,
    pub avx512f: bool, pub avx512dq: bool,
    pub bmi2: bool,
    pub neon: bool,          // AArch64
}

impl CpuFeatures {
    pub fn best_width(&self, ty: Ty) -> u8 {
        match ty {
            Ty::F64 if self.avx512f => 8,    // zmm: 8 × f64
            Ty::F64 if self.avx2    => 4,    // ymm: 4 × f64
            Ty::F64 if self.sse2    => 2,    // xmm: 2 × f64
            Ty::F64                 => 1,
            // …
        }
    }
}
```

### Vectorizer

```rust
/// The expression is already a pure dataflow DAG over element i, so
/// vectorization is a straight rewrite: every scalar op becomes a lane-wise
/// op, loads become vector loads, and the loop is unrolled by the vector width.
///
/// Three things must be handled:
///   1. TAIL: N % width leftover elements. Either a scalar epilogue, or a
///      masked store (AVX-512 k-registers make this free).
///   2. ALIGNMENT: unaligned loads (vmovupd) are nearly free on modern CPUs,
///      so we don't require alignment — but we do emit vmovapd when we can
///      prove 32/64-byte alignment, and the workbench shows the difference.
///   3. REDUCTIONS: a sum across lanes needs a horizontal reduce at the end
///      (vhaddpd chain, or vextractf128 + vaddpd for AVX2).
pub fn vectorize(f: &Function, width: u8) -> Function { /* … */ }
```

Expected speedups on `a[i]*b[i] + c[i]` over 1M elements:

| Mode | Time | Speedup |
|---|---|---|
| Interpreter | 4200 ms | 1× |
| Scalar JIT | 3.1 ms | 1350× |
| SSE2 (2-wide) | 1.7 ms | 2470× |
| AVX2 + FMA (4-wide) | 0.62 ms | 6770× |
| AVX-512 (8-wide) | 0.38 ms | 11000× |

The interpreter→JIT jump is the big one; the SIMD widths then show near-linear scaling until memory bandwidth saturates — which is itself the lesson, because 8-wide is not 2× faster than 4-wide.

---

## §13 Tiered Compilation

```rust
pub enum Tier {
    Interpreter,     // instant start, ~200 ns/eval
    Baseline,        // no optimizer, ~10 µs to compile, ~8 ns/eval
    Optimized,       // full pipeline + SIMD, ~80 µs to compile, ~1.5 ns/eval
}

pub struct TieredExpr {
    ir: Function,
    tier: AtomicU8,
    invocations: AtomicU64,
    baseline: OnceCell<CompiledExpr>,
    optimized: OnceCell<CompiledExpr>,
}

const BASELINE_THRESHOLD:  u64 = 10;
const OPTIMIZED_THRESHOLD: u64 = 1_000;

impl TieredExpr {
    /// Exactly the structure every real VM uses. The interesting part is the
    /// break-even math: compiling costs ~80 µs, and the optimized version saves
    /// ~198 ns per call over the interpreter, so tier-2 pays for itself after
    /// ~400 calls. Below that, compiling is a LOSS — which is why tiering
    /// exists at all and why the workbench charts the crossover.
    pub fn eval(&self, args: &[f64]) -> f64 {
        let n = self.invocations.fetch_add(1, Ordering::Relaxed);

        if n == OPTIMIZED_THRESHOLD { self.compile_optimized(); }
        else if n == BASELINE_THRESHOLD { self.compile_baseline(); }

        match self.tier.load(Ordering::Acquire) {
            2 => self.optimized.get().unwrap().call(args),
            1 => self.baseline.get().unwrap().call(args),
            _ => self.interpret(args),
        }
    }
}
```

---

## §14 Verification

A JIT that produces wrong answers is worse than no JIT, because the failure is silent and data-dependent.

### 14.1 Differential testing

```rust
/// THE core test. Generate random expressions and random inputs; the JIT and
/// the interpreter must agree bit-for-bit (with an explicit epsilon only for
/// fast-math mode, where divergence is expected and bounded).
///
/// Shown here for the all-f64 subset for brevity; the real generator produces
/// `Vec<RtValue>` matching each generated expression's actual `f.params`
/// types, so i64/bool parameters get exercised too, not just f64.
proptest! {
    #[test]
    fn jit_matches_interpreter(
        expr in arb_expression(depth: 1..8),
        inputs in prop::array::uniform8(any::<f64>()),
    ) {
        let interpreted = interpret(&expr, &inputs);
        let jitted = compile(&expr, OptLevel::Full).call(&inputs);

        if interpreted.is_nan() {
            prop_assert!(jitted.is_nan());
        } else {
            // Bit-exact. NOT approximately equal — without fast-math the JIT
            // must produce IDENTICAL results, and any drift is a real bug.
            prop_assert_eq!(interpreted.to_bits(), jitted.to_bits());
        }
    }
}
```

### 14.2 Optimization-level equivalence

Every expression must produce identical results at `-O0`, `-O1`, `-O2` (fast-math excepted). A CSE bug or a bad algebraic rule shows up here immediately.

### 14.3 Encoder round-trip

Every emitted instruction disassembles (via `iced-x86` / `capstone`) to the mnemonic we intended.

### 14.4 Cross-backend agreement

Same expression compiled to x86-64, AArch64 (under QEMU), and WASM must produce the same result.

---

## §15 Frontend — The Workbench

Stack: `react` · `vite` · `typescript` · `@codemirror/*` · `tailwindcss` · `shadcn/ui` · `d3` · `recharts` · `zustand`

### Panel 1 · Expression Editor
CodeMirror 6 with the custom expression language, live error squiggles, variable binding panel (set `x = 3.0` etc.), and mode toggle: **scalar** / **array (SIMD)**.

### Panel 2 · AST → SSA IR ⭐
Split view. Left: the AST as a D3 tree. Right: the SSA IR in textual form:

```
  v0 = param 0            ; x
  v1 = param 1            ; y
  v2 = mul v0, v0
  v3 = mul v1, v1
  v4 = add v2, v3
  v5 = sqrt v4
       ret v5
```

Hovering an IR value highlights the AST node *and* the source span. This three-way linking is what makes "the compiler lowered my expression into this" click.

### Panel 3 · Optimization Pipeline ⭐
A stepper through every pass. For each pass:
- IR before and after, with a **diff** (removed lines red, added green)
- Which rule fired, with its name and validity annotation
- Instruction count and dependency-chain depth before/after

Watching `x*x + 2*x + 1` go through reassociation and CSE, then FMA contraction, in five discrete steps, is the clearest possible demonstration of what an optimizer does.

### Panel 4 · Control Flow Graph
D3 + dagre. Blocks as nodes with their instruction lists; edges labeled with branch conditions. φ-nodes highlighted with edges back to their incoming blocks. Only interesting for `if`-containing expressions, which is exactly why the language has `if`.

### Panel 5 · Register Allocation ⭐
The panel that makes register allocation comprehensible.

- **Live interval chart**: X-axis is instruction index, one horizontal bar per SSA value spanning `[start, end)`, colored by assigned physical register. Overlapping bars in the same color are impossible by construction — and *seeing* that constraint is the whole idea.
- **Spill markers**: values that got spilled shown with a hatched pattern, with the spill/reload instructions marked on the axis.
- **Register pressure curve** overlaid: how many values are live at each point, with a red line at the register count. Where the curve crosses the line is exactly where spills happen.

### Panel 6 · Assembly + Hex ⭐
Three synchronized columns:

```
  offset    bytes                        assembly
  0000      55                           push rbp
  0001      48 89 E5                     mov  rbp, rsp
  0004      C5 FB 59 C0                  vmulsd xmm0, xmm0, xmm0
  0008      C5 F3 59 C9                  vmulsd xmm1, xmm1, xmm1
  000C      C5 FB 58 C1                  vaddsd xmm0, xmm0, xmm1
  0010      C5 FB 51 C0                  vsqrtsd xmm0, xmm0, xmm0
  0014      5D                           pop  rbp
  0015      C3                           ret
```

Hovering a byte breaks down its role: **VEX prefix** / opcode / ModRM (with mod, reg, rm decoded) / displacement / immediate. Clicking an instruction highlights the IR value it came from.

**This panel is the payoff of the entire project.** Most people have never seen the actual bytes their code becomes, annotated field by field.

### Panel 7 · Benchmark & Tiering ⭐
- Bar chart: interpreter vs baseline JIT vs optimized JIT vs SIMD widths
- Line chart: time vs input size, showing where SIMD saturates memory bandwidth
- **Tier-up timeline**: invocations on X, current tier as a step function, with compile-time spikes marked. Shows the break-even point where compiling starts paying off.
- Compile time vs execution time breakdown

### Panel 8 · CPU Target Selector
Toggle target features (SSE2 / AVX2 / FMA / AVX-512 / NEON) and target ISA (x86-64 / AArch64 / WASM), and watch Panels 5–7 regenerate. Same expression, radically different code. Includes a **side-by-side x86 vs AArch64** view of the same function, which makes the CISC/RISC difference visceral: 21 bytes of variable-length x86 vs 8 fixed 32-bit AArch64 words.

---

## §16 CLI

```
forge eval "sqrt(x*x + y*y)" --x 3 --y 4
forge compile EXPR --arch x86_64|aarch64|wasm --opt 0|1|2 --features avx2,fma
forge asm EXPR                  # annotated assembly + hex
forge ir EXPR [--after PASS]    # SSA IR, optionally after a specific pass
forge cfg EXPR --dot            # graphviz CFG
forge regalloc EXPR             # live intervals + assignments + spills
forge bench EXPR [--sizes 1,10,100,1K,1M]
forge verify EXPR --iters 100000     # differential vs interpreter
forge cpuinfo                   # detected features
forge repl
```

`forge asm "x*x + y*y"` printing annotated bytes is the single most satisfying command in the project.

---

## §17 File Structure

```
forge/
├── crates/
│   ├── forge-syntax/       # lexer, Pratt parser, AST, type check
│   ├── forge-ir/           # SSA IR, blocks, φ, dominance, verifier
│   ├── forge-opt/          # fold, simplify, strength-reduce, GVN, DCE, LICM, reassoc
│   ├── forge-regalloc/     # live intervals, linear scan, spilling, ABI constraints
│   ├── forge-x64/          # REX/ModRM/SIB/VEX/EVEX encoder, instruction selection
│   ├── forge-aarch64/      # fixed-width encoder, immediate encoding
│   ├── forge-wasm/         # WASM bytes backend
│   ├── forge-mem/          # ExecutableBuffer, W^X, MAP_JIT, icache
│   ├── forge-runtime/      # tiering, invocation counters, libm thunks
│   ├── forge-simd/         # feature detection, vectorizer, tails
│   ├── forge-bench/        # criterion harness, perf counters
│   ├── forge-cli/          # clap CLI + REPL
│   └── forge-wasm-api/     # wasm-bindgen surface for the workbench
├── workbench/              # React app
│   └── src/components/{editor,ir,passes,cfg,regalloc,asm,bench,target}/
├── tests/
│   ├── differential/       # JIT vs interpreter, proptest
│   ├── encoding/           # round-trip via iced-x86 / capstone
│   ├── golden/             # expression → expected hex bytes
│   └── cross_arch/         # x86 vs aarch64 (QEMU) vs wasm
└── benches/
```

---

## §18 Correctness Properties

1. **Semantic equivalence.** For every expression and every input, the JIT produces bit-identical results to the interpreter (fast-math excepted, where divergence is bounded and documented).
2. **Optimization safety.** `-O0`, `-O1`, `-O2` all produce identical results without fast-math.
3. **Encoding correctness.** Every emitted instruction disassembles to exactly the mnemonic and operands intended.
4. **Cross-architecture agreement.** x86-64, AArch64, and WASM produce identical results.
5. **Register allocation soundness.** No two values live at the same point are assigned the same register. Verified by an independent checker, not by the allocator itself.
6. **ABI compliance.** Generated functions are callable from C, preserve all callee-saved registers, maintain 16-byte stack alignment at every `call`, and honor Win64 shadow space.
7. **W^X maintained.** No page is ever simultaneously writable and executable, on any platform.
8. **Instruction cache coherency.** `sys_icache_invalidate` (AArch64) is called before any generated code is executed.
9. **SSA validity.** Every value is defined exactly once; every use is dominated by its definition. Checked by a verifier after every pass.
10. **Vectorization equivalence.** SIMD results match scalar results element-for-element, including the tail.
11. **No memory leaks.** Every `ExecutableBuffer` is `munmap`ped. As of Phase 5: verified empirically via `getrusage(RUSAGE_SELF).ru_maxrss` high-water-mark growth staying flat across 10,000 allocate/free cycles (`crates/forge-mem/tests/no_leaks.rs`) — **not** under Miri or valgrind. Miri cannot model raw `mmap`/`mprotect`/`sysconf` syscalls or a transmute-to-function-pointer call, so it's an explicit non-goal for this crate (see `forge-mem`'s crate-level doc comment); valgrind was never run either (no macOS/Apple Silicon support to exercise it on this project's development machine) and that gap isn't yet covered by an alternative on Linux.
12. **Tiering transparency.** Results are identical regardless of which tier serviced the call.

---

## §19 Performance Targets

| Metric | Target |
|---|---|
| Compile time, simple expression, `-O0` | < 10 µs |
| Compile time, complex expression, `-O2` | < 100 µs |
| Interpreter eval (`sqrt(x*x+y*y)`) | ~200 ns |
| Baseline JIT eval | < 10 ns |
| Optimized JIT eval | < 3 ns |
| **Speedup, interpreter → optimized** | **> 60×** |
| Array eval, 1M elems, scalar JIT | < 4 ms |
| Array eval, 1M elems, AVX2+FMA | < 0.8 ms |
| Array eval, 1M elems, AVX-512 | < 0.5 ms |
| Register allocation, 1000 values | < 50 µs (linear scan) |
| Encoder throughput | > 100 MB/s of machine code |
| Generated code size, `sqrt(x*x+y*y)` | ≤ 24 bytes (x86-64) |
| WASM workbench bundle (gzipped) | < 1.1 MB |
