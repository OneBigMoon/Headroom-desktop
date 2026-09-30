import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { JevPanel, type JevRecord } from "./JevPanel";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }));

const deepseekUrl = "https://api.deepseek.com/chat/completions";
const record: JevRecord = {
  id: "r1", timestamp: "2026-09-22T10:00:00.000Z", provider: "deepseek", endpoint: deepseekUrl,
  model: "deepseek-chat", status: "success", latencyMs: 120, inputTokens: 10,
  outputTokens: 4, choice: "coding", confidence: null, error: null,
};
const dashboard = {
  keyConfigured: false,
  keyPersistent: true,
  configuredTargets: [{ provider: "deepseek", protocol: "open_ai", url: deepseekUrl }],
  busy: false,
  records: [record],
};

const providerTriggerName = /TypeSafe Jev|自定义模型|DeepSeek|火山引擎|MiniMax|BigModel|阿里云百炼|Xiaomi MiMo|硅基流动|Z\.ai|OpenRouter|Kimi|BytePlus|AWS Bedrock|腾讯|ModelScope|PPIO|Anthropic|Google Gemini|OpenCode Go/;

async function chooseProvider(user: ReturnType<typeof userEvent.setup>, label: string) {
  await user.click(screen.getByRole("button", { name: providerTriggerName }));
  const dialog = screen.getByRole("dialog", { name: "选择提供方" });
  await user.click(within(dialog).getByRole("radio", { name: label }));
}

