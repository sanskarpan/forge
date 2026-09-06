import { create } from 'zustand';

export type Target = 'wasm' | 'x86_64' | 'aarch64';
export type Mode = 'scalar' | 'array';

export interface Span {
  start: number;
  end: number;
}

export interface Diagnostic {
  message: string;
  primary?: Span & { message?: string };
  secondary?: Array<Span & { message?: string }>;
}

export interface AstNode {
  kind: string;
  span?: Span;
  [key: string]: unknown;
}

export interface IrStage {
  name: string;
  text: string;
}

export interface Interval {
  value?: string;
  start: number;
  end: number;
  location?: string;
  class?: string;
}

export interface AssemblyInstruction {
  offset?: number;
  bytes?: string;
  text?: string;
  value?: string;
}

export interface CompileArtifact {
  ok: true;
  target?: string;
  parameter_types: string[];
  result_type: string;
  wasm_bytes_hex?: string;
  wasm_bytes_len?: number;
  bytes_hex?: string;
  bytes_len?: number;
  ir_stages: IrStage[];
  cfg: string;
  intervals?: Interval[];
  asm?: AssemblyInstruction[];
  encoding?: string;
}

export interface BenchmarkResult {
  ok: boolean;
  backend?: string;
  results?: Array<{
    size: number;
    calls?: number;
    elapsed_ms?: number;
    last_result?: number | null;
    error?: string;
  }>;
  error?: string;
}

interface WorkbenchState {
  source: string;
  args: string;
  target: Target;
  mode: Mode;
  activePanel: string;
  compiling: boolean;
  status: string;
  error: string | null;
  diagnostics: Diagnostic[];
  ast: AstNode | null;
  parameters: Array<{ name: string; type: string }>;
  resultType: string | null;
  artifact: CompileArtifact | null;
  benchmark: BenchmarkResult | null;
  tier: string;
  setSource: (source: string) => void;
  setArgs: (args: string) => void;
  setTarget: (target: Target) => void;
  setMode: (mode: Mode) => void;
  setActivePanel: (panel: string) => void;
  setCompilation: (value: Partial<WorkbenchState>) => void;
}

export const useWorkbench = create<WorkbenchState>((set) => ({
  source: 'sqrt(x * x + y * y)',
  args: '3, 4',
  target: 'wasm',
  mode: 'scalar',
  activePanel: 'overview',
  compiling: false,
  status: 'Ready. Load a forge-wasm-api bundle to compile in the browser.',
  error: null,
  diagnostics: [],
  ast: null,
  parameters: [],
  resultType: null,
  artifact: null,
  benchmark: null,
  tier: 'interpreter',
  setSource: (source) => set({ source }),
  setArgs: (args) => set({ args }),
  setTarget: (target) => set({ target }),
  setMode: (mode) => set({ mode }),
  setActivePanel: (activePanel) => set({ activePanel }),
  setCompilation: (value) => set(value),
}));
