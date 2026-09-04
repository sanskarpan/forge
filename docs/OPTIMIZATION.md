# Optimization semantics

The optimizer runs a fixed-point pipeline, capped at ten rounds. In debug
builds the IR verifier runs after every pass. The current order is constant
folding, algebraic simplification, shift strength reduction, GVN/CSE,
integer reassociation, and dead-code elimination.

Floating-point transformations are conservative. Constant arithmetic is
folded using the host IEEE-754 operations, but rules such as `x * 0 → 0`,
`x - x → 0`, and `x / x → 1` are restricted to integers because NaN,
infinity, and signed zero make those rewrites observably wrong for `f64`.
Integer overflow is wrapping, matching the interpreter and machine ALU.

Commutative operands are canonicalized before GVN, so `a + b` and `b + a`
share a value when they occur in a dominating region. Reassociation is
restricted to integer chains because changing floating-point grouping changes
rounding. Strength reduction handles power-of-two integer shifts with signed
rounding fixups and bitwise remainders; magic-division arithmetic is tested
but is not currently wired because the IR has no high-half multiply operation.

## Correctness contract

The interpreter in `forge-ir` is the reference implementation. Differential
tests compare optimized and unoptimized results, including NaN propagation,
signed zero, infinities, subnormals, and integer boundary values. A rewrite
must either preserve those bits or be explicitly marked as a future
fast-math-only transformation.

Inspect optimized IR with:

```sh
cargo run -p forge-cli -- ir 'x * 1.0 + y * y' --after opt
```
