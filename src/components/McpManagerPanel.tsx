import { useCallback, useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { SectionCard } from "./SectionCard";
import { ConfirmDialog } from "./ConfirmDialog";
import { ExtensionClientToggle } from "./ExtensionClientToggle";
import { useConfirmDialog } from "../lib/confirmDialog";
import { extensionMessage, useExtensionCopy, useExtensionRequest, type McpServer, type McpServerInput } from "../lib/extensionManager";
import "./ExtensionManager.css";

type Pair = { name: string; value: string };
type Draft = {
  originalId: string | null; id: string; transport: "stdio" | "http"; enabled: boolean;
  command: string; args: string; env: Pair[]; envVars: string; url: string; bearerEnv: string; headers: Pair[];
};
const emptyDraft = (): Draft => ({ originalId: null, id: "", transport: "stdio", enabled: true, command: "", args: "[]", env: [], envVars: "", url: "", bearerEnv: "", headers: [] });
function pairs(record: Record<string, string>): Pair[] { return Object.entries(record).map(([name, value]) => ({ name, value })); }
function pairRecord(rows: Pair[]): Record<string, string> {
  const entries = rows.filter((row) => row.name || row.value);
  if (entries.some((row) => !row.name.trim()) || new Set(entries.map((row) => row.name.trim())).size !== entries.length) throw new Error("invalid_fields");
  return Object.fromEntries(entries.map((row) => [row.name.trim(), row.value]));
}

const importExample = JSON.stringify({ mcpServers: { "my-server": { url: "https://example.invalid/mcp" } } }, null, 2);
type ImportPreview = { id: string; transport: "stdio" | "http" | "unsupported"; conflict: boolean };
function previewImport(json: string, servers: McpServer[]): ImportPreview[] | null {
  try {
    const parsed: unknown = JSON.parse(json);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed) || !("mcpServers" in parsed)) return null;
    const entries = parsed.mcpServers;
    if (!entries || typeof entries !== "object" || Array.isArray(entries)) return null;
    return Object.entries(entries).map(([id, config]) => {
      const value = config && typeof config === "object" && !Array.isArray(config) ? config as Record<string, unknown> : {};
      const command = typeof value.command === "string";
      const url = typeof value.url === "string";
      return { id, transport: command && !url ? "stdio" : url && !command ? "http" : "unsupported", conflict: servers.some((server) => server.id === id) };
    });
  } catch { return null; }
}

function PairEditor({ label, rows, setRows, disabled, addLabel }: {
  label: string; rows: Pair[]; setRows: (rows: Pair[]) => void; disabled: boolean; addLabel: string;
}) {
  const copy = useExtensionCopy();
  return <fieldset className="extension-manager__pairs" disabled={disabled}>
    <legend>{label}</legend>
    {rows.map((row, index) => <div className="extension-manager__pair" key={index}>
      <input aria-label={`${label}: ${copy.variable} ${index + 1}`} value={row.name} autoComplete="off" onChange={(event) => setRows(rows.map((item, position) => position === index ? { ...item, name: event.target.value } : item))} />
      <input aria-label={`${label}: ${copy.value} ${index + 1}`} type="password" autoComplete="new-password" value={row.value} onChange={(event) => setRows(rows.map((item, position) => position === index ? { ...item, value: event.target.value } : item))} />
      <button type="button" className="secondary-button" aria-label={`${copy.remove}: ${label} ${index + 1}`} onClick={() => setRows(rows.filter((_, position) => position !== index))}>{copy.remove}</button>
    </div>)}
    <button type="button" className="secondary-button" onClick={() => setRows([...rows, { name: "", value: "" }])}>{addLabel}</button>
  </fieldset>;
}

