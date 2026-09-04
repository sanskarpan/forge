# Register allocation

`forge-regalloc` uses a linear scan over the selected machine-instruction
stream. Each SSA value receives an inclusive interval `[start, end]`, a
register class (`Gpr` or `Xmm`), an optional coalescing hint, and a spill
weight. Intervals are sorted deterministically by start, end, and value ID.

At each interval start, expired active intervals release their locations. A
free register is chosen from the class-specific pool, with a hint preferred
when it is still usable. If the pool is full, the allocator spills the
lowest-weight active victim when doing so is profitable; otherwise the new
interval is spilled. Spill slots are reused only by genuinely non-overlapping
intervals and are eight bytes each.

The selected stream has point constraints that are not whole-lifetime pins:

- ABI parameters are copied from their incoming System V argument register.
- division/remainder use RAX/RDX at the instruction boundary.
- variable shifts use RCX/CL at the instruction boundary.

Keeping those as emitter-time copies avoids falsely pinning an entire value
and making otherwise valid programs impossible to allocate. The emitter
reserves three caller-saved scratch registers in each class so an instruction
with two spilled operands and a spilled destination can still be lowered.

## Verification

The independent allocation verifier checks that every interval has a
location, overlapping intervals in one class do not share a register or spill
slot, and every location is encodable. The pressure report is a dense
per-instruction count of simultaneously live GPR and XMM intervals. It is an
upper bound on demand because touching intervals may legally transfer one
physical register at an instruction boundary.

The CLI exposes this data with:

```sh
cargo run -p forge-cli -- regalloc 'sqrt(x*x + y*y)'
```

The workbench can consume the same interval and assignment fields from
`forge_runtime::CompilationArtifacts` without rerunning allocation.
