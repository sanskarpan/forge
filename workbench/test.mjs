import { readFile } from 'node:fs/promises';

const html = await readFile(new URL('./index.html', import.meta.url), 'utf8');
for (const marker of ['forgeWasm', 'source', 'args', 'Run']) {
  if (!html.includes(marker)) throw new Error(`workbench missing ${marker}`);
}
console.log('workbench smoke test: ok');
