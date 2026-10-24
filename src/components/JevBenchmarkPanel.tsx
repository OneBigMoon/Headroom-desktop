import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface BenchmarkSample {
  id: string;
  text: string;
  expected: string;
}

interface BenchmarkCase {
  caseId: string;
  expected: string;
  predicted: string | null;
  correct: boolean;
  status: "success" | "error";
  model: string | null;
  latencyMs: number;
  inputTokens: number | null;
  outputTokens: number | null;
  confidence: number | null;
  error: string | null;
}

export interface JevBenchmarkSnapshot {
  datasetVersion: string;
  samples: BenchmarkSample[];
  busy: boolean;
  result: {
    id: string;
    datasetVersion: string;
    startedAt: string;
    completedAt: string;
    provider: "typesafe";
    requestedModel: string;
    total: number;
    successful: number;
    failed: number;
    correct: number;
    accuracy: number;
    cases: BenchmarkCase[];
  } | null;
}

interface JevBenchmarkPanelProps {
  keyConfigured: boolean;
  disabled: boolean;
  onRunningChange: (running: boolean) => void;
  onComplete: () => void;
}

function benchmarkError(error: unknown): string {
  const code = typeof error === "string" ? error : error instanceof Error ? error.message : "";
  const messages: Record<string, string> = {
    jev_key_missing: "请先连接 TypeSafe Key，再运行测试。",
    jev_busy: "已有分类或测试正在运行，请等待完成。",
    jev_benchmark_read: "无法读取已保存的测试结果。请先保留并备份 telemetry/jev-benchmark.json，检查文件内容与版本后再恢复读取；反复重读不会修复损坏文件。",
    jev_benchmark_write: "测试结果未能保存；已发出的请求可能产生费用，请勿反复点击运行。",
    jev_key_store: "无法访问已保存的 TypeSafe Key，请检查系统钥匙串授权。",
  };
  return messages[code] ?? "本次测试未能完成；已发出的请求可能产生费用，不会自动重试。";
}

