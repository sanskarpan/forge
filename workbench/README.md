# Forge workbench

The workbench is a React/Vite application that runs the real compiler boundary
when a `forge-wasm-api` web bundle is loaded as `globalThis.forgeWasm`. It is
deliberately artifact-driven: one debounced compile updates every panel from
the same response, so the AST, IR, CFG, bytes, and benchmark cannot silently
describe different source revisions.

The UI includes:

- CodeMirror source editing with a 200 ms debounce and scalar/array mode;
- source diagnostics, checked AST spans, and a D3 AST tree;
- lowered/optimized textual IR with a selectable stepper and side-by-side diff;
- a dagre-laid-out CFG with its DOT source;
- register intervals/pressure metadata when a native artifact supplies it;
- synchronized raw bytes and native assembly when available, with an explicit
  WASM stack-machine state otherwise;
- a Recharts benchmark view and tier/backend label;
- x86-64, AArch64, WASM, and array-mode selectors with honest unavailable
  states for targets not exposed by the current browser API.

Install and run it locally:

```sh
npm ci
npm test
npm run build
npm run dev
```

Then open `http://localhost:5173`. Without a generated wasm-bindgen bundle the
application still renders the complete observatory and reports the missing API
as a visible status; it never invents native artifacts. The CI container runs
all three commands, including the production build.
