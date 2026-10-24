import { useEffect, useId, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { SectionCard } from "./SectionCard";
import { ExtensionClientToggle } from "./ExtensionClientToggle";
import { extensionErrorKey, extensionMessage, skillScopeLabel, useExtensionCopy, useExtensionRequest, type GitSkillScan, type GitSkillSummary, type SkillCatalog, type SkillScan, type SkillSummary } from "../lib/extensionManager";
import "./ExtensionManager.css";

const mergeSkills = (current: SkillSummary[], updates: SkillSummary[]) => {
  const byPath = new Map(current.map((skill) => [skill.skillPath, skill]));
  updates.forEach((skill) => byPath.set(skill.skillPath, skill));
  return [...byPath.values()];
};

export function SkillManagerPanel({ active, children }: { active: boolean; children?: ReactNode }) {
  const copy = useExtensionCopy();
  const { busy, error, setError, run } = useExtensionRequest();
  const [catalog, setCatalog] = useState<SkillCatalog>({ skills: [], warnings: [], configPath: "" });
  const [loaded, setLoaded] = useState(false);
  const [sourcePath, setSourcePath] = useState("");
  const [scan, setScan] = useState<SkillScan | null>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const [repositoryUrl, setRepositoryUrl] = useState("");
  const [discovery, setDiscovery] = useState<{ repositoryUrl: string; scan: GitSkillScan } | null>(null);
  const [reader, setReader] = useState<{ skillPath: string; content: string } | null>(null);
  const [view, setView] = useState<"installed" | "discover">("installed");
  const [query, setQuery] = useState("");
  const [discoveryQuery, setDiscoveryQuery] = useState("");
  const [notice, setNotice] = useState<{ kind: "saved" } | { kind: "imported"; success: number; failed: number } | null>(null);
  const tabId = useId();
  const normalizedQuery = query.trim().toLocaleLowerCase();
  const visibleSkills = catalog.skills.filter((skill) => [skill.name, skill.description, skill.directory, skill.skillPath].some((value) => value.toLocaleLowerCase().includes(normalizedQuery)));
  const visibleDiscoveries = discovery?.scan.skills.filter((skill) => [skill.name, skill.description].some((value) => value.toLocaleLowerCase().includes(discoveryQuery.trim().toLocaleLowerCase()))) ?? [];

  const refresh = () => run(
    () => invoke<SkillCatalog>("list_skills"),
    (next) => { setCatalog(next); setLoaded(true); }
  );
  useEffect(() => { if (active) void refresh(); }, [active]);

  const scanRepository = (source: string) => {
    const repository = source.trim();
    try {
      const url = new URL(repository);
      if (url.protocol !== "https:" || url.username || url.password) throw new Error();
    } catch { setError("sourceRequired"); return; }
    void run(() => invoke<GitSkillScan>("scan_git_skills", { repositoryUrl: repository }), (next) => {
      setDiscovery({ repositoryUrl: repository, scan: next });
    });
  };
  const chooseSource = () => {
    setNotice(null);
    void run(async () => {
      const source = await open({ directory: true, multiple: false });
      if (typeof source !== "string") return null;
      const result = await invoke<SkillScan>("scan_skill_source", { sourcePath: source });
      return { source, result };
    }, (next) => {
      if (next) { setSourcePath(next.source); setScan(next.result); setSelected([]); }
    });
  };
  const importSelected = () => {
    if (busy) return;
    setNotice(null);
    if (selected.length === 0) { setError("selectionRequired"); return; }
    void run(async () => {
      const imported: SkillSummary[] = [];
      const failures: string[] = [];
      let failure: unknown = null;
      for (const skillPath of selected) {
        const source = scan?.skills.find((skill) => skill.skillPath === skillPath);
        if (!source) continue;
        try { imported.push(await invoke<SkillSummary>("import_skill", { sourcePath: source.directory, target: "global" })); }
        catch (error) { failures.push(skillPath); failure = error; }
      }
      return { imported, failures, failure };
    }, ({ imported, failures, failure }) => {
      setCatalog((current) => ({ ...current, skills: mergeSkills(current.skills, imported) }));
      setSelected(failures);
      setNotice({ kind: "imported", success: imported.length, failed: failures.length });
      if (failure !== null) setError(extensionErrorKey(failure));
    });
  };
  const install = (skill: GitSkillSummary) => {
    if (!discovery || busy) return;
    setNotice(null);
    void run(() => invoke<SkillSummary>("import_git_skill", { repositoryUrl: discovery.repositoryUrl, subdirectory: skill.subdirectory, target: "global" }), (next) => {
      setCatalog((current) => ({ ...current, skills: mergeSkills(current.skills, [next]) }));
      setNotice({ kind: "imported", success: 1, failed: 0 });
    });
  };
  const read = (skill: SkillSummary) => void run(
    () => invoke<{ skillPath: string; content: string }>("read_skill", { skillPath: skill.skillPath }), setReader
  );
  const toggle = (skill: SkillSummary) => {
    setNotice(null);
    void run(
      () => invoke<SkillSummary>("set_skill_enabled", { skillPath: skill.skillPath, enabled: !skill.enabled }),
      (next) => { setCatalog((current) => ({ ...current, skills: mergeSkills(current.skills, [next]) })); setNotice({ kind: "saved" }); }
    );
  };

  return <div className="extension-manager" aria-busy={busy}>
    <SectionCard title={copy.skillsTitle} subtitle={copy.skillsSubtitle}>
      <div className="extension-manager__tabs" role="tablist" aria-label={copy.skillViews} onKeyDown={(event) => {
        if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event["key"])) return;
        event.preventDefault();
        const next = event["key"] === "Home" ? "installed" : event["key"] === "End" ? "discover" : view === "installed" ? "discover" : "installed";
        setView(next);
        document.getElementById(`${tabId}-${next}`)?.focus();
      }}>
        {(["installed", "discover"] as const).map((tab) => <button key={tab} type="button" id={`${tabId}-${tab}`} role="tab" aria-selected={view === tab} aria-controls={`${tabId}-${tab}-panel`} tabIndex={view === tab ? 0 : -1} onClick={() => setView(tab)}>{tab === "installed" ? copy.installedTab : copy.discoverTab}</button>)}
      </div>
      {error && <p className="extension-manager__error" role="alert">{copy[error]}</p>}
      {busy && <p className="extension-manager__muted" role="status">{loaded ? copy.busy : copy.loading}</p>}
      {notice && !busy && <p className="extension-manager__notice" role="status">{notice.kind === "saved" ? `${copy.savedResult} ${copy.skillActivation}` : <>{extensionMessage(copy.skillImportResult, { success: notice.success, failed: notice.failed })} {notice.success > 0 && copy.skillActivation} {notice.failed > 0 && copy.failedSelections}</>}</p>}
      {view === "installed" && <div role="tabpanel" id={`${tabId}-installed-panel`} aria-labelledby={`${tabId}-installed`}>
        <div className="extension-manager__toolbar"><button type="button" className="secondary-button" disabled={busy} onClick={() => void refresh()}>{copy.refresh}</button></div>
        <label className="extension-manager__search">{copy.searchSkills}<input type="search" value={query} onChange={(event) => setQuery(event.target.value)} autoComplete="off" /></label>
        {catalog.warnings.length > 0 && <div className="extension-manager__notices"><strong>{copy.warning}</strong><ul>{catalog.warnings.map((warning, index) => <li key={index}>{warning}</li>)}</ul></div>}
        {loaded && catalog.skills.length === 0 && <p className="extension-manager__muted">{copy.emptySkills}</p>}
        {loaded && catalog.skills.length > 0 && visibleSkills.length === 0 && <p className="extension-manager__muted">{copy.noMatches}</p>}
        <div className="extension-manager__list">{visibleSkills.map((skill) => <article className="extension-manager__item" key={skill.skillPath}>
          <div className="extension-manager__item-heading"><h3>{skill.name}</h3><span className="extension-manager__badge">{skillScopeLabel(skill.scope, copy)}</span><span className="extension-manager__badge">{skill.enabled ? copy.enabled : copy.disabled}</span></div>
          {skill.description && <p className="extension-manager__muted">{skill.description}</p>}
          <p className="extension-manager__path">{skill.skillPath}</p>
          <div className="extension-manager__actions"><button className="secondary-button" type="button" disabled={busy} onClick={() => read(skill)}>{copy.readSkill}</button>{skill.canToggle && <ExtensionClientToggle enabled={skill.enabled} disabled={busy} onToggle={() => toggle(skill)} enabledLabel={copy.enabled} disabledLabel={copy.disabled} enableLabel={copy.enable} disableLabel={copy.disable} />}</div>
          {!skill.canToggle && <p className="extension-manager__muted">{copy.unavailable}</p>}
        </article>)}</div>
      </div>}
      {reader && <section className="extension-manager__form extension-manager__reader" aria-label={copy.skillFile}>
        <div className="extension-manager__item-heading"><h3>{copy.skillFile}</h3><button className="secondary-button" type="button" onClick={() => setReader(null)}>{copy.close}</button></div>
        <p className="extension-manager__path">{reader.skillPath}</p><pre>{reader.content}</pre>
      </section>}
      {view === "discover" && <div role="tabpanel" id={`${tabId}-discover-panel`} aria-labelledby={`${tabId}-discover`}>
        <p className="extension-manager__muted">{copy.discoveryHint}</p>
        <div className="extension-manager__toolbar extension-manager__discovery-actions">
          {discovery && <button type="button" className="secondary-button" disabled={busy} onClick={() => scanRepository(discovery.repositoryUrl)}>{copy.refreshRepository}</button>}
          <button type="button" className="secondary-button" disabled={busy} onClick={chooseSource}>{copy.importLocal}</button>
        </div>
        <form className="extension-manager__form" onSubmit={(event) => { event.preventDefault(); scanRepository(repositoryUrl); }}>
          <label>{copy.repository}<input type="url" value={repositoryUrl} onChange={(event) => setRepositoryUrl(event.target.value)} disabled={busy} autoComplete="off" required /></label>
          <div className="extension-manager__actions"><button type="submit" className="secondary-button" disabled={busy || !repositoryUrl.trim()}>{copy.loadRepository}</button></div>
        </form>
        {discovery && <p className="extension-manager__path">{copy.repositorySource}: {discovery.repositoryUrl}</p>}
        <label className="extension-manager__search">{copy.searchDiscoveries}<input type="search" value={discoveryQuery} onChange={(event) => setDiscoveryQuery(event.target.value)} autoComplete="off" /></label>
        {discovery && discovery.scan.warnings.length > 0 && <div className="extension-manager__notices"><strong>{copy.warning}</strong><ul>{discovery.scan.warnings.map((warning, index) => <li key={index}>{warning}</li>)}</ul></div>}
        {discovery && discovery.scan.skills.length === 0 && <p className="extension-manager__muted">{copy.emptySkills}</p>}
        {discovery && discovery.scan.skills.length > 0 && visibleDiscoveries.length === 0 && <p className="extension-manager__muted">{copy.noMatches}</p>}
        <div className="extension-manager__list">{visibleDiscoveries.map((skill) => {
          const installed = catalog.skills.some((existing) => existing.directory === skill.installDirectory);
          return <article className="extension-manager__item" key={skill.subdirectory}>
            <div className="extension-manager__item-heading"><h3>{skill.name}</h3></div>
            {skill.description && <p className="extension-manager__muted">{skill.description}</p>}
            <div className="extension-manager__actions"><button type="button" className="primary-button" disabled={busy || installed} onClick={() => install(skill)}>{installed ? copy.installedTab : copy.installSkill}</button></div>
          </article>;
        })}</div>
        {scan && <fieldset className="extension-manager__pairs extension-manager__local-import" disabled={busy}>
          <legend>{copy.selectSkills}</legend>
          <p className="extension-manager__path">{copy.localSource}: {sourcePath}</p>
          {scan.warnings.length > 0 && <div className="extension-manager__notices"><strong>{copy.warning}</strong><ul>{scan.warnings.map((warning, index) => <li key={index}>{warning}</li>)}</ul></div>}
          {scan.skills.length === 0 && <p className="extension-manager__muted">{copy.emptySkills}</p>}
          {scan.skills.map((skill) => <div className="extension-manager__item" key={skill.skillPath}>
            <label className="extension-manager__checkbox"><input type="checkbox" checked={selected.includes(skill.skillPath)} onChange={(event) => setSelected((current) => event.target.checked ? [...current, skill.skillPath] : current.filter((path) => path !== skill.skillPath))} />{skill.name}</label>
            {skill.description && <p className="extension-manager__muted">{skill.description}</p>}
            <button className="secondary-button" type="button" onClick={() => read(skill)}>{copy.readSkill}</button>
          </div>)}
          <button type="button" className="primary-button" onClick={importSelected} disabled={busy || selected.length === 0}>{copy.importSelected}</button>
        </fieldset>}
      </div>}
    </SectionCard>
    {view === "installed" && children}
  </div>;
}
