import { EditorState } from '@codemirror/state';
import { EditorView } from '@codemirror/view';
import dagre from 'dagre';
import * as d3 from 'd3';
import { useEffect, useMemo, useRef, useState } from 'react';
import { Bar, BarChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';
import { AstNode, CompileArtifact, Diagnostic, Interval, Target, useWorkbench } from './store';
import { compileCurrent, hexBytes } from './compiler';

const targets: Array<{ id: Target; label: string; detail: string }> = [
  { id: 'wasm', label: 'WASM', detail: 'browser-executable stack machine' },
  { id: 'x86_64', label: 'x86-64', detail: 'native bytes when the native API is exposed' },
  { id: 'aarch64', label: 'AArch64', detail: 'AAPCS64 analysis when the native API is exposed' },
];

function Panel({ title, eyebrow, children, wide = false }: { title: string; eyebrow?: string; children: React.ReactNode; wide?: boolean }) {
  return <section className={`panel ${wide ? 'panel-wide' : ''}`}>
    <div className="panel-heading">
      <div><span className="eyebrow">{eyebrow ?? 'compiler artifact'}</span><h2>{title}</h2></div>
      <span className="panel-mark">●</span>
    </div>
    {children}
  </section>;
}

function CodeBlock({ children, className = '' }: { children: React.ReactNode; className?: string }) {
  return <pre className={`code-block ${className}`}>{children}</pre>;
}

function SourceEditor() {
  const source = useWorkbench((state) => state.source);
  const setSource = useWorkbench((state) => state.setSource);
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | undefined>(undefined);
  const sourceListener = useRef(setSource);

  useEffect(() => { sourceListener.current = setSource; }, [setSource]);

  useEffect(() => {
    if (!host.current) return;
    const state = EditorState.create({
      doc: source,
      extensions: [
        EditorView.lineWrapping,
        EditorView.theme({
          '&': { backgroundColor: '#0b111d', color: '#dbe7ff', fontSize: '15px' },
          '.cm-content': { padding: '16px', caretColor: '#7ee787', fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace' },
          '.cm-gutters': { display: 'none' },
          '&.cm-focused': { outline: 'none' },
        }),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) sourceListener.current(update.state.doc.toString());
        }),
      ],
    });
    const editor = new EditorView({ state, parent: host.current });
    view.current = editor;
    return () => { editor.destroy(); view.current = undefined; };
  }, []);

  useEffect(() => {
    const editor = view.current;
    if (!editor || editor.state.doc.toString() === source) return;
    editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: source } });
  }, [source]);

  return <div className="editor-frame" ref={host} aria-label="Forge expression editor" />;
}

function Header() {
  const { target, mode, setTarget, setMode, source, args, compiling, status, setArgs } = useWorkbench((state) => ({
    target: state.target, mode: state.mode, setTarget: state.setTarget, setMode: state.setMode,
    source: state.source, args: state.args, compiling: state.compiling, status: state.status, setArgs: state.setArgs,
  }));
  const [loading, setLoading] = useState(false);
  const compile = async () => { setLoading(true); await compileCurrent(source, target); setLoading(false); };
  return <header className="topbar">
    <div className="brand"><span className="brand-glyph">ƒ</span><div><strong>forge</strong><span>compiler workbench</span></div></div>
    <div className="top-controls">
      <label className="compact-label">target
        <select value={target} onChange={(event) => setTarget(event.target.value as Target)}>
          {targets.map((item) => <option key={item.id} value={item.id}>{item.label}</option>)}
        </select>
      </label>
      <div className="segmented" aria-label="execution mode">
        {(['scalar', 'array'] as const).map((item) => <button key={item} className={mode === item ? 'selected' : ''} onClick={() => setMode(item)}>{item}</button>)}
      </div>
      <button className="run-button" onClick={compile} disabled={loading || compiling}>{loading || compiling ? 'Compiling…' : 'Compile ↗'}</button>
    </div>
    <div className="status-line"><span className={compiling ? 'pulse-dot' : 'status-dot'} />{status}</div>
    <div className="source-row">
      <span className="source-label">EXPRESSION</span><span className="hint">live / 200 ms debounce</span>
      <label className="args-input">args <input value={args} onChange={(event) => setArgs(event.target.value)} aria-label="numeric arguments" /></label>
    </div>
  </header>;
}

