import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { McpManagerPanel } from "./McpManagerPanel";
import type { McpServer } from "../lib/extensionManager";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }));
const server: McpServer = { id: "local-search", transport: "stdio", enabled: true, command: "search-tool", args: ["--stdio"], env: { DEMO_ENV: "synthetic-value" }, envVars: [], url: null, bearerTokenEnvVar: null, httpHeaders: {}, editable: true, managedBy: null, readonlyReason: null };

describe("McpManagerPanel", () => {
  beforeEach(() => { invokeMock.mockReset(); invokeMock.mockResolvedValue([server]); });

  it("reads only while active and preserves list on refresh failure", async () => {
    const { rerender } = render(<McpManagerPanel active={false}><p>built-in cards</p></McpManagerPanel>);
    expect(invokeMock).not.toHaveBeenCalled();
    rerender(<McpManagerPanel active><p>built-in cards</p></McpManagerPanel>);
    await screen.findByRole("heading", { name: "local-search" });
    invokeMock.mockRejectedValueOnce("read failed");
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("current data has been kept");
    expect(screen.getByRole("heading", { name: "local-search" })).toBeInTheDocument();
    expect(screen.getByText("built-in cards")).toBeInTheDocument();
    expect(screen.queryByText("synthetic-value")).not.toBeInTheDocument();
  });

  it("creates stdio entries with exact arguments and masked environment values", async () => {
    const user = userEvent.setup();
    render(<McpManagerPanel active />);
    await screen.findByRole("heading", { name: "local-search" });
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await user.type(screen.getByLabelText("Name"), "custom-tool");
    await user.type(screen.getByLabelText("Command"), "my-command");
    fireEvent.change(screen.getByLabelText("Arguments (JSON array)"), { target: { value: '["", "arg with spaces"]' } });
    await user.click(screen.getByRole("button", { name: "Add variable" }));
    await user.type(screen.getByLabelText("Environment values: Variable 1"), "DEMO");
    const password = screen.getByLabelText("Environment values: Value 1");
    expect(password).toHaveAttribute("type", "password");
    await user.type(password, "synthetic-input");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("save_mcp_server", { request: { originalId: null, server: { id: "custom-tool", transport: "stdio", enabled: true, command: "my-command", args: ["", "arg with spaces"], env: { DEMO: "synthetic-input" }, envVars: [], url: null, bearerTokenEnvVar: null, httpHeaders: {} } } }));
  });

  it("retains the edit draft and original server when a rename conflicts", async () => {
    const user = userEvent.setup();
    render(<McpManagerPanel active />);
    await screen.findByRole("heading", { name: "local-search" });
    await user.click(screen.getByRole("button", { name: "Edit" }));
    await user.clear(screen.getByLabelText("Name"));
    await user.type(screen.getByLabelText("Name"), "taken-name");
    invokeMock.mockRejectedValueOnce("MCP server already exists");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("conflicting entry");
    expect(screen.getByLabelText("Name")).toHaveValue("taken-name");
    expect(screen.getByLabelText("Environment values: Value 1")).toHaveValue("synthetic-value");
    expect(screen.getByRole("heading", { name: "local-search" })).toBeInTheDocument();
    expect(invokeMock).toHaveBeenLastCalledWith("save_mcp_server", expect.objectContaining({ request: expect.objectContaining({ originalId: "local-search" }) }));
  });

  it("uses HTTP fields and hides header values", async () => {
    const user = userEvent.setup();
    render(<McpManagerPanel active />);
    await screen.findByRole("heading", { name: "local-search" });
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await user.type(screen.getByLabelText("Name"), "remote");
    fireEvent.change(screen.getByLabelText("Arguments (JSON array)"), { target: { value: "invalid hidden stdio arguments" } });
    await user.selectOptions(screen.getByLabelText("Transport"), "http");
    await user.type(screen.getByLabelText("Server URL"), "https://example.com/mcp");
    await user.type(screen.getByLabelText("Bearer token environment variable"), "REMOTE_TOKEN");
    await user.click(screen.getByRole("button", { name: "Add header" }));
    await user.type(screen.getByLabelText("HTTP headers: Variable 1"), "X-Demo");
    const value = screen.getByLabelText("HTTP headers: Value 1");
    expect(value).toHaveAttribute("type", "password");
    await user.type(value, "synthetic-header");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("save_mcp_server", expect.objectContaining({ request: { originalId: null, server: { id: "remote", transport: "http", enabled: true, command: null, args: [], env: {}, envVars: [], url: "https://example.com/mcp", bearerTokenEnvVar: "REMOTE_TOKEN", httpHeaders: { "X-Demo": "synthetic-header" } } } })));
    expect(screen.getByRole("status")).toHaveTextContent("Configuration saved.");
    expect(screen.getByRole("status")).toHaveTextContent("Reload MCPs in Codex");
  });

  it("validates JSON locally and retains a conflicting import", async () => {
    const user = userEvent.setup();
    render(<McpManagerPanel active />);
    await screen.findByRole("heading", { name: "local-search" });
    await user.click(screen.getByRole("button", { name: "Import JSON" }));
    fireEvent.change(screen.getByLabelText("Import JSON"), { target: { value: "invalid" } });
    await user.click(screen.getByRole("button", { name: "Import" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("valid JSON");
    expect(invokeMock).toHaveBeenCalledTimes(1);
    const json = '{"mcpServers":{"local-search":{"command":"demo"}}}';
    fireEvent.change(screen.getByLabelText("Import JSON"), { target: { value: json } });
    expect(screen.getByRole("region", { name: "Import preview" })).toHaveTextContent("Name already exists");
    expect(screen.getByRole("button", { name: "Import" })).toBeDisabled();
    expect(invokeMock).toHaveBeenCalledTimes(1);
    const changed = '{"mcpServers":{"created-elsewhere":{"command":"demo"}}}';
    fireEvent.change(screen.getByLabelText("Import JSON"), { target: { value: changed } });
    invokeMock.mockRejectedValueOnce("already exists");
    await user.click(screen.getByRole("button", { name: "Import" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("conflicting entry");
    expect(screen.getByLabelText("Import JSON")).toHaveValue(changed);
  });

  it("previews only server names and transports and reports a successful import count", async () => {
    const user = userEvent.setup();
    render(<McpManagerPanel active />);
    await screen.findByRole("heading", { name: "local-search" });
    await user.click(screen.getByRole("button", { name: "Import JSON" }));
    const json = JSON.stringify({ mcpServers: {
      remote: { url: "https://example.invalid/mcp", headers: { Authorization: "fixture-header-secret" } },
      local: { command: "fixture-command", env: { TOKEN: "fixture-env-secret" } },
    } });
    fireEvent.change(screen.getByLabelText("Import JSON"), { target: { value: json } });
    const preview = screen.getByRole("region", { name: "Import preview" });
    expect(preview).toHaveTextContent("remote");
    expect(preview).toHaveTextContent("HTTP");
    expect(preview).toHaveTextContent("stdio");
    expect(preview).not.toHaveTextContent("fixture-header-secret");
    expect(preview).not.toHaveTextContent("fixture-env-secret");
    expect(preview).not.toHaveTextContent("https://example.invalid/mcp");
    invokeMock.mockResolvedValueOnce([server, { ...server, id: "remote" }, { ...server, id: "local" }]);
    await user.click(screen.getByRole("button", { name: "Import" }));
    expect(screen.getByRole("status")).toHaveTextContent("Imported 2 MCP server(s).");
    expect(screen.getByRole("status")).toHaveTextContent("Reload MCPs in Codex");
    expect(screen.queryByRole("region", { name: "Import preview" })).not.toBeInTheDocument();
  });

  it("searches server names, commands and URLs locally without searching credential values", async () => {
    const user = userEvent.setup();
    invokeMock.mockResolvedValueOnce([server, { ...server, id: "remote-docs", command: null, transport: "http", url: "https://docs.example.invalid/mcp" }]);
    render(<McpManagerPanel active />);
    await screen.findByRole("heading", { name: "local-search" });
    await user.type(screen.getByLabelText("Search MCP servers"), "DOCS.EXAMPLE");
    expect(screen.getByRole("heading", { name: "remote-docs" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "local-search" })).not.toBeInTheDocument();
    await user.clear(screen.getByLabelText("Search MCP servers"));
    await user.type(screen.getByLabelText("Search MCP servers"), "synthetic-value");
    expect(screen.getByText("No matching entries.")).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it("requires in-app confirmation for deletion", async () => {
    const user = userEvent.setup();
    render(<McpManagerPanel active />);
    await screen.findByRole("heading", { name: "local-search" });
    await user.click(screen.getByRole("button", { name: "Delete" }));
    let dialog = screen.getByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(invokeMock).not.toHaveBeenCalledWith("delete_mcp_server", expect.anything());
    await user.click(screen.getByRole("button", { name: "Delete" }));
    dialog = screen.getByRole("dialog");
    invokeMock.mockResolvedValueOnce([]);
    await user.click(within(dialog).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("delete_mcp_server", { id: "local-search" }));
  });

  it("waits for enable changes and prevents duplicate mutations", async () => {
    let finish!: (value: McpServer[]) => void;
    render(<McpManagerPanel active />);
    await screen.findByRole("heading", { name: "local-search" });
    invokeMock.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    const button = screen.getByRole("switch", { name: "Disable: Codex" });
    fireEvent.click(button);
    fireEvent.click(button);
    expect(button).toBeDisabled();
    expect(button).toBeChecked();
    expect(invokeMock.mock.calls.filter(([name]) => name === "set_mcp_server_enabled")).toHaveLength(1);
    await act(async () => finish([{ ...server, enabled: false }]));
    expect(screen.getByRole("switch", { name: "Enable: Codex" })).toBeEnabled();
    expect(screen.getByRole("switch")).not.toBeChecked();
  });

  it("keeps the prior enabled state when the toggle fails", async () => {
    const user = userEvent.setup();
    render(<McpManagerPanel active />);
    await screen.findByRole("heading", { name: "local-search" });
    invokeMock.mockRejectedValueOnce("synthetic mutation failure");

    await user.click(screen.getByRole("switch", { name: "Disable: Codex" }));

    expect(await screen.findByRole("alert")).toBeInTheDocument();
    const toggle = screen.getByRole("switch", { name: "Disable: Codex" });
    expect(toggle).toBeChecked();
    expect(toggle).toBeEnabled();
  });

  it("keeps managed and unsupported entries read-only", async () => {
    invokeMock.mockResolvedValue([{ ...server, editable: false, managedBy: "headroom" }, { ...server, id: "reserved", editable: false, managedBy: "reserved" }]);
    render(<McpManagerPanel active />);
    await screen.findByText("Managed by CodexBox. Use the tool cards below.");
    expect(screen.getByText(/Edit it in the Codex configuration/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Edit" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Delete" })).not.toBeInTheDocument();
  });
});
