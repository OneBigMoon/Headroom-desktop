import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { OptimizePanel } from "./OptimizePanel";
import type { AppliedPatterns } from "../lib/types";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args)
}));

const samplePatterns: AppliedPatterns = {
  claudeMd: [
    {
      title: "Edit Tool Rules",
      bullets: [
        "Always Read a file before attempting to Edit it.",
        "Re-Read after external linter mutations to avoid stale-edit errors."
      ]
    }
  ],
  memoryMd: [
    {
      title: "Subagent Usage",
      bullets: [
        "Prefer inline Grep/Read/Glob over spawning Explore for single-file lookups."
      ]
    }
  ]
};

describe("OptimizePanel", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it("renders pill counts derived from preloadedApplied without invoking IPC", () => {
    render(
      <OptimizePanel projectPath="/proj" preloadedApplied={samplePatterns} />
    );

    expect(
      screen.getByRole("button", { name: /2 learnings in CLAUDE.local\.md/i })
    ).toBeEnabled();
    expect(
      screen.getByRole("button", { name: /1 reminder in MEMORY\.md/i })
    ).toBeEnabled();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("renders Codex AGENTS.md and instructions.md results", async () => {
    const codexPatterns: AppliedPatterns = {
      claudeMd: [],
      memoryMd: [],
      codexAgentsMd: [
        { title: "Tool discipline", bullets: ["Read once before editing."] }
      ],
      codexInstructionsMd: [
        { title: "Retry discipline", bullets: ["Retry only after state changes."] }
      ]
    };
    render(
      <OptimizePanel
        projectPath="codex"
        source="codex"
        preloadedApplied={codexPatterns}
      />
    );
    const user = userEvent.setup();

    expect(
      screen.getByRole("button", { name: /codexbox learnings in AGENTS\.md: 1/i })
    ).toBeEnabled();
    expect(
      screen.getByRole("button", { name: /codexbox reminders in instructions\.md: 1/i })
    ).toBeEnabled();

    await user.click(
      screen.getByRole("button", { name: /codexbox learnings in AGENTS\.md: 1/i })
    );
    expect(await screen.findByRole("dialog")).toHaveTextContent(
      "Learnings in ~/.codex/AGENTS.md"
    );
    expect(screen.getByText("Read once before editing.")).toBeInTheDocument();
  });

  it("disables pills when the preloaded counts are zero", () => {
    render(
      <OptimizePanel
        projectPath="/proj"
        preloadedApplied={{ claudeMd: [], memoryMd: [] }}
      />
    );

    expect(
      screen.getByRole("button", { name: /0 learnings in CLAUDE.local\.md/i })
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: /0 reminders in MEMORY\.md/i })
    ).toBeDisabled();
  });

  it("collapses to a single 'not scanned yet' pill when Learn has never run", () => {
    render(
      <OptimizePanel
        projectPath="/proj"
        preloadedApplied={{ claudeMd: [], memoryMd: [] }}
        neverScanned
      />
    );

    expect(screen.getByRole("button", { name: /not scanned yet/i })).toBeDisabled();
    expect(
      screen.queryByRole("button", { name: /learnings in CLAUDE.local\.md/i })
    ).toBeNull();
    expect(
      screen.queryByRole("button", { name: /reminders in MEMORY\.md/i })
    ).toBeNull();
  });

  it("opens the CLAUDE.md modal with the section list when the pill is clicked", async () => {
    render(
      <OptimizePanel projectPath="/proj" preloadedApplied={samplePatterns} />
    );
    const user = userEvent.setup();

    await user.click(
      screen.getByRole("button", { name: /2 learnings in CLAUDE.local\.md/i })
    );

    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("Learnings in CLAUDE.local.md");
    expect(dialog).toHaveTextContent("Edit Tool Rules");
    expect(dialog).toHaveTextContent(
      "Always Read a file before attempting to Edit it."
    );
  });

  it("renders a loading state when no preloaded data is supplied and IPC has not resolved", () => {
    invokeMock.mockReturnValue(new Promise(() => {})); // never resolves
    render(<OptimizePanel projectPath="/proj" />);

    // Pills land in disabled (count=0) state until applied loads.
    expect(
      screen.getByRole("button", { name: /0 learnings in CLAUDE.local\.md/i })
    ).toBeDisabled();
    expect(invokeMock).toHaveBeenCalledWith("list_applied_patterns", {
      projectPath: "/proj"
    });
  });

  it("refetches applied results when a completed Learn run changes refreshSignal", async () => {
    invokeMock
      .mockResolvedValueOnce({ claudeMd: [], memoryMd: [] })
      .mockResolvedValueOnce(samplePatterns);

    const { rerender } = render(<OptimizePanel projectPath="/proj" refreshSignal={0} />);
    expect(
      await screen.findByRole("button", { name: /0 learnings in CLAUDE\.local\.md/i }),
    ).toBeDisabled();
    expect(invokeMock).toHaveBeenCalledTimes(1);

    rerender(<OptimizePanel projectPath="/proj" refreshSignal={1} />);

    expect(
      await screen.findByRole("button", { name: /2 learnings in CLAUDE\.local\.md/i }),
    ).toBeEnabled();
    expect(invokeMock).toHaveBeenLastCalledWith("list_applied_patterns", { projectPath: "/proj" });
  });

  it("offers a retry when the first load fails", async () => {
    // Without this the pills stay disabled at "0 learnings" forever: the modal
    // that holds the error cannot be opened, so an IPC failure is invisible.
    invokeMock
      .mockRejectedValueOnce(new Error("list_applied_patterns: no such project"))
      .mockResolvedValueOnce(samplePatterns);

    render(<OptimizePanel projectPath="/proj" />);
    const user = userEvent.setup();

    const pill = await screen.findByRole("button", { name: /could not load learnings.*retry/i });
    expect(pill).toHaveAttribute("title", "list_applied_patterns: no such project");
    expect(pill).toBeEnabled();

    await user.click(pill);

    expect(
      await screen.findByRole("button", { name: /2 learnings in CLAUDE.local\.md/i })
    ).toBeEnabled();
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });

  it("surfaces a delete-pattern failure inside the open modal", async () => {
    invokeMock.mockRejectedValueOnce(new Error("write CLAUDE.local.md: permission denied"));

    render(
      <OptimizePanel projectPath="/proj" preloadedApplied={samplePatterns} />
    );
    const user = userEvent.setup();

    await user.click(
      screen.getByRole("button", { name: /2 learnings in CLAUDE.local\.md/i })
    );
    const deleteButtons = await screen.findAllByRole("button", { name: /^Delete$/ });
    await user.click(deleteButtons[0]);

    expect(
      await screen.findByText(/write CLAUDE.local\.md: permission denied/i)
    ).toBeInTheDocument();
  });

  it("calls delete_applied_pattern with the right args and reports busy state", async () => {
    let resolveDelete: () => void = () => {};
    invokeMock.mockImplementation((command: string) => {
      if (command === "delete_applied_pattern") {
        return new Promise<void>((resolve) => {
          resolveDelete = resolve;
        });
      }
      throw new Error(`unexpected invoke: ${command}`);
    });

    const onAppliedMutated = vi.fn();
    render(
      <OptimizePanel
        projectPath="/proj"
        preloadedApplied={samplePatterns}
        onAppliedMutated={onAppliedMutated}
      />
    );
    const user = userEvent.setup();

    await user.click(
      screen.getByRole("button", { name: /2 learnings in CLAUDE.local\.md/i })
    );

    const deleteButtons = await screen.findAllByRole("button", { name: /^Delete$/ });
    expect(deleteButtons.length).toBeGreaterThan(0);
    await user.click(deleteButtons[0]);

    expect(invokeMock).toHaveBeenCalledWith("delete_applied_pattern", {
      projectPath: "/proj",
      fileKind: "claude",
      sectionTitle: "Edit Tool Rules",
      bulletText: "Always Read a file before attempting to Edit it."
    });

    // While the IPC is in flight the row's button should show the busy label.
    expect(await screen.findByRole("button", { name: /^…$/ })).toBeDisabled();

    // Resolve and confirm the parent was notified (preloaded path).
    resolveDelete();
    await waitFor(() => expect(onAppliedMutated).toHaveBeenCalled());
  });

  it("expands and collapses long bullets via the Show more / Show less toggle", async () => {
    const longBullet = "a".repeat(200);
    const longPatterns: AppliedPatterns = {
      claudeMd: [{ title: "Verbose Section", bullets: [longBullet] }],
      memoryMd: []
    };
    render(
      <OptimizePanel projectPath="/proj" preloadedApplied={longPatterns} />
    );
    const user = userEvent.setup();

    await user.click(
      screen.getByRole("button", { name: /1 learning in CLAUDE.local\.md/i })
    );

    const expand = await screen.findByRole("button", { name: /^Show more$/ });
    await user.click(expand);
    expect(
      await screen.findByRole("button", { name: /^Show less$/ })
    ).toBeInTheDocument();
  });

  it("closes the modal when the close button is pressed", async () => {
    render(
      <OptimizePanel projectPath="/proj" preloadedApplied={samplePatterns} />
    );
    const user = userEvent.setup();

    await user.click(
      screen.getByRole("button", { name: /2 learnings in CLAUDE.local\.md/i })
    );
    expect(await screen.findByRole("dialog")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Close/i }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument()
    );
  });

  it("closes the modal when the backdrop is clicked", async () => {
    render(
      <OptimizePanel projectPath="/proj" preloadedApplied={samplePatterns} />
    );
    const user = userEvent.setup();

    await user.click(
      screen.getByRole("button", { name: /2 learnings in CLAUDE.local\.md/i })
    );
    const dialog = await screen.findByRole("dialog");
    // The backdrop is the dialog element itself; clicking outside the inner
    // card (i.e. on the dialog directly) closes it.
    await user.click(dialog);
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument()
    );
  });
});