function astChildren(node: AstNode): AstNode[] {
  const children: AstNode[] = [];
  for (const [key, value] of Object.entries(node)) {
    if (key === 'span' || key === 'kind') continue;
    if (Array.isArray(value)) children.push(...value.filter((item): item is AstNode => !!item && typeof item === 'object' && 'kind' in item));
    else if (value && typeof value === 'object' && 'kind' in value) children.push(value as AstNode);
  }
  return children;
}

function AstPanel({ ast }: { ast: AstNode | null }) {
  const layout = useMemo(() => {
    if (!ast) return null;
    const root = d3.hierarchy(ast, astChildren);
    return d3.tree<AstNode>().nodeSize([44, 120])(root);
  }, [ast]);
  if (!layout) return <Empty message="Compile a valid expression to inspect the AST." />;
  const nodes = layout.descendants();
  const links = layout.links();
  const minX = Math.min(...nodes.map((node) => node.x));
  const maxX = Math.max(...nodes.map((node) => node.x));
  return <div className="ast-wrap">
    <svg className="ast-svg" viewBox={`0 ${minX - 28} ${Math.max(520, layout.height + 160)} ${maxX - minX + 56}`} role="img" aria-label="AST tree">
      <g transform={`translate(30,${-minX + 28}) rotate(90)`}>
        {links.map((link, index) => <path key={index} className="tree-link" d={`M${link.source.y},${link.source.x} C${(link.source.y + link.target.y) / 2},${link.source.x} ${(link.source.y + link.target.y) / 2},${link.target.x} ${link.target.y},${link.target.x}`} />)}
        {nodes.map((node) => <g key={node.data.span ? `${node.data.span.start}-${node.data.span.end}` : node.depth} transform={`translate(${node.y},${node.x}) rotate(-90)`}>
          <circle r="16" className="tree-node" /><text dy="4" textAnchor="middle" className="tree-text">{node.data.kind.slice(0, 3)}</text>
          <title>{node.data.kind} · {node.data.span ? `${node.data.span.start}:${node.data.span.end}` : 'no span'}</title>
        </g>)}
      </g>
    </svg>
    <CodeBlock>{JSON.stringify(ast, null, 2)}</CodeBlock>
  </div>;
}

function DiffLines({ first, second }: { first: string; second: string }) {
  const left = first.split('\n');
  const right = second.split('\n');
  const length = Math.max(left.length, right.length);
  return <div className="diff-view">{Array.from({ length }, (_, index) => {
    const a = left[index] ?? '';
    const b = right[index] ?? '';
    const same = a === b;
    return <div className="diff-row" key={index}><span className={same ? 'diff-same' : 'diff-old'}>{a ? `${same ? '  ' : '− '}${a}` : ''}</span><span className={same ? 'diff-same' : 'diff-new'}>{b ? `${same ? '  ' : '+ '}${b}` : ''}</span></div>;
  })}</div>;
}

function IrPanel({ artifact }: { artifact: CompileArtifact | null }) {
  const [stage, setStage] = useState(0);
  const stages = artifact?.ir_stages ?? [];
  if (!artifact || stages.length === 0) return <Empty message="IR appears after compilation." />;
  const current = stages[Math.min(stage, stages.length - 1)];
  return <div>
    <div className="stepper">{stages.map((item, index) => <button key={item.name} className={index === stage ? 'step-active' : ''} onClick={() => setStage(index)}><span>{String(index + 1).padStart(2, '0')}</span>{item.name}</button>)}</div>
    <CodeBlock className="ir-block">{current.text}</CodeBlock>
    {stages.length > 1 && <details className="diff-details"><summary>diff: {stages[0].name} → {stages[stages.length - 1].name}</summary><DiffLines first={stages[0].text} second={stages[stages.length - 1].text} /></details>}
  </div>;
}

