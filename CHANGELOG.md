# Changelog

Forge follows a lightweight Keep a Changelog format. The repository is still
pre-1.0, so public APIs and target support may evolve between releases.

## [Unreleased]

### Added

- Production repository documentation, security guidance, contribution
  workflow, and an mdBook documentation site deployed through GitHub Pages.
- Defensive SSA verification for malformed, duplicated, mistyped, and
  same-block out-of-order IR.
- A deterministic compiler-pipeline demonstration asset for the README and
  documentation site.

### Verified

- Workspace tests, Clippy with warnings denied, rustfmt, native differential
  coverage, emulated ARM64 tests, WASM packaging, Workbench build, Windows
  validation, and Linux Valgrind executable-memory checks.

## 0.1.0 — 2026-09-12

The first documented Forge implementation milestone. It includes the typed
frontend and SSA IR, interpreter oracle, optimizer, native x86-64 and AArch64
backends, W^X executable memory, register allocation, SIMD array evaluation,
typed WASM artifacts, runtime tiering, CLI inspection tools, and the React
Workbench. See [CHECKLIST.md](CHECKLIST.md) for the evidence-backed feature
matrix and historical phase record.
