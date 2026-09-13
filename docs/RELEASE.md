# Release and compatibility

Forge is currently pre-1.0. The public surface is useful for experiments and
the Workbench, but native ABI and target support can evolve between releases.

## Versioning

- Rust crates share the workspace version in the root `Cargo.toml`.
- User-visible changes are recorded in [CHANGELOG.md](../CHANGELOG.md).
- A release should be cut only from `main` after the full required CI matrix is
  green.
- Changes to source syntax, `RtValue`, `ExternalFunction`, artifact JSON, or
  target support require a specification and checklist update.

## Supported execution modes

| Mode | Behavior |
| --- | --- |
| Native x86-64 | Executes supported emitted functions with the host ABI |
| Native AArch64 | Executes the supported scalar AAPCS64 subset |
| Portable runtime | Uses the verified interpreter when native execution is unavailable |
| WASM | Emits and executes typed portable artifacts in browser/Node hosts |
| Workbench native targets | Serializes inspection artifacts; never executes native bytes in the browser |

## Compatibility expectations

The interpreter defines semantic behavior. Generated code must match it for
supported native shapes, including signed zero and integer wrapping. Unsupported
shapes should return a stable error or use the documented interpreter fallback.
They must not be silently treated as native execution.

Registered external functions are native-only and caller-owned. The caller is
responsible for keeping the function address live and supplying the exact
scalar C-ABI signature declared to Forge. Portable artifacts reject raw
addresses by design.

## Release checklist

```sh
cargo test --workspace --offline --locked
cargo clippy --workspace --all-targets --offline --locked -- -D warnings
cargo fmt --all -- --check
npm ci --prefix workbench
npm test --prefix workbench
npm run build --prefix workbench
```

Then verify the GitHub Actions CI, container validation, documentation build,
and Pages deployment for the exact release commit. Update the changelog and
tag only after those checks are complete.