function CfgPanel({ cfg }: { cfg: string | undefined }) {
  const graph = useMemo(() => {
    const result = new dagre.graphlib.Graph().setGraph({ rankdir: 'LR', nodesep: 28, ranksep: 70 }).setDefaultEdgeLabel(() => ({}));
    for (const match of cfg?.matchAll(/block(\d+) \[label="([^\"]*)/g) ?? []) result.setNode(`block${match[1]}`, { label: match[2].replaceAll('\\n', ' · '), width: 150, height: 42 });
    for (const match of cfg?.matchAll(/block(\d+) -> block(\d+)(?: \[label="([^"]*)")?/g) ?? []) result.setEdge(`block${match[1]}`, `block${match[2]}`, { label: match[3] ?? '' });
    dagre.layout(result);
    return result;
  }, [cfg]);
  if (!cfg) return <Empty message="CFG appears after compilation." />;
  const width = Math.max(600, graph.graph().width + 40);
  const height = Math.max(120, graph.graph().height + 40);
  return <div className="cfg-wrap"><svg viewBox={`0 0 ${width} ${height}`} role="img" aria-label="control flow graph">
    <defs><marker id="arrow" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto"><path d="M0,0 L8,4 L0,8 z" fill="#62d6a7" /></marker></defs>
    {graph.edges().map((edge, index) => { const points = graph.edge(edge).points; return <g key={index}><path className="cfg-edge" markerEnd="url(#arrow)" d={points.map((point, pointIndex) => `${pointIndex ? 'L' : 'M'}${point.x + 20},${point.y + 20}`).join(' ')} /><text className="cfg-edge-label" x={points[Math.floor(points.length / 2)].x + 24} y={points[Math.floor(points.length / 2)].y + 14}>{graph.edge(edge).label}</text></g>; })}
    {graph.nodes().map((id) => { const node = graph.node(id); return <g key={id} transform={`translate(${node.x + 20},${node.y + 20})`}><rect x={-node.width / 2} y={-node.height / 2} width={node.width} height={node.height} rx="8" className="cfg-node" /><text textAnchor="middle" dy="4" className="cfg-text">{node.label}</text></g>; })}
  </svg><details><summary>DOT source</summary><CodeBlock>{cfg}</CodeBlock></details></div>;
}

function IntervalPanel({ intervals }: { intervals?: Interval[] }) {
  if (!intervals?.length) return <Empty message="Native register intervals are unavailable for the WASM stack target." />;
  const max = Math.max(...intervals.map((item) => item.end), 1);
  return <div className="interval-chart"><div className="axis"><span>0</span><span>{Math.round(max / 2)}</span><span>{max}</span></div>{intervals.map((item, index) => <div className="interval-row" key={`${item.value ?? index}-${index}`}><span className="interval-name">{item.value ?? `v${index}`}</span><div className="interval-track"><span className={`interval-bar ${item.location?.startsWith('spill') ? 'spilled' : ''}`} style={{ left: `${item.start / max * 100}%`, width: `${Math.max(1, (item.end - item.start) / max * 100)}%` }} title={`${item.start}..${item.end} · ${item.location ?? 'unassigned'}`} /></div><span className="interval-location">{item.location ?? '—'}</span></div>)}</div>;
}

function AssemblyPanel({ artifact }: { artifact: CompileArtifact | null }) {
  const bytes = artifact ? hexBytes(artifact.wasm_bytes_hex) : new Uint8Array();
  const rows = Array.from({ length: Math.ceil(bytes.length / 12) }, (_, index) => bytes.slice(index * 12, index * 12 + 12));
  if (!artifact) return <Empty message="Assembly and bytes appear after compilation." />;
  return <div><div className="artifact-badge">encoding: {artifact.encoding ?? 'target unavailable'} · {artifact.wasm_bytes_len} bytes</div>
    {artifact.asm?.length ? <CodeBlock>{artifact.asm.map((item) => `${String(item.offset ?? 0).padStart(4, '0')}  ${(item.bytes ?? '').padEnd(24)}  ${item.text ?? ''}`).join('\n')}</CodeBlock> : <div className="assembly-empty">Native assembly is not emitted for WASM. Raw module bytes are shown below.</div>}
    <div className="hex-table">{rows.map((row, index) => <div className="hex-row" key={index}><span className="hex-offset">{(index * 12).toString(16).padStart(4, '0')}</span>{Array.from(row, (byte, byteIndex) => <span className="hex-byte" title={`WASM byte ${byteIndex}`}>{byte.toString(16).padStart(2, '0')}</span>)}</div>)}</div>
  </div>;
}

function BenchmarkPanel({ benchmark }: { benchmark: { ok: boolean; backend?: string; results?: Array<{ size: number; elapsed_ms?: number; last_result?: number | null }>; error?: string } | null }) {
  if (!benchmark) return <Empty message="Run a valid compilation to measure the portable baseline." />;
  if (!benchmark.ok) return <div className="error-box">{benchmark.error ?? 'Benchmark failed.'}</div>;
  const data = benchmark.results ?? [];
  return <div><div className="artifact-badge">backend: {benchmark.backend ?? 'unknown'} · tier: {useWorkbench.getState().tier}</div><div className="chart-wrap"><ResponsiveContainer width="100%" height={210}><BarChart data={data}><CartesianGrid strokeDasharray="3 3" stroke="#243149" /><XAxis dataKey="size" stroke="#7c8aa5" /><YAxis stroke="#7c8aa5" /><Tooltip contentStyle={{ background: '#10192a', border: '1px solid #2a3a57' }} /><Bar dataKey="elapsed_ms" fill="#7ee787" radius={[4, 4, 0, 0]} /></BarChart></ResponsiveContainer></div><div className="benchmark-table">{data.map((row) => <div key={row.size}><span>{row.size.toLocaleString()} calls</span><strong>{(row.elapsed_ms ?? 0).toFixed(3)} ms</strong><em>{String(row.last_result)}</em></div>)}</div></div>;
}

function TargetPanel() {
  const { target, mode, setTarget, setMode } = useWorkbench((state) => ({ target: state.target, mode: state.mode, setTarget: state.setTarget, setMode: state.setMode }));
  return <div className="target-grid">{targets.map((item) => <button className={`target-card ${target === item.id ? 'target-selected' : ''}`} key={item.id} onClick={() => setTarget(item.id)}><span className="target-icon">{item.label === 'WASM' ? '▱' : item.label === 'x86-64' ? '≋' : '◈'}</span><strong>{item.label}</strong><small>{item.detail}</small>{target === item.id && <b>active</b>}</button>)}<button className={`target-card ${mode === 'array' ? 'target-selected' : ''}`} onClick={() => setMode(mode === 'array' ? 'scalar' : 'array')}><span className="target-icon">▦</span><strong>Array mode</strong><small>SIMD path and scalar tail</small><b>{mode}</b></button></div>;
}

function DiagnosticsPanel({ diagnostics, error }: { diagnostics: Diagnostic[]; error: string | null }) {
  if (!error && diagnostics.length === 0) return <div className="ok-box">✓ No diagnostics. Source spans are retained in the AST.</div>;
  return <div className="diagnostics-list">{error && <div className="error-box">{error}</div>}{diagnostics.map((diagnostic, index) => <div className="diagnostic" key={index}><strong>{diagnostic.message}</strong>{diagnostic.primary && <span>span {diagnostic.primary.start}:{diagnostic.primary.end}{diagnostic.primary.message ? ` · ${diagnostic.primary.message}` : ''}</span>}</div>)}</div>;
}

function Empty({ message }: { message: string }) { return <div className="empty">{message}</div>; }

export default function App() {
  const state = useWorkbench();
  useEffect(() => {
    const timer = window.setTimeout(() => { void compileCurrent(state.source, state.target); }, 200);
    return () => window.clearTimeout(timer);
  }, [state.source, state.target]);
  return <div className="app-shell"><Header /><main className="workspace">
    <div className="editor-column"><Panel title="Expression editor" eyebrow="01 · input"><SourceEditor /><div className="editor-footer"><span>Source spans stay linked to every artifact.</span><span>Tip: try <code>if x &lt; 0 then abs(x) else x</code></span></div></Panel><Panel title="Diagnostics" eyebrow="frontend boundary"><DiagnosticsPanel diagnostics={state.diagnostics} error={state.error} /></Panel></div>
    <div className="artifact-column"><Panel title="AST → SSA IR" eyebrow="02 · structure" wide><div className="split-panel"><AstPanel ast={state.ast} /><IrPanel artifact={state.artifact} /></div></Panel><Panel title="Control-flow graph" eyebrow="03 · control flow"><CfgPanel cfg={state.artifact?.cfg} /></Panel><div className="two-up"><Panel title="Register allocation" eyebrow="04 · liveness"><IntervalPanel intervals={state.artifact?.intervals} /></Panel><Panel title="Assembly + hex" eyebrow="05 · emission"><AssemblyPanel artifact={state.artifact} /></Panel></div><Panel title="Benchmark & tiering" eyebrow="06 · runtime"><BenchmarkPanel benchmark={state.benchmark} /></Panel><Panel title="Target selector" eyebrow="07 · regeneration"><TargetPanel /></Panel></div>
  </main><footer className="app-footer"><span>FORGE / live compiler observatory</span><span>target: {state.target} · mode: {state.mode} · API: {globalThis.forgeWasm ? 'connected' : 'not loaded'}</span></footer></div>;
}
