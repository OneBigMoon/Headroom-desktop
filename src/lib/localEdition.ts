export const LOCAL_COMMUNITY_EDITION = true;
export const LOCAL_COMMUNITY_NAME = "codexbox";
export const LOCAL_COMMUNITY_LABEL = "codexbox - local Codex workspace";

/** The local edition exposes only the Codex client surface. */
export const CODEX_CONNECTOR_ID = "codex";

export function isCodexConnector(clientId: string): boolean {
  return clientId === CODEX_CONNECTOR_ID;
}

export function filterCodexConnectors<T extends { clientId: string }>(
  connectors: readonly T[],
): T[] {
  return connectors.filter((connector) => isCodexConnector(connector.clientId));
}

export function codexLearnInvocation(agent: string):
  | { agent: typeof CODEX_CONNECTOR_ID; projectPath: null }
  | null {
  return agent === CODEX_CONNECTOR_ID
    ? { agent: CODEX_CONNECTOR_ID, projectPath: null }
    : null;
}

/** Remove unsupported client names from shared UI copy, not API identifiers. */
export function codexOnlyUiCopy(value: string): string {
  return value
    .replace(/Claude Code\s+(?:and\/or|and)\s+Codex/gi, "Codex")
    .replace(/Claude Code\s+or\s+Codex/gi, "Codex")
    .replace(/\bClaude Code\b|\bOpenCode\b|\bGrok Build\b|\bZCode\b/gi, "Codex")
    .replace(/\bCodex\s+Codex\b/gi, "Codex");
}
