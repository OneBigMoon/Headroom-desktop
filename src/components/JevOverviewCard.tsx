import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { PROVIDERS, type JevRecord } from "./JevPanel";
import "./JevOverviewCard.css";

interface JevOverviewCardProps {
  active: boolean;
  onOpenDecisions: () => void;
}

interface JevDashboardSnapshot {
  autoEnabled?: boolean;
  records: JevRecord[];
}

const numberFormat = new Intl.NumberFormat("zh-CN");

function formatCount(value: number): string {
  return numberFormat.format(value);
}

function p95Latency(records: JevRecord[]): number | null {
  const values = records.map((record) => record.latencyMs).filter(Number.isFinite).sort((a, b) => a - b);
  return values.length > 0 ? values[Math.ceil(values.length * 0.95) - 1] : null;
}

function targetKey(record: JevRecord): string {
  return JSON.stringify([record.provider, record.endpoint, record.model]);
}

export function JevOverviewCard({ active, onOpenDecisions }: JevOverviewCardProps) {
  const [dashboard, setDashboard] = useState<JevDashboardSnapshot | null>(null);
  const [loading, setLoading] = useState(active);
  const [loadFailed, setLoadFailed] = useState(false);
  const [retry, setRetry] = useState(0);

  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    let autoEnabled = false;
    let timer: number | undefined;
    setLoading(true);
    setLoadFailed(false);
    const load = async () => {
      try {
        const result = await invoke<JevDashboardSnapshot>("get_jev_dashboard");
        if (cancelled) return;
        autoEnabled = result.autoEnabled === true;
        setDashboard(result);
        setLoadFailed(false);
      } catch {
        if (!cancelled) setLoadFailed(true);
      } finally {
        if (!cancelled) {
          setLoading(false);
          if (autoEnabled) timer = window.setTimeout(() => void load(), 4000);
        }
      }
    };
    void load();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [active, retry]);

  const records = dashboard?.records ?? [];
  const successful = records.filter((record) => record.status === "success");
  const failed = records.length - successful.length;
  const successPercent = records.length > 0 ? (successful.length / records.length) * 100 : 0;
  const latency = p95Latency(records);
  const outputRecords = records.filter((record) => record.outputTokens !== null);
  const outputTotal = outputRecords.reduce((sum, record) => sum + (record.outputTokens ?? 0), 0);
  const latestChoice = [...successful].reverse().find((record) => record.choice !== null);
  const scopedRecords = latestChoice
    ? successful.filter((record) => targetKey(record) === targetKey(latestChoice) && record.choice !== null)
    : [];
  const choiceCounts = new Map<string, number>();
  for (const record of scopedRecords) {
    if (record.choice !== null) choiceCounts.set(record.choice, (choiceCounts.get(record.choice) ?? 0) + 1);
  }
  const choices = [...choiceCounts].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));

  return (
    <section className="soft-card jev-overview" aria-labelledby="jev-overview-title">
      <header className="jev-overview__header">
        <div>
          <h2 id="jev-overview-title">决策概览</h2>
          <p>手动与 Codex 自动评估 · 最近 200 条</p>
        </div>
        <button type="button" className="jev-overview__link" onClick={onOpenDecisions}>打开决策</button>
      </header>

      {loading ? (
        <p className="jev-overview__state" role="status">正在读取决策记录…</p>
      ) : loadFailed ? (
        <div className="jev-overview__state" role="alert">
          <p>暂时无法读取决策记录。</p>
          <button type="button" className="jev-overview__link" onClick={() => setRetry((value) => value + 1)}>重试</button>
        </div>
      ) : records.length === 0 ? (
        <p className="jev-overview__state">还没有决策记录。到“类型化决策”手动评估，或开启 TypeSafe 自动评估 Codex 请求。</p>
      ) : (
        <>
          <div className="jev-overview__metrics" aria-label="决策会话指标">
            <div><span>请求数</span><strong>{formatCount(records.length)}</strong></div>
            <div><span>调用成功率</span><strong>{successPercent.toFixed(1)}%</strong></div>
            <div><span>延迟 p95</span><strong>{latency === null ? "暂无数据" : `${formatCount(Math.round(latency))} ms`}</strong></div>
            <div>
              <span>输出 token（已上报）</span>
              <strong>{outputRecords.length === 0 ? "暂无数据" : formatCount(outputTotal)}</strong>
              <small>{outputRecords.length}/{records.length} 条有数据；未返回不计为 0</small>
            </div>
          </div>

          <div className="jev-overview__charts">
            <div className="jev-overview__chart">
              <h3>请求结果</h3>
              <div className="jev-overview__status-bar" role="img" aria-label={`成功 ${successful.length} 条，失败 ${failed} 条`}>
                {successful.length > 0 && <span className="jev-overview__status-success" style={{ width: `${successPercent}%` }} />}
                {failed > 0 && <span className="jev-overview__status-failure" style={{ width: `${100 - successPercent}%` }} />}
              </div>
              <p className="jev-overview__legend"><span>成功 {formatCount(successful.length)}</span><span>失败 {formatCount(failed)}</span></p>
            </div>
            <div className="jev-overview__chart">
              <h3>最近成功模型的分类结果</h3>
              {latestChoice ? (
                <>
                  <p className="jev-overview__scope">{latestChoice.model ?? "未命名模型"} · {latestChoice.provider === "custom" ? "自定义模型" : PROVIDERS[latestChoice.provider].label} · {formatCount(scopedRecords.length)} 条</p>
                  <div className="jev-overview__choices" aria-label="分类结果分布">
                    {choices.map(([choice, count]) => (
                      <div className="jev-overview__choice" key={choice}>
                        <span className="jev-overview__choice-label" title={choice}>{choice}</span>
                        <span className="jev-overview__choice-track" aria-hidden="true"><span style={{ width: `${(count / scopedRecords.length) * 100}%` }} /></span>
                        <strong>{formatCount(count)}</strong>
                      </div>
                    ))}
                  </div>
                </>
              ) : <p className="jev-overview__scope">暂无成功分类结果。</p>}
            </div>
          </div>
        </>
      )}
      <p className="jev-overview__note">Jev 仅分类观察，未替代 Codex 请求。分类测试结果和额外费用估值见决策面板；调用成功率不代表分类正确率。</p>
    </section>
  );
}
