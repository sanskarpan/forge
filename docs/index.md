# Forge — An Auditable JIT Compiler

![Forge compilation pipeline](assets/forge-pipeline.gif)

Forge is a compact compiler for typed mathematical expressions. It is built
from first principles so the path from source text to executable bytes is
inspectable:

```text
source → typed SSA → verified optimization → allocation → handwritten bytes → W^X JIT
```

The [architecture guide](ARCHITECTURE.md) explains the components and their
contracts. Start with [testing and verification](TESTING.md) if you are
reviewing correctness, or [platforms and W^X](PLATFORMS.md) if you are
working near executable memory.

## Status

Forge is pre-1.0 and educational in scope. The current implementation table in
the [checklist](../CHECKLIST.md) is the source of truth for supported
behavior. Unsupported operations fail clearly or use the interpreter oracle;
the project does not pretend that a fallback is native execution.

## Explore

- [Architecture](ARCHITECTURE.md) — pipeline, crate ownership, and invariants
- [Testing](TESTING.md) — local gates, differential testing, and CI matrix
- [Release compatibility](RELEASE.md) — targets, API stability, and versioning
- [Encoding](ENCODING.md) — handwritten instruction encoding methodology
- [Register allocation](REGALLOC.md) — liveness, spills, and verification
- [Optimization](OPTIMIZATION.md) — semantics-preserving transformations
- [Platforms](PLATFORMS.md) — W^X, JIT permissions, cache coherence, and ABIs

Source, issues, and pull requests are hosted on
[GitHub](https://github.com/sanskarpan/forge). Contributions should follow
[CONTRIBUTING.md](../CONTRIBUTING.md), and security reports should follow
[SECURITY.md](../SECURITY.md).
