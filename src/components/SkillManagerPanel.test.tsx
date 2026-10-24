import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SkillManagerPanel } from "./SkillManagerPanel";
import type { GitSkillScan, SkillCatalog, SkillSummary } from "../lib/extensionManager";

const invokeMock = vi.fn();
const openMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: (...args: unknown[]) => openMock(...args) }));
const sourceRepository = "https://example.com/skills.git";
const skill: SkillSummary = { name: "code-review", description: "Synthetic local skill", skillPath: "/demo/.agents/skills/code-review/SKILL.md", directory: "/demo/.agents/skills/code-review", scope: "global_agents", enabled: true, canToggle: true };
const sourceSkill: SkillSummary = { ...skill, name: "source-review", scope: "local", skillPath: "/source/review/SKILL.md", directory: "/source/review" };
const catalog: SkillCatalog = { skills: [skill], warnings: [], configPath: "/demo/config.toml" };
const repository: GitSkillScan = { skills: [{ name: "pdf", description: "Create and review PDF documents", subdirectory: "skills/.curated/pdf", installDirectory: "/demo/.agents/skills/pdf" }, { name: "spreadsheets", description: "Analyze tabular data", subdirectory: "skills/.curated/spreadsheets", installDirectory: "/demo/.agents/skills/spreadsheets" }], warnings: [] };
const pdfSkill: SkillSummary = { ...skill, name: "pdf", description: repository.skills[0].description, skillPath: "/demo/.agents/skills/pdf/SKILL.md", directory: "/demo/.agents/skills/pdf" };
const card = (name: string) => within(screen.getByRole("heading", { name }).closest("article")!);
const enterDiscover = async () => {
  fireEvent.click(screen.getByRole("tab", { name: "Discover" }));
};
const loadSource = async (name = "pdf") => {
  fireEvent.change(screen.getByLabelText("Public Git repository (HTTPS)"), { target: { value: sourceRepository } });
  fireEvent.click(screen.getByRole("button", { name: "Load repository" }));
  await screen.findByRole("heading", { name });
};