export function McpManagerPanel({ active, children }: { active: boolean; children?: ReactNode }) {
  const copy = useExtensionCopy();
  const { busy, error, setError, run } = useExtensionRequest();
  const confirm = useConfirmDialog();
  const [servers, setServers] = useState<McpServer[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [showImport, setShowImport] = useState(false);
  const [json, setJson] = useState("");
  const [query, setQuery] = useState("");
  const [notice, setNotice] = useState<{ kind: "saved" | "imported"; count?: number } | null>(null);
  const preview = useMemo(() => previewImport(json, servers), [json, servers]);
  const hasConflicts = preview?.some((entry) => entry.conflict) ?? false;
  const normalizedQuery = query.trim().toLocaleLowerCase();
  const visibleServers = servers.filter((server) => [server.id, server.command, server.url].some((value) => value?.toLocaleLowerCase().includes(normalizedQuery)));
  const refresh = useCallback(() => run(() => invoke<McpServer[]>("list_mcp_servers"), (next) => { setServers(next); setLoaded(true); }), [run]);
  useEffect(() => { if (active) void refresh(); }, [active, refresh]);
  const update = <K extends keyof Draft>(key: K, value: Draft[K]) => setDraft((current) => current ? { ...current, [key]: value } : current);

  const edit = (server: McpServer) => {
    if (!server.editable || server.transport === "unsupported") return;
    setShowImport(false);
    setDraft({ originalId: server.id, id: server.id, transport: server.transport, enabled: server.enabled,
      command: server.command ?? "", args: JSON.stringify(server.args, null, 2), env: pairs(server.env), envVars: server.envVars.join("\n"),
      url: server.url ?? "", bearerEnv: server.bearerTokenEnvVar ?? "", headers: pairs(server.httpHeaders) });
    setError(null);
    setNotice(null);
  };
  const save = (event: FormEvent) => {
    event.preventDefault();
    if (!draft || busy) return;
    setNotice(null);
    let server: McpServerInput;
    try {
      const args: unknown = draft.transport === "stdio" ? JSON.parse(draft.args) : [];
      if (!draft.id.trim() || !Array.isArray(args) || args.some((argument) => typeof argument !== "string")) throw new Error();
      if (draft.transport === "stdio" && !draft.command.trim()) throw new Error();
      if (draft.transport === "http" && !["https:", "http:"].includes(new URL(draft.url).protocol)) throw new Error();
      server = { id: draft.id.trim(), transport: draft.transport, enabled: draft.enabled,
        command: draft.transport === "stdio" ? draft.command.trim() : null,
        args: draft.transport === "stdio" ? args as string[] : [],
        env: draft.transport === "stdio" ? pairRecord(draft.env) : {},
        envVars: draft.transport === "stdio" ? draft.envVars.split(/\r?\n/).map((name) => name.trim()).filter(Boolean) : [],
        url: draft.transport === "http" ? draft.url.trim() : null,
        bearerTokenEnvVar: draft.transport === "http" ? draft.bearerEnv.trim() || null : null,
        httpHeaders: draft.transport === "http" ? pairRecord(draft.headers) : {} };
    } catch { setError("invalidFields"); return; }
    void run(() => invoke<McpServer[]>("save_mcp_server", { request: { originalId: draft.originalId, server } }), (next) => { setServers(next); setDraft(null); setNotice({ kind: "saved" }); });
  };
  const importJson = (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setNotice(null);
    if (!preview?.length) { setError("invalidJson"); return; }
    if (hasConflicts) { setError("conflict"); return; }
    const count = preview.length;
    void run(() => invoke<McpServer[]>("import_mcp_servers", { json }), (next) => { setServers(next); setShowImport(false); setJson(""); setNotice({ kind: "imported", count }); });
  };
  const remove = async (server: McpServer) => {
    if (busy || !server.editable) return;
    if (await confirm.request({ title: `${copy.deleteTitle} ${server.id}`, body: copy.deleteBody, confirmLabel: copy.remove, cancelLabel: copy.cancel })) {
      setNotice(null);
      await run(() => invoke<McpServer[]>("delete_mcp_server", { id: server.id }), (next) => { setServers(next); setNotice({ kind: "saved" }); });
    }
  };

  return <div className="extension-manager" aria-busy={busy}>
    <SectionCard title={copy.mcpTitle} subtitle={copy.mcpSubtitle}>
      <div className="extension-manager__toolbar">
        <button type="button" className="secondary-button" disabled={busy} onClick={() => void refresh()}>{copy.refresh}</button>
        <button type="button" className="primary-button" disabled={busy} onClick={() => { setDraft(emptyDraft()); setShowImport(false); setError(null); setNotice(null); }}>{copy.add}</button>
        <button type="button" className="secondary-button" disabled={busy} onClick={() => { setShowImport(true); setDraft(null); setError(null); setNotice(null); }}>{copy.importJson}</button>
      </div>
      {error && <p className="extension-manager__error" role="alert">{copy[error]}</p>}
      {busy && <p className="extension-manager__muted" role="status">{loaded ? copy.busy : copy.loading}</p>}
      {notice && !busy && <p className="extension-manager__notice" role="status">{notice.kind === "imported" ? extensionMessage(copy.mcpImportResult, { count: notice.count ?? 0 }) : copy.savedResult} {copy.mcpActivation}</p>}
      {draft && <form className="extension-manager__form" onSubmit={save}>
        <h3>{draft.originalId === null ? copy.add : copy.edit}</h3>
        <fieldset disabled={busy} className="extension-manager__grid">
          <label>{copy.name}<input value={draft.id} onChange={(event) => update("id", event.target.value)} required /></label>
          <label>{copy.transport}<select value={draft.transport} onChange={(event) => update("transport", event.target.value as "stdio" | "http")}><option value="stdio">stdio</option><option value="http">HTTP</option></select></label>
          {draft.transport === "stdio" ? <>
            <label className="extension-manager__wide">{copy.command}<input value={draft.command} onChange={(event) => update("command", event.target.value)} autoComplete="off" required /></label>
            <label className="extension-manager__wide">{copy.args}<textarea value={draft.args} onChange={(event) => update("args", event.target.value)} rows={3} spellCheck={false} /></label>
            <div className="extension-manager__wide"><PairEditor label={copy.env} rows={draft.env} setRows={(rows) => update("env", rows)} disabled={busy} addLabel={copy.addVariable} /></div>
            <label className="extension-manager__wide">{copy.envVars}<textarea value={draft.envVars} onChange={(event) => update("envVars", event.target.value)} rows={2} /><small>{copy.envVarsHint}</small></label>
          </> : <>
            <label className="extension-manager__wide">{copy.url}<input type="url" value={draft.url} onChange={(event) => update("url", event.target.value)} required /></label>
            <label className="extension-manager__wide">{copy.bearerEnv}<input value={draft.bearerEnv} onChange={(event) => update("bearerEnv", event.target.value)} autoComplete="off" /></label>
            <div className="extension-manager__wide"><PairEditor label={copy.headers} rows={draft.headers} setRows={(rows) => update("headers", rows)} disabled={busy} addLabel={copy.addHeader} /></div>
          </>}
          <label className="extension-manager__checkbox"><input type="checkbox" checked={draft.enabled} onChange={(event) => update("enabled", event.target.checked)} />{copy.enabled}</label>
        </fieldset>
        <div className="extension-manager__actions"><button className="primary-button" type="submit" disabled={busy}>{copy.save}</button><button className="secondary-button" type="button" disabled={busy} onClick={() => setDraft(null)}>{copy.cancel}</button></div>
      </form>}
      {showImport && <form className="extension-manager__form" onSubmit={importJson}>
        <label>{copy.importJson}<textarea value={json} onChange={(event) => setJson(event.target.value)} rows={7} disabled={busy} spellCheck={false} /></label>
        <p className="extension-manager__muted">{copy.jsonHint}</p>
        <details className="extension-manager__example"><summary>{copy.jsonExample}</summary><pre>{importExample}</pre></details>
        {preview && preview.length > 0 && <section className="extension-manager__preview" aria-label={copy.importPreview}>
          <h3>{copy.importPreview}</h3><p className="extension-manager__muted">{copy.previewHint}</p>
          <ul>{preview.map((entry) => <li key={entry.id}><strong>{entry.id}</strong><span>{entry.transport === "http" ? "HTTP" : entry.transport === "stdio" ? "stdio" : copy.readonly}</span><span className={entry.conflict ? "extension-manager__error" : "extension-manager__muted"}>{entry.conflict ? copy.existingEntry : copy.newEntry}</span></li>)}</ul>
          {hasConflicts && <p className="extension-manager__error">{copy.importBlocked}</p>}
        </section>}
        <div className="extension-manager__actions"><button className="primary-button" type="submit" disabled={busy || !json.trim() || hasConflicts}>{copy.importAction}</button><button className="secondary-button" type="button" disabled={busy} onClick={() => setShowImport(false)}>{copy.cancel}</button></div>
      </form>}
      <label className="extension-manager__search">{copy.searchMcp}<input type="search" value={query} onChange={(event) => setQuery(event.target.value)} autoComplete="off" /></label>
      {loaded && servers.length === 0 && <p className="extension-manager__muted">{copy.emptyMcp}</p>}
      {loaded && servers.length > 0 && visibleServers.length === 0 && <p className="extension-manager__muted">{copy.noMatches}</p>}
      <div className="extension-manager__list">{visibleServers.map((server) => <article className="extension-manager__item" key={server.id}>
        <div className="extension-manager__item-heading"><h3>{server.id}</h3><span className="extension-manager__badge">{server.transport === "http" ? "HTTP" : server.transport === "stdio" ? "stdio" : copy.readonly}</span><span className="extension-manager__badge">{server.enabled ? copy.enabled : copy.disabled}</span></div>
        <p className="extension-manager__path">{server.transport === "http" ? server.url : server.command}</p>
        {server.editable && server.transport !== "unsupported" ? <div className="extension-manager__actions">
          <button className="secondary-button" type="button" disabled={busy} onClick={() => edit(server)}>{copy.edit}</button>
          <ExtensionClientToggle enabled={server.enabled} disabled={busy} enabledLabel={copy.enabled} disabledLabel={copy.disabled} enableLabel={copy.enable} disableLabel={copy.disable} onToggle={() => { setNotice(null); void run(() => invoke<McpServer[]>("set_mcp_server_enabled", { id: server.id, enabled: !server.enabled }), (next) => { setServers(next); setNotice({ kind: "saved" }); }); }} />
          <button className="secondary-button extension-manager__danger" type="button" disabled={busy} onClick={() => void remove(server)}>{copy.remove}</button>
        </div> : <p className="extension-manager__muted">{server.managedBy === "headroom" ? copy.managedBelow : copy.readonly}</p>}
      </article>)}</div>
    </SectionCard>
    {children}
    {confirm.prompt && <ConfirmDialog prompt={confirm.prompt} onAnswer={confirm.answer} />}
  </div>;
}
