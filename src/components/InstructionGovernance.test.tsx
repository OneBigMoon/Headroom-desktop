import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { InstructionGovernance } from "./InstructionGovernance";
import { invoke } from "@tauri-apps/api/core";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/i18n", () => ({ useI18n: () => ({ resolvedLocale: "en" }) }));
const report = { target: "codex", path: "/test/AGENTS.md", baseline_hash: "before", candidate_hash: "after", baseline: "old", candidate: "new", findings: ["outdated"], blocked: false };
beforeEach(() => { vi.mocked(invoke).mockReset(); });
describe("instruction governance", () => {
  it("restores the selected persisted snapshot and disables unchanged candidates", async () => {
    vi.mocked(invoke).mockImplementation(async command => command === "audit_instructions" ? [{ ...report, candidate_hash: "before" }] : command === "list_instruction_snapshots" ? [{ id: "saved-id", target: "codex", created_at: "" }] : undefined);
    render(<InstructionGovernance />);
    fireEvent.click(screen.getByText("Read and preview"));
    await screen.findByText("outdated");
    expect((screen.getByText("Back up and apply candidate") as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByText("Restore snapshots"));
    fireEvent.click(screen.getByText("Restore", { exact: true }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("restore_instruction_snapshot", { id: "saved-id" }));
    expect((await screen.findByRole("status")).textContent).toContain("restored and verified");
  });
  it("does not write on preview and applies only the reviewed hashes", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => command === "audit_instructions" ? [report] : command === "list_instruction_snapshots" ? [] : "snapshot-id");
    render(<InstructionGovernance />);
    expect(invoke).not.toHaveBeenCalled();
    fireEvent.click(screen.getByText("Read and preview"));
    await screen.findByText("outdated");
    expect(invoke).not.toHaveBeenCalledWith("apply_instruction_candidate", expect.anything());
    fireEvent.click(screen.getByText("Back up and apply candidate"));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("apply_instruction_candidate", { target: "codex", baselineHash: "before", candidateHash: "after" }));
    await screen.findByRole("status");
  });
  it("blocks malformed candidates and shows backend errors", async () => {
    vi.mocked(invoke).mockImplementation(async command => command === "audit_instructions" ? [{ ...report, blocked: true }] : []);
    render(<InstructionGovernance />); fireEvent.click(screen.getByText("Read and preview"));
    await screen.findByText("outdated");
    expect((screen.getByText("Back up and apply candidate") as HTMLButtonElement).disabled).toBe(true);
    vi.mocked(invoke).mockRejectedValue(new Error("changed file"));
    fireEvent.click(screen.getByText("Read and preview"));
    expect((await screen.findByRole("status")).textContent).toContain("changed file");
  });
});
