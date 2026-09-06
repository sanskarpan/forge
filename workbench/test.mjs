import { readFile } from 'node:fs/promises';

const read = (name) => readFile(new URL(`./${name}`, import.meta.url), 'utf8');
const [html, app, compiler, packageJson] = await Promise.all([
  read('index.html'),
  read('src/App.tsx'),
  read('src/compiler.ts'),
  read('package.json'),
]);

if (!packageJson.includes('"build"')) throw new Error('workbench build script is missing');

for (const marker of ['id="root"', 'src="/src/main.tsx"']) {
  if (!html.includes(marker)) throw new Error(`workbench shell missing ${marker}`);
}
for (const marker of ['EditorView', 'AstPanel', 'IrPanel', 'CfgPanel', 'IntervalPanel', 'AssemblyPanel', 'BenchmarkPanel', 'TargetPanel', 'ResponsiveContainer']) {
  if (!app.includes(marker)) throw new Error(`workbench UI missing ${marker}`);
}
for (const marker of ['parse_and_check', 'compile_artifact_json', 'compile_target_artifact_json', 'WebAssembly.instantiate', 'benchmark', 'globalThis.forgeWasm']) {
  if (!compiler.includes(marker)) throw new Error(`workbench adapter missing ${marker}`);
}
const manifest = JSON.parse(packageJson);
for (const script of ['test', 'build', 'dev']) {
  if (!manifest.scripts?.[script]) throw new Error(`workbench manifest missing ${script} script`);
}
console.log('workbench React smoke test: ok');
