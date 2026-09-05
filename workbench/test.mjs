import { readFile } from 'node:fs/promises';

const html = await readFile(new URL('./index.html', import.meta.url), 'utf8');
for (const marker of ['forgeWasm', 'compile_wasm', 'compile_artifact_json', 'parse_and_check', 'WebAssembly.instantiate', 'benchmarkModule', 'benchmark', 'ast', 'diagnostics', 'ir_stages', 'cfg', 'source', 'args', 'Run']) {
  if (!html.includes(marker)) throw new Error(`workbench missing ${marker}`);
}
console.log('workbench smoke test: ok');