describe("JevPanel", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(dashboard);
  });

  it("loads dashboard without sending text or keys", async () => {
    render(<JevPanel />);
    await screen.findByLabelText("待评估文本");
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("get_jev_dashboard"));
    expect(invokeMock).not.toHaveBeenCalledWith("evaluate_jev", expect.anything());
    expect(invokeMock).not.toHaveBeenCalledWith("set_adapter_key", expect.anything());
  });

  it("distinguishes unreported native output tokens from a reported zero", async () => {
    invokeMock.mockResolvedValue({
      ...dashboard,
      keyConfigured: true,
      records: [
        { ...record, id: "missing", provider: "typesafe", endpoint: null, model: "jev-latest", outputTokens: null },
        { ...record, id: "zero", provider: "typesafe", endpoint: null, model: "jev-latest", outputTokens: 0 },
      ],
    });
    render(<JevPanel />);

    expect(await screen.findByText("1/2 条有数据；未返回不计为 0")).toBeInTheDocument();
    expect(screen.getByText("输出 token（已上报）").parentElement).toHaveTextContent("0");
    expect(screen.getByRole("cell", { name: "未返回" })).toBeInTheDocument();
  });

  it("requires consent and sends exact DeepSeek configuration", async () => {
    render(<JevPanel />);
    const user = userEvent.setup();
    await chooseProvider(user, "DeepSeek");
    await user.type(await screen.findByLabelText("待评估文本"), "fix this code");
    expect(screen.getByRole("button", { name: "发送并评估" })).toBeDisabled();
    await user.click(screen.getByLabelText(/我同意/));
    await user.click(screen.getByRole("button", { name: "发送并评估" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("evaluate_jev", {
      text: "fix this code",
      config: { provider: "deepseek", protocol: null, url: null, model: "deepseek-chat" },
    }));
  });

  it("defaults to an already configured TypeSafe key without asking for a second one", async () => {
    invokeMock.mockResolvedValue({ ...dashboard, keyConfigured: true, configuredTargets: [] });
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("待评估文本");
    expect(screen.getByRole("button", { name: "TypeSafe Jev（原生）" })).toBeInTheDocument();
    expect(screen.getByText("TypeSafe 官方 Key 已连接，无需重复设置。")).toBeInTheDocument();
    expect(screen.getByText(/TypeSafe 官方 Key 保存在 macOS 钥匙串，重启后自动恢复/)).toBeInTheDocument();
    expect(screen.queryByLabelText("TypeSafe Key")).toBeNull();
    expect(screen.getByText("已配置 Key")).toBeInTheDocument();
    await user.click(screen.getByLabelText(/我同意/));
    await chooseProvider(user, "DeepSeek");
    expect(screen.getByLabelText(/我同意/)).not.toBeChecked();
    expect(screen.getByRole("button", { name: "发送并评估" })).toBeDisabled();
    expect(screen.getByText("未配置 Key")).toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalledWith("evaluate_jev", expect.anything());
  });

  it("sets a new TypeSafe key only through the native command", async () => {
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("TypeSafe Key");
    expect(screen.getByPlaceholderText("保存到系统钥匙串")).toBeInTheDocument();
    await user.type(screen.getByLabelText("TypeSafe Key"), "typesafe-secret");
    await user.click(screen.getByRole("button", { name: "设置 key" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("set_jev_key", { key: "typesafe-secret" }));
    expect(invokeMock).not.toHaveBeenCalledWith("set_adapter_key", expect.anything());
    expect(localStorage.getItem("typesafe-secret")).toBeNull();
  });

  it("labels session-only builds honestly", async () => {
    invokeMock.mockResolvedValue({ ...dashboard, keyPersistent: false });
    render(<JevPanel />);
    await screen.findByLabelText("TypeSafe Key");
    expect(screen.getByPlaceholderText("仅保留在当前会话")).toBeInTheDocument();
    expect(screen.getByText(/此构建的 TypeSafe Key 只保留在当前运行期间/)).toBeInTheDocument();
  });

  it("reports a failed TypeSafe key save without claiming the key is configured", async () => {
    invokeMock.mockImplementation((command: string) => command === "set_jev_key"
      ? Promise.reject("jev_key_store_write")
      : Promise.resolve(dashboard));
    render(<JevPanel />);
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText("TypeSafe Key"), "test-secret");
    await user.click(screen.getByRole("button", { name: "设置 key" }));
    expect(await screen.findByText("TypeSafe Key 未保存，请检查系统钥匙串是否可用，然后重试。")).toBeInTheDocument();
    expect(screen.getByLabelText("TypeSafe Key")).toHaveValue("test-secret");
    expect(screen.getByText("未配置 Key")).toBeInTheDocument();
  });

  it("sends a custom Anthropic target and never reuses its key after URL changes", async () => {
    const customUrl = "https://custom.example/v1/messages";
    let keySaved = false;
    invokeMock.mockImplementation((command: string) => {
      if (command === "set_adapter_key") {
        keySaved = true;
        return Promise.resolve(undefined);
      }
      if (command === "get_jev_dashboard") {
        return Promise.resolve({
          ...dashboard,
          configuredTargets: keySaved ? [{ provider: "custom", protocol: "anthropic", url: customUrl }] : [],
        });
      }
      return Promise.resolve(dashboard);
    });
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("待评估文本");
    await chooseProvider(user, "自定义模型");
    await user.selectOptions(screen.getByLabelText("接口协议"), "anthropic");
    await user.type(screen.getByLabelText(/请求 URL/), customUrl);
    await user.type(screen.getByLabelText("模型 ID"), "my-model");
    await user.type(screen.getByLabelText("提供方 API Key"), "secret");
    await user.click(screen.getByRole("button", { name: "设置 key" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("set_adapter_key", {
      config: { provider: "custom", protocol: "anthropic", url: customUrl, model: "my-model" }, key: "secret",
    }));
    await user.type(screen.getByLabelText("待评估文本"), "research this");
    await user.click(screen.getByLabelText(/我同意/));
    await user.click(screen.getByRole("button", { name: "发送并评估" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("evaluate_jev", {
      text: "research this", config: { provider: "custom", protocol: "anthropic", url: customUrl, model: "my-model" },
    }));
    fireEvent.change(screen.getByLabelText(/请求 URL/), { target: { value: "https://other.example/v1/messages" } });
    expect(screen.getByLabelText(/我同意/)).not.toBeChecked();
    expect(screen.getByRole("button", { name: "发送并评估" })).toBeDisabled();
  });

  it("rejects unsafe remote HTTP and permits unauthenticated localhost", async () => {
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("待评估文本");
    await chooseProvider(user, "自定义模型");
    await user.type(screen.getByLabelText("模型 ID"), "local-model");
    fireEvent.change(screen.getByLabelText(/请求 URL/), { target: { value: "http://remote.example/v1/chat/completions" } });
    expect(screen.getByText(/请填写有效的接口地址/)).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText(/请求 URL/), { target: { value: "http://127.0.0.1:11434/v1/chat/completions" } });
    expect(screen.getByText("本机接口可直连")).toBeInTheDocument();
  });

  it("derives the complete endpoint from a custom base URL", async () => {
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("待评估文本");
    await chooseProvider(user, "自定义模型");
    await user.selectOptions(screen.getByLabelText("地址格式"), "base");
    await user.selectOptions(screen.getByLabelText("接口协议"), "anthropic");
    await user.type(screen.getByLabelText(/基础地址/), "https://my-provider.example/v1");
    await user.type(screen.getByLabelText("模型 ID"), "my-model");
    await user.type(screen.getByLabelText("提供方 API Key"), "secret");
    await user.click(screen.getByRole("button", { name: "设置 key" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("set_adapter_key", {
      config: { provider: "custom", protocol: "anthropic", url: "https://my-provider.example/v1/messages", model: "my-model" }, key: "secret",
    }));
  });

  it("keeps each provider's model ID while switching in the same session", async () => {
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("待评估文本");
    await chooseProvider(user, "自定义模型");
    await user.type(screen.getByLabelText("模型 ID"), "private-model");
    await chooseProvider(user, "DeepSeek");
    expect(screen.getByLabelText("选择模型")).toHaveValue("deepseek-chat");
    await chooseProvider(user, "自定义模型");
    expect(screen.getByLabelText("模型 ID")).toHaveValue("private-model");
  });

  it("offers OpenCode Go Chat Completions with its own endpoint and default model", async () => {
    invokeMock.mockResolvedValue({
      ...dashboard,
      configuredTargets: [{ provider: "opencode_go", protocol: "open_ai", url: "https://opencode.ai/zen/go/v1/chat/completions" }],
    });
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("待评估文本");
    await chooseProvider(user, "OpenCode Go");
    expect(screen.getByLabelText("选择模型")).toHaveValue("deepseek-v4.1-flash");
    expect(screen.getByRole("option", { name: "kimi-k3" })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: "minimax-m3" })).toBeNull();
    expect(screen.queryByRole("option", { name: "gpt-5.6-luna" })).toBeNull();
    expect(screen.getByLabelText("OpenCode Go 接口协议")).toHaveValue("open_ai");
    await user.type(screen.getByLabelText("待评估文本"), "review this code");
    await user.click(screen.getByLabelText(/我同意/));
    await user.click(screen.getByRole("button", { name: "发送并评估" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("evaluate_jev", {
      text: "review this code",
      config: { provider: "opencode_go", protocol: "open_ai", url: null, model: "deepseek-v4.1-flash" },
    }));
  });

  it("switching OpenCode Go protocol resets consent and requires its own key", async () => {
    invokeMock.mockResolvedValue({
      ...dashboard,
      configuredTargets: [{ provider: "opencode_go", protocol: "open_ai", url: "https://opencode.ai/zen/go/v1/chat/completions" }],
    });
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("待评估文本");
    await chooseProvider(user, "OpenCode Go");
    await user.click(screen.getByLabelText(/我同意/));
    await user.selectOptions(screen.getByLabelText("OpenCode Go 接口协议"), "anthropic");
    expect(screen.getByLabelText(/我同意/)).not.toBeChecked();
    expect(screen.getByLabelText("选择模型")).toHaveValue("minimax-m3");
    expect(screen.getByRole("option", { name: "qwen3.8-flash" })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: "kimi-k3" })).toBeNull();
    expect(screen.getByRole("button", { name: "发送并评估" })).toBeDisabled();
    await user.type(screen.getByLabelText("OpenCode Go API Key"), "go-session-secret");
    await user.click(screen.getByRole("button", { name: "设置 key" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("set_adapter_key", {
      config: { provider: "opencode_go", protocol: "anthropic", url: null, model: "minimax-m3" },
      key: "go-session-secret",
    }));
  });

  it("blocks UTF-8 input beyond backend limit", async () => {
    render(<JevPanel />);
    const textarea = await screen.findByLabelText("待评估文本");
    fireEvent.change(textarea, { target: { value: "中".repeat(6000) } });
    expect(screen.getByRole("alert")).toHaveTextContent("文本超过 16 KiB");
    expect(screen.getByRole("button", { name: "发送并评估" })).toBeDisabled();
  });

  it("reports unknown quality and cost without fake estimates", async () => {
    render(<JevPanel />);
    await screen.findByLabelText("待评估文本");
    expect(screen.getByText("NOT EVALUATED")).toBeInTheDocument();
    expect(screen.getByText("未提供账单金额")).toBeInTheDocument();
    expect(screen.getByText("无基线")).toBeInTheDocument();
  });

  it("refreshes failed request record without automatic retry", async () => {
    const failedRecord: JevRecord = { ...record, id: "failed", status: "error", choice: null, error: "adapter_network" };
    invokeMock.mockImplementation((command: string) => command === "evaluate_jev"
      ? Promise.reject("adapter_network")
      : Promise.resolve({ ...dashboard, records: [failedRecord] }));
    render(<JevPanel />);
    const user = userEvent.setup();
    await chooseProvider(user, "DeepSeek");
    await user.type(await screen.findByLabelText("待评估文本"), "check this task");
    await user.click(screen.getByLabelText(/我同意/));
    await user.click(screen.getByRole("button", { name: "发送并评估" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("无法连接提供方");
    expect(screen.getByText("adapter_network")).toBeInTheDocument();
    expect(invokeMock.mock.calls.filter(([name]) => name === "evaluate_jev")).toHaveLength(1);
  });

  it("offers a model dropdown for every built-in adapter and no OpenAI preset", async () => {
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("待评估文本");
    expect(screen.queryByRole("option", { name: "OpenAI" })).toBeNull();
    for (const provider of ["DeepSeek", "Anthropic", "OpenRouter", "阿里云百炼", "Google Gemini", "OpenCode Go"]) {
      await chooseProvider(user, provider);
      expect(screen.getByLabelText("选择模型").tagName).toBe("SELECT");
      expect(screen.getByLabelText("选择模型")).not.toHaveValue("");
    }
    await chooseProvider(user, "DeepSeek");
    expect(screen.getByRole("option", { name: "deepseek-chat" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "deepseek-reasoner" })).toBeInTheDocument();
    await chooseProvider(user, "阿里云百炼");
    expect(screen.getByRole("option", { name: "qwen-turbo" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "qwen-max" })).toBeInTheDocument();
    await chooseProvider(user, "Google Gemini");
    expect(screen.getByRole("option", { name: "gemini-2.5-pro" })).toBeInTheDocument();
    await chooseProvider(user, "自定义模型");
    expect(screen.getByLabelText("模型 ID").tagName).toBe("INPUT");
  });

  it("shows the 14 new provider options in the Jev picker", async () => {
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("待评估文本");
    await user.click(screen.getByRole("button", { name: "TypeSafe Jev（原生）" }));

    const dialog = screen.getByRole("dialog", { name: "选择提供方" });
    const newProviderLabels = [
      "火山引擎", "MiniMax CN", "MiniMax Global", "BigModel", "Xiaomi MiMo", "硅基流动", "Z.ai",
      "Kimi CN", "Kimi Global", "BytePlus", "AWS Bedrock", "腾讯云", "模力方舟 ModelScope", "PPIO",
    ];
    expect(within(dialog).getAllByRole("radio")).toHaveLength(22);
    for (const label of newProviderLabels) {
      expect(within(dialog).getByRole("radio", { name: label })).toBeInTheDocument();
    }
  });

  it("uses one provider key input and hides it after the adapter is connected", async () => {
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("待评估文本");
    await chooseProvider(user, "DeepSeek");

    expect(screen.queryByLabelText("提供方 API Key")).not.toBeInTheDocument();
    expect(screen.getByText("DeepSeek Key 已连接，无需重复设置。")).toBeInTheDocument();
    expect(screen.queryAllByRole("textbox").filter((element) => element.getAttribute("type") === "password")).toHaveLength(0);
  });

  it("shows region and workspace fields for regional providers", async () => {
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("待评估文本");

    await chooseProvider(user, "BytePlus");
    expect(screen.getByLabelText("地区")).toHaveValue("ap-southeast");
    await chooseProvider(user, "AWS Bedrock");
    expect(screen.getByLabelText("地区")).toHaveValue("us-east-1");
    await chooseProvider(user, "阿里云百炼");
    expect(screen.getByText("填写有效 Workspace ID 后显示官方请求地址。")).toBeInTheDocument();
    await user.type(screen.getByLabelText("高级：阿里云 Workspace ID（用于官方模型目录）"), "workspace-123");
    expect(screen.getByLabelText("高级：阿里云 Workspace ID（用于官方模型目录）")).toHaveValue("workspace-123");
    expect(screen.getAllByText("https://workspace-123.cn-beijing.maas.aliyuncs.com/compatible-mode/v1/chat/completions").length).toBeGreaterThan(0);
    await user.selectOptions(screen.getByLabelText("地区"), "ap-southeast-1");
    expect(screen.getAllByText("https://workspace-123.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1/chat/completions").length).toBeGreaterThan(0);
  });

  it("allows a manual model ID on a provider without an offline catalog", async () => {
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("待评估文本");
    await chooseProvider(user, "火山引擎");

    expect(screen.getByLabelText("模型 / Endpoint ID")).toBeInTheDocument();
    expect(screen.getByText("设置此提供方的 Key 后可读取官方模型目录。")).toBeInTheDocument();
    await user.type(screen.getByLabelText("模型 / Endpoint ID"), "doubao-seed-1-6");
    expect(screen.getByLabelText("模型 / Endpoint ID")).toHaveValue("doubao-seed-1-6");
  });

  it("keeps all new platforms scoped to Jev commands", async () => {
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("待评估文本");
    for (const label of [
      "火山引擎", "MiniMax CN", "MiniMax Global", "BigModel", "Xiaomi MiMo", "硅基流动", "Z.ai",
      "Kimi CN", "Kimi Global", "BytePlus", "AWS Bedrock", "腾讯云", "模力方舟 ModelScope", "PPIO",
    ]) {
      await chooseProvider(user, label);
    }

    const commands = invokeMock.mock.calls.map(([command]) => command);
    expect(commands.every((command) => command === "get_jev_dashboard" || command === "get_jev_models")).toBe(true);
    expect(invokeMock).not.toHaveBeenCalledWith("set_adapter_key", expect.anything());
    expect(invokeMock).not.toHaveBeenCalledWith("evaluate_jev", expect.anything());
  });

  it("uses official catalog models after an adapter key is configured", async () => {
    invokeMock.mockImplementation((command: string, args: unknown) => {
      if (command === "get_jev_models") {
        expect(args).toEqual({ config: { provider: "deepseek", protocol: null, url: null, model: "" } });
        return Promise.resolve(["deepseek-chat", "deepseek-reasoner"]);
      }
      return Promise.resolve(dashboard);
    });
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("待评估文本");
    await chooseProvider(user, "DeepSeek");
    await waitFor(() => expect(screen.getByText(/已读取官方模型目录，共 2 项/)).toBeInTheDocument());
    await user.selectOptions(screen.getByLabelText("选择模型"), "deepseek-reasoner");
    await user.type(screen.getByLabelText("待评估文本"), "classify this");
    await user.click(screen.getByLabelText(/我同意/));
    await user.click(screen.getByRole("button", { name: "发送并评估" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("evaluate_jev", {
      text: "classify this", config: { provider: "deepseek", protocol: null, url: null, model: "deepseek-reasoner" },
    }));
    expect(invokeMock).toHaveBeenCalledWith("get_jev_models", {
      config: { provider: "deepseek", protocol: null, url: null, model: "" },
    });
  });

  it("falls back to offline models when official catalog retrieval fails", async () => {
    invokeMock.mockImplementation((command: string) => command === "get_jev_models"
      ? Promise.reject("jev_catalog_unavailable") : Promise.resolve(dashboard));
    render(<JevPanel />);
    const user = userEvent.setup();
    await screen.findByLabelText("待评估文本");
    await chooseProvider(user, "DeepSeek");
    await waitFor(() => expect(screen.getByText("官方模型目录暂不可用，正在使用离线预设或手动 ID。")).toBeInTheDocument());
    expect(screen.getByLabelText("选择模型")).toHaveValue("deepseek-chat");
    expect(screen.getByRole("option", { name: "deepseek-reasoner" })).toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalledWith("evaluate_jev", expect.anything());
  });
});
