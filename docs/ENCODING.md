# Encoding and emission

The x86-64 backend is split into three auditable layers:

1. `forge-x64` represents physical registers, labels, fixups, and individual
   hand-written encoders.
2. `forge-regalloc` maps SSA values to registers or eight-byte spill slots.
3. `forge-emit` translates selected virtual instructions into encoder calls,
   inserts reload/store traffic, lays out frames and constant pools, and
   returns the final bytes.

`iced-x86` is a test oracle only. It disassembles bytes produced by the
project; it is never used to generate them.

## x86 instruction shape

Most register instructions consist of optional prefixes, an opcode, and a
ModRM byte. The ModRM byte is `mod:2 | reg:3 | rm:3`; extended registers add
REX.R/B bits. A memory operand may need a SIB byte: `rm=100` means SIB follows
for RSP/R12, and a zero-displacement RBP/R13 base requires a displacement
byte.

64-bit integer operations always set REX.W. Any REX prefix also changes the
legacy high-byte register names, so the encoder emits REX deliberately rather
than treating it as a cosmetic prefix.

The assembler selects an eight-bit displacement when it fits and otherwise a
32-bit displacement. Backward conditional/unconditional jumps use a short
form when the known distance fits; forward jumps use rel32 fixups because
their final distance is not known until the label is bound.

## Two-address lowering

x86 scalar arithmetic overwrites its destination. The selected instruction
`dst = lhs op rhs` therefore becomes `mov dst, lhs; op dst, rhs` unless
allocation coalesces `dst` with `lhs`. The emitter handles the non-coalesced
case explicitly, including XMM and integer register classes.

Floating constants and sign masks live in a deduplicated pool after the code.
RIP-relative fixups are patched once the pool is placed. Calls to libm are
indirect and preserve live caller-saved registers around ABI marshaling.

## Verification

Every encoder family has golden-byte and disassembly tests under
`crates/forge-x64/tests/round_trip.rs`. Emitter tests additionally inspect
frame layout, control-flow fixups, spill reloads, and ABI return placement.
On an ARM development host these byte-level tests still run; x86 execution
tests are target-gated and require an x86-64 runner.
