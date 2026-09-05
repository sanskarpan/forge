# Forge workbench

This repository ships a dependency-free browser shell so it can be served and
smoke-tested without downloading a JavaScript dependency tree. A WASM build
should expose `window.forgeWasm` with `compile_wasm(source)` (or `compile`),
`compile_artifact_json(source)`, and `parse_and_check(source)`. The shell then
instantiates the emitted module, reports the result, and renders:

- source diagnostics with primary spans and a checked AST;
- lowered and optimized textual IR plus a CFG DOT representation;
- WASM signature metadata and raw bytes;
- browser timing samples for repeated calls to the compiled export.

`run_wasm(source, args)` (or `run`) remains a portable interpreter fallback
when a bundle cannot emit a module. Start it with `npm run dev` and open
`http://localhost:4173`.