export function JevBenchmarkPanel({ keyConfigured, disabled, onRunningChange, onComplete }: JevBenchmarkPanelProps) {
  const [snapshot, setSnapshot] = useState<JevBenchmarkSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const runInFlight = useRef(false);
  const readInFlight = useRef(false);
  const mounted = useRef(true);
  const previouslyBusy = useRef(false);

  const load = useCallback(async () => {
    if (readInFlight.current) return;
    readInFlight.current = true;
    try {
      const next = await invoke<JevBenchmarkSnapshot>("get_jev_benchmark");
      if (mounted.current) {
        setSnapshot(next);
        setError(null);
      }
    } catch (err) {
      if (mounted.current) setError(benchmarkError(err));
    } finally {
      readInFlight.current = false;
      if (mounted.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    void load();
    return () => { mounted.current = false; };
  }, [load]);

  useEffect(() => {
    if (!snapshot?.busy || running) return;
    const timer = window.setInterval(() => void load(), 1500);
    return () => window.clearInterval(timer);
  }, [snapshot?.busy, running, load]);

  const busy = running || snapshot?.busy === true;
  useEffect(() => {
    onRunningChange(busy);
    return () => onRunningChange(false);
  }, [busy, onRunningChange]);

  useEffect(() => {
    if (previouslyBusy.current && !snapshot?.busy) onComplete();
    previouslyBusy.current = snapshot?.busy === true;
  }, [snapshot?.busy, onComplete]);

  const run = async () => {
    if (!keyConfigured || disabled || busy || loading || !snapshot || runInFlight.current) return;
    runInFlight.current = true;
    setRunning(true);
    setError(null);
    try {
      const next = await invoke<JevBenchmarkSnapshot>("run_jev_benchmark");
      if (mounted.current) setSnapshot(next);
    } catch (err) {
      if (mounted.current) setError(benchmarkError(err));
    } finally {
      runInFlight.current = false;
      if (mounted.current) {
        setRunning(false);
        onComplete();
      }
    }
  };

  const result = snapshot?.result;
  const models = result ? [...new Set(result.cases.map((item) => item.model).filter((model): model is string => model !== null))] : [];
  const cases = new Map(result?.cases.map((item) => [item.caseId, item]));
  const inputCases = result?.cases.filter((item) => item.inputTokens !== null) ?? [];
  const outputCases = result?.cases.filter((item) => item.outputTokens !== null) ?? [];

  return (
    <section className="jev-panel__benchmark" aria-labelledby="jev-benchmark-title">
      <div className="jev-panel__evaluate-heading">
        <div>
          <h3 id="jev-benchmark-title">TypeSafe 分类测试</h3>
          <p className="jev-panel__hint">使用固定中英样本和预先标注的答案验证四类意图。结果只代表这份测试集，不代表真实 Codex 请求的准确率。</p>
        </div>
        <button type="button" className="jev-panel__button" onClick={() => void run()} disabled={!keyConfigured || disabled || busy || loading || !snapshot}>
          {busy ? "测试进行中…" : result ? "重新运行分类测试" : "运行分类测试"}
        </button>
      </div>
      <p className="jev-panel__hint">点击运行会向 TypeSafe 的 jev-latest 发送 {snapshot?.samples.length ?? 12} 条公开测试文本，可能产生费用；不会发送你的会话文本，也不会自动运行或重试。测试记录与上方业务调用统计分开保存。</p>
      {!keyConfigured && <p className="jev-panel__hint">连接 TypeSafe Key 后可运行这项测试。</p>}
      {loading && <p role="status">正在读取测试结果…</p>}
      {busy && <p role="status">正在逐条调用模型并核对答案，请等待本轮完成。</p>}
      {error && <div className="jev-panel__error" role="alert"><p>{error}</p><button type="button" className="jev-panel__button jev-panel__button--quiet" onClick={() => void load()} disabled={busy}>重新读取测试结果</button></div>}
      {result ? (
        <>
          <div className="jev-panel__metrics" aria-label="最近一次分类测试结果">
            <div><span>内置测试集正确率</span><strong>{result.accuracy.toFixed(1)}%</strong><small>正确 {result.correct}/{result.total}；请求失败也计入总数</small></div>
            <div><span>分类错误 / 请求失败</span><strong>{result.successful - result.correct} / {result.failed}</strong><small>成功返回 {result.successful}/{result.total} 条</small></div>
            <div><span>测试输入 token（已上报）</span><strong>{inputCases.length ? inputCases.reduce((sum, item) => sum + (item.inputTokens ?? 0), 0).toLocaleString() : "暂无数据"}</strong><small>{inputCases.length}/{result.total} 条有数据</small></div>
            <div><span>测试输出 token（已上报）</span><strong>{outputCases.length ? outputCases.reduce((sum, item) => sum + (item.outputTokens ?? 0), 0).toLocaleString() : "暂无数据"}</strong><small>{outputCases.length}/{result.total} 条有数据；未返回不计为 0</small></div>
          </div>
          <p className="jev-panel__hint">测试集 {result.datasetVersion} · 完成时间（本地）：{new Date(result.completedAt).toLocaleString()} · 请求模型 {result.requestedModel} · 实际返回模型：{models.join("、") || "未上报"}</p>
          <p className="jev-panel__hint">以上是本轮测试的独立用量，不含在业务费用估值中；实际测试费用以 TypeSafe 账单为准。</p>
        </>
      ) : !loading && !error && <p className="jev-panel__empty">尚无测试结果。运行后会显示正确率、失败数和每条样本的实际返回。</p>}
      {snapshot && <details className="jev-panel__benchmark-details">
        <summary>查看 {snapshot.samples.length} 条测试样本{result ? "与结果" : "及预期答案"}</summary>
        <ol>
          {snapshot.samples.map((sample) => {
            const item = result?.datasetVersion === snapshot.datasetVersion ? cases.get(sample.id) : undefined;
            return <li key={sample.id}>
              <p>{sample.text}</p>
              <p>预期：{sample.expected} · 实际：{item?.predicted ?? "尚无分类结果"} {item && <span className={`jev-panel__record-status jev-panel__record-status--${item.correct ? "success" : "error"}`}>{item.status === "error" ? "请求失败" : item.correct ? "正确" : "分类错误"}</span>}</p>
              {item && <p className="jev-panel__hint">模型 {item.model ?? "未上报"} · 耗时 {item.latencyMs.toLocaleString()} ms · 输入 token {item.inputTokens?.toLocaleString() ?? "未上报"} · 输出 token {item.outputTokens?.toLocaleString() ?? "未上报"}{item.error ? ` · 错误：${item.error}` : ""}</p>}
            </li>;
          })}
        </ol>
        {result && result.datasetVersion !== snapshot.datasetVersion && <p className="jev-panel__hint">上次结果属于旧版测试集；请重新运行以查看当前样本的逐条结果。</p>}
      </details>}
    </section>
  );
}