describe("SkillManagerPanel", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    openMock.mockReset();
    openMock.mockResolvedValue(null);
    invokeMock.mockImplementation(async (command) => {
      if (command === "list_skills") return catalog;
      if (command === "scan_git_skills") return repository;
      if (command === "scan_skill_source") return { skills: [sourceSkill], warnings: [] };
      throw new Error(`Unexpected command: ${command}`);
    });
  });

  it("automatically detects installed skills while active without directory or target inputs", async () => {
    const { rerender } = render(<SkillManagerPanel active={false} />);
    expect(invokeMock).not.toHaveBeenCalled();
    rerender(<SkillManagerPanel active />);
    await screen.findByRole("heading", { name: "code-review" });
    expect(invokeMock).toHaveBeenCalledWith("list_skills");
    expect(screen.queryByLabelText(/^Project directory/)).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Import destination")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Skill subdirectory")).not.toBeInTheDocument();
    expect(screen.getByText(/detected automatically/)).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it("keeps the current catalog when a refresh fails", async () => {
    render(<SkillManagerPanel active />);
    await screen.findByRole("heading", { name: "code-review" });
    invokeMock.mockRejectedValueOnce("read failure");
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("current data has been kept");
    expect(screen.getByRole("heading", { name: "code-review" })).toBeInTheDocument();
  });

  it("renders SKILL.md as plain text without executing or rendering HTML", async () => {
    render(<SkillManagerPanel active />);
    await screen.findByRole("heading", { name: "code-review" });
    const content = '# Review\n<script>window.notExecuted=true</script>\n[link](https://example.com)';
    invokeMock.mockResolvedValueOnce({ skillPath: skill.skillPath, content });
    fireEvent.click(screen.getByRole("button", { name: "View SKILL.md" }));
    const reader = await screen.findByRole("region", { name: "SKILL.md content" });
    expect(reader.querySelector("pre")?.textContent).toBe(content);
    expect(reader.querySelector("script")).toBeNull();
    expect(reader.querySelector("a")).toBeNull();
    expect(invokeMock).toHaveBeenLastCalledWith("read_skill", { skillPath: skill.skillPath });
  });

  it("makes no network request until a repository is explicitly loaded and searches locally across tab changes", async () => {
    const user = userEvent.setup();
    render(<SkillManagerPanel active><p>Managed skill packages</p></SkillManagerPanel>);
    await screen.findByRole("heading", { name: "code-review" });
    screen.getByRole("tab", { name: "Installed" }).focus();
    await user.keyboard("{ArrowRight}");
    expect(screen.getByRole("tab", { name: "Discover" })).toHaveFocus();
    expect(screen.getByLabelText("Public Git repository (HTTPS)")).toHaveValue("");
    expect(screen.getByText(/Choose a public repository or a local folder/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Refresh repository" })).not.toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledTimes(1);
    await user.type(screen.getByLabelText("Public Git repository (HTTPS)"), sourceRepository);
    expect(invokeMock).toHaveBeenCalledTimes(1);
    await user.click(screen.getByRole("button", { name: "Load repository" }));
    await screen.findByRole("heading", { name: "pdf" });
    expect(invokeMock).toHaveBeenCalledWith("scan_git_skills", { repositoryUrl: sourceRepository });
    expect(screen.queryByText("Managed skill packages")).not.toBeInTheDocument();
    await user.type(screen.getByLabelText("Search available skills"), "TABULAR");
    expect(screen.queryByRole("heading", { name: "pdf" })).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "spreadsheets" })).toBeInTheDocument();
    await user.click(screen.getByRole("tab", { name: "Installed" }));
    expect(screen.getByText("Managed skill packages")).toBeInTheDocument();
    await user.click(screen.getByRole("tab", { name: "Discover" }));
    expect(screen.getByLabelText("Search available skills")).toHaveValue("TABULAR");
    expect(invokeMock.mock.calls.filter(([command]) => command === "scan_git_skills")).toHaveLength(1);
    await user.click(screen.getByRole("button", { name: "Refresh repository" }));
    await waitFor(() => expect(invokeMock.mock.calls.filter(([command]) => command === "scan_git_skills")).toHaveLength(2));
  });

  it("does not scan an empty repository or fetch when changing tabs", async () => {
    render(<SkillManagerPanel active />);
    await screen.findByRole("heading", { name: "code-review" });
    fireEvent.click(screen.getByRole("tab", { name: "Discover" }));
    const input = screen.getByLabelText("Public Git repository (HTTPS)");
    expect(screen.getByRole("button", { name: "Load repository" })).toBeDisabled();
    fireEvent.submit(input.closest("form")!);
    expect(await screen.findByRole("alert")).toHaveTextContent("public HTTPS repository URL");
    fireEvent.click(screen.getByRole("tab", { name: "Installed" }));
    fireEvent.click(screen.getByRole("tab", { name: "Discover" }));
    expect(invokeMock.mock.calls.filter(([command]) => command === "scan_git_skills")).toHaveLength(0);
  });

  it("allows manual retry after discovery fails without a retry loop", async () => {
    render(<SkillManagerPanel active />);
    await screen.findByRole("heading", { name: "code-review" });
    invokeMock.mockRejectedValueOnce("repository unavailable");
    fireEvent.click(screen.getByRole("tab", { name: "Discover" }));
    fireEvent.change(screen.getByLabelText("Public Git repository (HTTPS)"), { target: { value: sourceRepository } });
    fireEvent.click(screen.getByRole("button", { name: "Load repository" }));
    await screen.findByRole("alert");
    expect(invokeMock.mock.calls.filter(([command]) => command === "scan_git_skills")).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Load repository" }));
    await screen.findByRole("heading", { name: "pdf" });
    expect(invokeMock.mock.calls.filter(([command]) => command === "scan_git_skills")).toHaveLength(2);
  });

  it("installs a discovered skill globally and merges it into the installed list", async () => {
    render(<SkillManagerPanel active />);
    await screen.findByRole("heading", { name: "code-review" });
    await enterDiscover();
    await loadSource();
    invokeMock.mockResolvedValueOnce(pdfSkill);
    fireEvent.click(card("pdf").getByRole("button", { name: "Install" }));
    await waitFor(() => expect(card("pdf").getByRole("button", { name: "Installed" })).toBeDisabled());
    expect(invokeMock).toHaveBeenLastCalledWith("import_git_skill", { repositoryUrl: sourceRepository, subdirectory: "skills/.curated/pdf", target: "global" });
    expect(screen.getByRole("status")).toHaveTextContent("Imported 1; failed 0.");
    expect(screen.getByRole("status")).toHaveTextContent("new Codex session");
    fireEvent.click(screen.getByRole("tab", { name: "Installed" }));
    expect(screen.getByRole("heading", { name: "pdf" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "code-review" })).toBeInTheDocument();
  });

  it("allows a same-name skill in another directory and blocks a different name at the same destination", async () => {
    const existing = { ...skill, directory: "/demo/.agents/skills/review-v1", skillPath: "/demo/.agents/skills/review-v1/SKILL.md" };
    const next = { ...existing, directory: "/demo/.agents/skills/review-v2", skillPath: "/demo/.agents/skills/review-v2/SKILL.md" };
    const sameName = { name: skill.name, description: "Another review skill", subdirectory: "skills/review-v2", installDirectory: next.directory };
    const sameDestination = { name: "renamed-review", description: "Already occupied destination", subdirectory: "skills/review-v1", installDirectory: existing.directory };
    invokeMock.mockResolvedValueOnce({ ...catalog, skills: [existing] });
    render(<SkillManagerPanel active />);
    await screen.findByRole("heading", { name: "code-review" });
    invokeMock.mockResolvedValueOnce({ skills: [sameName, sameDestination], warnings: [] });
    fireEvent.click(screen.getByRole("tab", { name: "Discover" }));
    await loadSource("renamed-review");
    expect(card("code-review").getByRole("button", { name: "Install" })).toBeEnabled();
    expect(card("renamed-review").getByRole("button", { name: "Installed" })).toBeDisabled();
    invokeMock.mockResolvedValueOnce(next);
    fireEvent.click(card("code-review").getByRole("button", { name: "Install" }));
    await waitFor(() => expect(card("code-review").getByRole("button", { name: "Installed" })).toBeDisabled());
    expect(invokeMock).toHaveBeenLastCalledWith("import_git_skill", { repositoryUrl: sourceRepository, subdirectory: "skills/review-v2", target: "global" });
    fireEvent.click(screen.getByRole("tab", { name: "Installed" }));
    expect(screen.getAllByRole("heading", { name: "code-review" })).toHaveLength(2);
  });

  it("loads a custom repository explicitly and binds installs to the scanned repository", async () => {
    const user = userEvent.setup();
    render(<SkillManagerPanel active />);
    await screen.findByRole("heading", { name: "code-review" });
    await enterDiscover();
    await loadSource();
    const input = screen.getByLabelText("Public Git repository (HTTPS)");
    await user.clear(input);
    await user.type(input, "https://example.com/public/skills.git");
    expect(invokeMock.mock.calls.filter(([command]) => command === "scan_git_skills")).toHaveLength(1);
    await user.click(screen.getByRole("button", { name: "Load repository" }));
    await waitFor(() => expect(invokeMock).toHaveBeenLastCalledWith("scan_git_skills", { repositoryUrl: "https://example.com/public/skills.git" }));
    await user.clear(input);
    await user.type(input, "https://example.com/unscanned/skills.git");
    invokeMock.mockResolvedValueOnce(pdfSkill);
    await user.click(card("pdf").getByRole("button", { name: "Install" }));
    await waitFor(() => expect(invokeMock).toHaveBeenLastCalledWith("import_git_skill", { repositoryUrl: "https://example.com/public/skills.git", subdirectory: "skills/.curated/pdf", target: "global" }));
  });

  it("rejects non-HTTPS custom repositories without sending a request", async () => {
    const user = userEvent.setup();
    render(<SkillManagerPanel active />);
    await screen.findByRole("heading", { name: "code-review" });
    await enterDiscover();
    const input = screen.getByLabelText("Public Git repository (HTTPS)");
    await user.clear(input);
    await user.type(input, "http://example.com/skills.git");
    await user.click(screen.getByRole("button", { name: "Load repository" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("public HTTPS repository URL");
    expect(invokeMock.mock.calls.filter(([command]) => command === "scan_git_skills")).toHaveLength(0);
  });

  it("does not scan or report a failure when the folder picker is canceled", async () => {
    render(<SkillManagerPanel active />);
    await screen.findByRole("heading", { name: "code-review" });
    await enterDiscover();
    fireEvent.click(screen.getByRole("button", { name: "Import local folder" }));
    await waitFor(() => expect(openMock).toHaveBeenCalledWith({ directory: true, multiple: false }));
    await waitFor(() => expect(screen.queryByRole("status")).not.toBeInTheDocument());
    expect(invokeMock.mock.calls.some(([command]) => command === "scan_skill_source")).toBe(false);
    expect(invokeMock.mock.calls.some(([command]) => command === "scan_git_skills")).toBe(false);
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("scans a picked folder automatically and retains failed selections without a path input", async () => {
    const user = userEvent.setup();
    render(<SkillManagerPanel active />);
    await screen.findByRole("heading", { name: "code-review" });
    await enterDiscover();
    openMock.mockResolvedValueOnce("/source");
    await user.click(screen.getByRole("button", { name: "Import local folder" }));
    await user.click(await screen.findByRole("checkbox", { name: "source-review" }));
    expect(invokeMock).toHaveBeenLastCalledWith("scan_skill_source", { sourcePath: "/source" });
    expect(screen.queryByLabelText("Local source directory")).not.toBeInTheDocument();
    invokeMock.mockRejectedValueOnce("目标技能已存在；不会覆盖：/fixture/review");
    await user.click(screen.getByRole("button", { name: "Import selected" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("conflicting entry");
    expect(screen.getByRole("checkbox", { name: "source-review" })).toBeChecked();
    expect(screen.getByText("Local source directory: /source")).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Imported 0; failed 1.");
    expect(invokeMock).toHaveBeenLastCalledWith("import_skill", { sourcePath: sourceSkill.directory, target: "global" });
  });

  it("keeps only failed selections after a partial local import and preserves existing installed skills", async () => {
    const user = userEvent.setup();
    const second = { ...sourceSkill, name: "second-source", directory: "/source/second", skillPath: "/source/second/SKILL.md" };
    render(<SkillManagerPanel active />);
    await screen.findByRole("heading", { name: "code-review" });
    await enterDiscover();
    openMock.mockResolvedValueOnce("/source");
    invokeMock.mockResolvedValueOnce({ skills: [sourceSkill, second], warnings: [] });
    await user.click(screen.getByRole("button", { name: "Import local folder" }));
    await user.click(await screen.findByRole("checkbox", { name: "source-review" }));
    await user.click(screen.getByRole("checkbox", { name: "second-source" }));
    invokeMock.mockResolvedValueOnce({ ...sourceSkill, skillPath: "/demo/imported/SKILL.md", scope: "global_agents" }).mockRejectedValueOnce("already exists");
    await user.click(screen.getByRole("button", { name: "Import selected" }));
    await screen.findByRole("alert");
    expect(screen.getByRole("checkbox", { name: "source-review" })).not.toBeChecked();
    expect(screen.getByRole("checkbox", { name: "second-source" })).toBeChecked();
    expect(screen.getByRole("status")).toHaveTextContent("Imported 1; failed 1.");
    expect(screen.getByRole("status")).toHaveTextContent("Failed items remain selected");
    await user.click(screen.getByRole("tab", { name: "Installed" }));
    expect(screen.getByRole("heading", { name: "source-review" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "code-review" })).toBeInTheDocument();
  });

  it("filters installed skills locally without rescanning", async () => {
    const user = userEvent.setup();
    const other = { ...skill, name: "docs", description: "Documentation helper", skillPath: "/other/SKILL.md", directory: "/other" };
    invokeMock.mockResolvedValueOnce({ ...catalog, skills: [skill, other] });
    render(<SkillManagerPanel active />);
    await screen.findByRole("heading", { name: "code-review" });
    await user.type(screen.getByLabelText("Search installed skills"), "DOCUMENTATION");
    expect(screen.getByRole("heading", { name: "docs" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "code-review" })).not.toBeInTheDocument();
    await user.clear(screen.getByLabelText("Search installed skills"));
    await user.type(screen.getByLabelText("Search installed skills"), "/demo/.agents");
    expect(screen.getByRole("heading", { name: "code-review" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "docs" })).not.toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it("waits for Codex enable changes and does not duplicate pending operations", async () => {
    let finish!: (value: SkillSummary) => void;
    render(<SkillManagerPanel active />);
    await screen.findByRole("heading", { name: "code-review" });
    invokeMock.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    const toggle = screen.getByRole("switch", { name: "Disable: Codex" });
    fireEvent.click(toggle);
    fireEvent.click(toggle);
    expect(toggle).toBeDisabled();
    expect(toggle).toHaveAttribute("aria-checked", "true");
    expect(invokeMock.mock.calls.filter(([name]) => name === "set_skill_enabled")).toHaveLength(1);
    await act(async () => finish({ ...skill, enabled: false }));
    expect(screen.getByRole("switch", { name: "Enable: Codex" })).toBeEnabled();
    expect(screen.getByRole("switch", { name: "Enable: Codex" })).toHaveAttribute("aria-checked", "false");
  });
});
