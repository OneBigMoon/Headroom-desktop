import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useI18n } from "../lib/i18n";

type Audit = {
  target: string; path: string; baseline_hash: string; candidate_hash: string;
  baseline: string; candidate: string; findings: string[]; blocked: boolean;
};
type Snapshot = { id: string; target: string; created_at: string };

export function InstructionGovernance() {
  const { resolvedLocale } = useI18n();
  const zh = resolvedLocale.startsWith("zh");
  const [reports, setReports] = useState<Audit[]>([]);
  const [snapshots, setSnapshots] = useState<Snapshot[]>([]);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const refresh = async () => {
    const [audits, saved] = await Promise.all([
      invoke<Audit[]>("audit_instructions"), invoke<Snapshot[]>("list_instruction_snapshots"),
    ]);
    setReports(audits); setSnapshots(saved);
  };
  const run = async (action: () => Promise<void>) => {
    setBusy(true); setMessage("");
    try { await action(); } catch (error) { setMessage(String(error)); }
    finally { setBusy(false); }
  };
  return <article className="soft-card panel-card instruction-governance">
    <h3>{zh ? "指令审计与恢复" : "Instruction audit and recovery"}</h3>
    <p>{zh ? "检查本机 Codex / Claude 的工具提示。仅更新已有 Headroom 区块；用户规则保留。此检查不等同于模型行为评测。" : "Review local Codex / Claude tool hints. Only existing Headroom blocks are refreshed; user rules are preserved. This is not a model behavior evaluation."}</p>
    <button className="secondary-button secondary-button--small" disabled={busy} onClick={() => void run(refresh)}>{zh ? "读取并预览" : "Read and preview"}</button>
    {message && <p role="status">{message}</p>}
    {reports.map(report => <section key={report.target}>
      <h4>{report.target}</h4><p>{report.path}</p>
      <ul>{report.findings.map((finding, i) => <li key={i}>{zh ? finding.replace("managed template differs from current canonical source", "托管提示与当前模板不同").replace("current managed template", "托管提示已是当前版本").replace("duplicate or malformed ownership markers; manual review required", "标记重复或残缺，需要人工检查").replace("overlapping ownership blocks; manual review required", "托管区块交叉或嵌套，需要人工检查").replace("Legacy RTK instructions present; compare with managed RTK rules before manual removal", "发现旧 RTK 规则，请核对后手动整理") : finding}</li>)}</ul>
      <details><summary>{zh ? "原文与候选" : "Original and candidate"}</summary>
        <h5>{zh ? "当前原文" : "Current instructions"}</h5><p className="instruction-governance__hash">SHA-256: {report.baseline_hash}</p><pre className="log-output">{report.baseline || (zh ? "文件为空或不存在" : "Empty or absent file")}</pre>
        <h5>{zh ? "更新候选" : "Proposed instructions"}</h5><p className="instruction-governance__hash">SHA-256: {report.candidate_hash}</p><pre className="log-output">{report.candidate}</pre>
      </details>
      <button className="secondary-button secondary-button--small" disabled={busy || report.blocked || report.baseline_hash === report.candidate_hash}
        onClick={() => void run(async () => {
          const id = await invoke<string>("apply_instruction_candidate", { target: report.target, baselineHash: report.baseline_hash, candidateHash: report.candidate_hash });
          await refresh(); setMessage((zh ? "已备份并更新。新会话需重新加载指令。备份：" : "Backed up and updated. Reload instructions in a new session. Snapshot: ") + id);
        })}>{zh ? "备份并应用此候选" : "Back up and apply candidate"}</button>
    </section>)}
    {snapshots.length > 0 && <details><summary>{zh ? "恢复备份" : "Restore snapshots"}</summary>
      {snapshots.map(({ id, target, created_at }) => <p key={id}>{target} · {created_at ? new Date(created_at).toLocaleString() : (zh ? "早期备份" : "Earlier backup")}{" "}<button className="secondary-button secondary-button--small" disabled={busy} onClick={() => void run(async () => {
        await invoke("restore_instruction_snapshot", { id }); await refresh();
        setMessage(zh ? "已恢复并校验文件；新会话需重新加载。" : "File restored and verified; reload in a new session.");
      })}>{zh ? "恢复" : "Restore"}</button></p>)}
    </details>}
  </article>;
}
