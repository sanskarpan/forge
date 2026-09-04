# Forge workbench

This repository currently ships a dependency-free browser shell so it can be
served and smoke-tested without downloading a JavaScript dependency tree. A
WASM build should expose `window.forgeWasm.run(source, args)`; the runner then
passes expressions and f64 arguments to that API and reports the result or
diagnostics. Start it with `npm run dev` and open `http://localhost:4173`.
