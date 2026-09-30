import { describe, expect, it } from "vitest";

import {
  CODEX_CONNECTOR_ID,
  LOCAL_COMMUNITY_NAME,
  codexOnlyUiCopy,
  codexLearnInvocation,
  filterCodexConnectors,
  isCodexConnector,
} from "./localEdition";

describe("codexbox local edition", () => {
  it("uses the codexbox product name", () => {
    expect(LOCAL_COMMUNITY_NAME).toBe("codexbox");
  });

  it("filters shared connector data to Codex without changing the records", () => {
    const connectors = [
      { clientId: "claude_code", enabled: true },
      { clientId: CODEX_CONNECTOR_ID, enabled: false },
      { clientId: "opencode", enabled: true },
    ];

    expect(filterCodexConnectors(connectors)).toEqual([
      { clientId: CODEX_CONNECTOR_ID, enabled: false },
    ]);
    expect(isCodexConnector(CODEX_CONNECTOR_ID)).toBe(true);
    expect(isCodexConnector("grok_build")).toBe(false);
  });

  it("only creates the learn IPC payload for Codex", () => {
    expect(codexLearnInvocation("codex")).toEqual({
      agent: "codex",
      projectPath: null,
    });
    expect(codexLearnInvocation("claude")).toBeNull();
    expect(codexLearnInvocation("opencode")).toBeNull();
    expect(codexLearnInvocation("grok")).toBeNull();
  });

  it("filters unsupported client names only in visible copy", () => {
    expect(codexOnlyUiCopy("Claude Code and/or Codex, OpenCode, Grok Build, ZCode")).toBe(
      "Codex, Codex, Codex, Codex",
    );
  });
});
