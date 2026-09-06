import { AstNode, BenchmarkResult, CompileArtifact, Diagnostic, Target, useWorkbench } from './store';

export interface ForgeWasmApi {
  parse_and_check?: (source: string) => string | Promise<string>;
  compile_artifact_json?: (source: string) => string | Promise<string>;
  compile_target_artifact_json?: (source: string, target: string) => string | Promise<string>;
  compile_wasm?: (source: string) => Uint8Array | number[] | Promise<Uint8Array | number[]>;
  benchmark?: (source: string, sizes: Uint32Array) => string | Promise<string>;
  run_wasm?: (source: string, args: number[]) => number | Promise<number>;
}

declare global {
  var forgeWasm: ForgeWasmApi | undefined;
}

const sizes = new Uint32Array([1, 10, 100, 1_000, 10_000]);
let compileGeneration = 0;

function api(): ForgeWasmApi | undefined {
  return globalThis.forgeWasm;
}

async function jsonCall<T>(call: (() => string | Promise<string>) | undefined): Promise<T> {
  if (!call) throw new Error('forge-wasm-api bundle does not expose this operation');
  return JSON.parse(await call());
}

export function parseArguments(raw: string): number[] {
  if (!raw.trim()) return [];
  return raw.split(',').map((value) => {
    const parsed = Number(value.trim());
    if (!Number.isFinite(parsed) && value.trim() !== 'NaN' && value.trim() !== 'Infinity' && value.trim() !== '-Infinity') {
      throw new Error(`invalid numeric argument: ${value.trim()}`);
    }
    return parsed;
  });
}

export function hexBytes(hex: string): Uint8Array {
  const tokens = hex.trim() ? hex.trim().split(/\s+/) : [];
  return Uint8Array.from(tokens.map((token) => Number.parseInt(token, 16)));
}

export function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join(' ');
}

function diagnosticsFrom(value: unknown): Diagnostic[] {
  if (!value || typeof value !== 'object') return [{ message: 'invalid diagnostics response' }];
  const diagnostics = (value as { diagnostics?: unknown }).diagnostics;
  return Array.isArray(diagnostics) ? diagnostics as Diagnostic[] : [{ message: 'source rejected by the compiler' }];
}

async function browserBenchmark(evaluate: (...args: number[]) => number, args: number[]) {
  const results = Array.from(sizes, (size) => {
    const started = performance.now();
    let last = Number.NaN;
    for (let index = 0; index < size; index += 1) last = evaluate(...args);
    return { size, calls: size, elapsed_ms: performance.now() - started, last_result: last };
  });
  return { ok: true, backend: 'browser-wasm-export', results };
}

export async function compileCurrent(source: string, target: Target): Promise<void> {
  const generation = ++compileGeneration;
  const state = useWorkbench.getState();
  const commit = (value: Parameters<typeof state.setCompilation>[0]) => {
    if (generation === compileGeneration) state.setCompilation(value);
  };
  commit({ compiling: true, status: 'Parsing and compiling…', error: null, diagnostics: [] });
  const wasm = api();
  if (!wasm) {
    commit({ compiling: false, status: 'No forge-wasm-api bundle loaded.', error: 'Load the generated wasm-bindgen web bundle as window.forgeWasm.' });
    return;
  }

  try {
    const checked = await jsonCall<{ ok: boolean; stage?: string; ast?: AstNode; parameters?: Array<{ name: string; type: string }>; result_type?: string; diagnostics?: Diagnostic[] }>(() => wasm.parse_and_check!(source));
    if (!checked.ok) {
      commit({
        compiling: false,
        status: `Source ${checked.stage ?? 'validation'} failed.`,
        diagnostics: diagnosticsFrom(checked),
        ast: null,
        artifact: null,
        benchmark: null,
        resultType: null,
        parameters: [],
      });
      return;
    }

    commit({
      ast: checked.ast ?? null,
      parameters: checked.parameters ?? [],
      resultType: checked.result_type ?? null,
      status: `Compiling ${target} artifact…`,
    });

    const artifactCall = wasm.compile_target_artifact_json
      ? () => wasm.compile_target_artifact_json!(source, target)
      : target === 'wasm' && wasm.compile_artifact_json
        ? () => wasm.compile_artifact_json!(source)
        : undefined;
    if (!artifactCall) {
      commit({ compiling: false, artifact: null, benchmark: null, status: `${target} artifacts are not exposed by the current API.` });
      return;
    }
    const artifact = await jsonCall<CompileArtifact>(artifactCall);
    if (!artifact.ok) throw new Error((artifact as unknown as { error?: string }).error ?? 'artifact compilation failed');
    commit({ artifact, compiling: false, status: 'Compiled successfully. WASM export is ready.' });

    if (target !== 'wasm') {
      commit({ status: `Compiled ${target} inspection artifact successfully. Native bytes are not executed in the browser.`, tier: `inspect ${target}` });
      return;
    }

    const args = parseArguments(useWorkbench.getState().args);
    const wasmHex = artifact.wasm_bytes_hex ?? artifact.bytes_hex;
    if (!wasmHex) throw new Error('WASM artifact did not include executable bytes');
    const bytes = hexBytes(wasmHex);
    const instance = await WebAssembly.instantiate(bytes, {});
    const evaluate = instance.exports.eval;
    if (typeof evaluate !== 'function') throw new Error('compiled module does not export eval');
    const invoke = (...values: number[]) => Number((evaluate as (...values: number[]) => number)(...values));
    const result = invoke(...args);
    commit({ status: `Compiled successfully. Result: ${String(result)}`, tier: 'baseline WASM' });
    const benchmark = wasm.benchmark
      ? await jsonCall<BenchmarkResult>(() => wasm.benchmark!(source, sizes))
      : await browserBenchmark(invoke, args);
    commit({ benchmark });
  } catch (error) {
    commit({
      compiling: false,
      status: 'Compilation or execution failed.',
      error: error instanceof Error ? error.message : String(error),
      artifact: null,
      benchmark: null,
    });
  }
}
