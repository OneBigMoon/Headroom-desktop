import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { SectionCard } from "./SectionCard";
import { JevProviderPicker } from "./JevProviderPicker";
import "./JevPanel.css";

const MAX_TEXT_LENGTH = 16 * 1024;
const MAX_INPUT_BYTES = 16 * 1024;
const MAX_RECORDS = 200;
const RECENT_RECORDS = 20;
type DecisionProvider = "typesafe" | "custom" | "deepseek" | "volcengine" | "minimax_cn" | "minimax_global" | "bigmodel" | "qwen" | "mimo" | "siliconflow" | "zai" | "openrouter" | "kimi_cn" | "kimi_global" | "byteplus" | "aws_bedrock_mantle" | "tencent_tokenhub" | "modelscope" | "ppio" | "anthropic" | "gemini" | "opencode_go";
type AdapterProtocol = "open_ai" | "anthropic";
type UrlMode = "full" | "base";
type CatalogProvider = Exclude<DecisionProvider, "typesafe" | "custom">;
type CatalogStatus = "loading" | "online" | "offline";

const OPENCODE_GO_ENDPOINTS: Record<AdapterProtocol, string> = {
  open_ai: "https://opencode.ai/zen/go/v1/chat/completions",
  anthropic: "https://opencode.ai/zen/go/v1/messages",
};

export const PROVIDERS: Record<Exclude<DecisionProvider, "custom">, { label: string; url: string; model: string }> = {
  typesafe: { label: "TypeSafe Jev", url: "https://api.typesafe.ai/v1/systemone", model: "jev-latest" },
  deepseek: { label: "DeepSeek", url: "https://api.deepseek.com/chat/completions", model: "deepseek-chat" },
  volcengine: { label: "火山引擎", url: "https://ark.cn-beijing.volces.com/api/v3/chat/completions", model: "" },
  minimax_cn: { label: "MiniMax CN", url: "https://api.minimax.cn/v1/chat/completions", model: "MiniMax-M3" },
  minimax_global: { label: "MiniMax Global", url: "https://api.minimax.io/v1/chat/completions", model: "MiniMax-M3" },
  bigmodel: { label: "BigModel", url: "https://open.bigmodel.cn/api/paas/v4/chat/completions", model: "glm-5-turbo" },
  qwen: { label: "阿里云百炼", url: "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions", model: "qwen-plus" },
  mimo: { label: "Xiaomi MiMo", url: "https://api.xiaomimimo.com/v1/chat/completions", model: "" },
  siliconflow: { label: "硅基流动", url: "https://api.siliconflow.cn/v1/chat/completions", model: "" },
  zai: { label: "Z.ai", url: "https://api.z.ai/api/paas/v4/chat/completions", model: "glm-5.1" },
  openrouter: { label: "OpenRouter", url: "https://openrouter.ai/api/v1/chat/completions", model: "openrouter/auto" },
  kimi_cn: { label: "Kimi CN", url: "https://api.moonshot.cn/v1/chat/completions", model: "" },
  kimi_global: { label: "Kimi Global", url: "https://api.moonshot.ai/v1/chat/completions", model: "" },
  byteplus: { label: "BytePlus", url: "https://ark.ap-southeast.bytepluses.com/api/v3/chat/completions", model: "" },
  aws_bedrock_mantle: { label: "AWS Bedrock", url: "https://bedrock-mantle.us-east-1.api.aws/v1/chat/completions", model: "" },
  tencent_tokenhub: { label: "腾讯云", url: "https://tokenhub.tencentmaas.com/v1/chat/completions", model: "" },
  modelscope: { label: "模力方舟 ModelScope", url: "https://api-inference.modelscope.cn/v1/chat/completions", model: "" },
  ppio: { label: "PPIO", url: "https://api.ppio.com/openai/v1/chat/completions", model: "deepseek/deepseek-v4-flash" },
  anthropic: { label: "Anthropic", url: "https://api.anthropic.com/v1/messages", model: "claude-haiku-4-5-20251001" },
  gemini: { label: "Google Gemini", url: "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", model: "gemini-2.5-flash" },
  opencode_go: { label: "OpenCode Go", url: OPENCODE_GO_ENDPOINTS.open_ai, model: "deepseek-v4.1-flash" },
};
const OFFLINE_MODELS: Record<CatalogProvider, string[]> = {
  deepseek: ["deepseek-chat", "deepseek-reasoner"],
  volcengine: [],
  minimax_cn: ["MiniMax-M3", "MiniMax-M2.7"],
  minimax_global: ["MiniMax-M3", "MiniMax-M2.7"],
  bigmodel: ["glm-5-turbo"],
  qwen: ["qwen-plus", "qwen-turbo", "qwen-max"],
  mimo: [],
  siliconflow: [],
  zai: ["glm-5.1"],
  kimi_cn: [],
  kimi_global: [],
  byteplus: [],
  aws_bedrock_mantle: [],
  tencent_tokenhub: [],
  modelscope: [],
  ppio: ["deepseek/deepseek-v4-flash", "deepseek/deepseek-v4-pro"],
  anthropic: ["claude-haiku-4-5-20251001", "claude-sonnet-5"],
  openrouter: ["openrouter/auto"],
  gemini: ["gemini-2.5-flash", "gemini-2.5-pro"],
  opencode_go: ["deepseek-v4.1-flash", "glm-5.3-flash", "glm-5.3", "glm-5.2", "glm-5.1", "kimi-k3", "kimi-k2.7-code", "kimi-k2.6", "longcat-2.0", "deepseek-v4-pro", "deepseek-v4-flash", "deepseek-v4-flash-vision-exp", "mimo-v2.6-flash", "mimo-v2.6-pro", "mimo-v2.5", "mimo-v2.5-pro", "hy4-preview", "hy3"],
};
const GO_MESSAGES_MODELS = ["minimax-m3", "minimax-m2.7", "minimax-m2.5", "qwen3.8-max", "qwen3.8-flash", "qwen3.7-max", "qwen3.7-plus", "qwen3.6-plus"];
const REMOTE_CATALOG_PROVIDERS = new Set<CatalogProvider>([
  "deepseek", "volcengine", "minimax_cn", "minimax_global", "qwen", "mimo",
  "siliconflow", "openrouter", "kimi_cn", "kimi_global", "aws_bedrock_mantle",
  "tencent_tokenhub", "anthropic", "gemini", "opencode_go",
]);

const REGION_OPTIONS = {
  qwen: ["cn-beijing", "ap-southeast-1"],
  byteplus: ["ap-southeast", "eu-west"],
  aws_bedrock_mantle: ["us-east-1", "us-west-2", "eu-west-1", "ap-southeast-1"],
  tencent_tokenhub: ["guangzhou", "singapore"],
} as const;
type RegionalProvider = keyof typeof REGION_OPTIONS;

function builtInEndpoint(provider: Exclude<DecisionProvider, "custom">, goProtocol: AdapterProtocol, region: string, workspaceId: string): string | null {
  if (provider === "opencode_go") return OPENCODE_GO_ENDPOINTS[goProtocol];
  if (provider === "qwen") return workspaceId ? `https://${workspaceId}.${region}.maas.aliyuncs.com/compatible-mode/v1/chat/completions` : null;
  if (provider === "aws_bedrock_mantle") return `https://bedrock-mantle.${region}.api.aws/v1/chat/completions`;
  if (provider === "byteplus") return `https://ark.${region}.bytepluses.com/api/v3/chat/completions`;
  if (provider === "tencent_tokenhub") return `https://${region === "singapore" ? "tokenhub-intl" : "tokenhub"}.tencentmaas.com/v1/chat/completions`;
  return PROVIDERS[provider].url;
}
const INITIAL_MODELS: Record<DecisionProvider, string> = {
  typesafe: PROVIDERS.typesafe.model,
  custom: "",
  deepseek: PROVIDERS.deepseek.model,
  volcengine: PROVIDERS.volcengine.model,
  minimax_cn: PROVIDERS.minimax_cn.model,
  minimax_global: PROVIDERS.minimax_global.model,
  bigmodel: PROVIDERS.bigmodel.model,
  qwen: PROVIDERS.qwen.model,
  mimo: PROVIDERS.mimo.model,
  siliconflow: PROVIDERS.siliconflow.model,
  zai: PROVIDERS.zai.model,
  kimi_cn: PROVIDERS.kimi_cn.model,
  kimi_global: PROVIDERS.kimi_global.model,
  byteplus: PROVIDERS.byteplus.model,
  aws_bedrock_mantle: PROVIDERS.aws_bedrock_mantle.model,
  tencent_tokenhub: PROVIDERS.tencent_tokenhub.model,
  modelscope: PROVIDERS.modelscope.model,
  ppio: PROVIDERS.ppio.model,
  anthropic: PROVIDERS.anthropic.model,
  openrouter: PROVIDERS.openrouter.model,
  gemini: PROVIDERS.gemini.model,
  opencode_go: PROVIDERS.opencode_go.model,
};

function validCustomUrl(raw: string, protocol: AdapterProtocol, mode: UrlMode): string | null {
  if (raw.length > 2048 || raw.trim() !== raw) return null;
  try {
    const url = new URL(raw);
    const local = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
    if (!url.hostname || url.username || url.password || url.search || url.hash || (url.protocol !== "https:" && !(url.protocol === "http:" && local))) return null;
    if (mode === "base") url.pathname = `${url.pathname.replace(/\/$/, "")}/${protocol === "open_ai" ? "chat/completions" : "messages"}`;
    return url.toString();
  } catch { return null; }
}

export interface JevRecord {
  id: string;
  timestamp: string;
  provider: DecisionProvider;
  endpoint: string | null;
  model: string | null;
  status: "success" | "error";
  latencyMs: number;
  inputTokens: number | null;
  outputTokens: number | null;
  choice: string | null;
  confidence: number | null;
  error: string | null;
}

interface JevDashboard {
  keyConfigured: boolean;
  keyPersistent: boolean;
  configuredTargets: { provider: DecisionProvider; protocol: AdapterProtocol; url: string }[];
  busy: boolean;
  records: JevRecord[];
}

const EMPTY_DASHBOARD: JevDashboard = { keyConfigured: false, keyPersistent: false, configuredTargets: [], busy: false, records: [] };
const EXAMPLE_TEXT = "请检查这段 Rust 代码为什么无法通过编译。";

function errorMessage(error: unknown): string {
  const code = typeof error === "string" ? error : error instanceof Error ? error.message : "";
  const messages: Record<string, string> = {
    jev_invalid_key: "Key 格式无效，请检查后重试。",
    jev_key_missing: "请先设置 TypeSafe Key。",
    jev_busy: "正在评估，请等待完成后再更改 Key。",
    jev_key_store_read: "无法读取已保存的 TypeSafe Key，请检查系统钥匙串是否可用，然后刷新重试。",
    jev_key_store_write: "TypeSafe Key 未保存，请检查系统钥匙串是否可用，然后重试。",
    jev_key_store_delete: "无法从系统钥匙串删除 TypeSafe Key，请稍后重试。",
    jev_key_store_invalid: "已保存的 TypeSafe Key 格式无效，请在系统钥匙串中移除后重新设置。",
    adapter_invalid_url: "接口 URL 无效：远程服务须使用 HTTPS；本地 HTTP 仅允许 localhost。不要在 URL 中放入 Key。",
    adapter_invalid_model: "模型 ID 无效，请填写提供方给出的模型 ID。",
    adapter_invalid_protocol: "请选择受支持的接口协议。",
    adapter_invalid_config: "预设提供方的接口地址或协议不可更改，请选“自定义”接入。",
    adapter_workspace_required: "阿里云需要 Workspace ID，请先从控制台复制并填写。",
    adapter_invalid_workspace: "Workspace ID 格式无效，请检查控制台中的原值。",
    adapter_region_required: "请先选择提供方地区。",
    adapter_invalid_region: "地区无效，请从当前提供方支持的地区中选择。",
    adapter_key_missing: "请先设置这个接口的 API Key。",
    adapter_unauthorized: "提供方拒绝了此 Key，请检查 Key 和账户权限。",
    adapter_rate_limited: "请求受限，请稍后手动重试。",
    adapter_timeout: "请求超时；结果可能未知，请先核对提供方账单，再决定是否重试。",
    adapter_network: "无法连接提供方，请检查接口地址和网络。",
    adapter_http_4xx: "提供方拒绝了请求，请检查模型 ID、协议和账户。",
    adapter_http_5xx: "提供方服务暂时不可用。",
    adapter_invalid_response: "接口返回了无法解析的响应。",
    adapter_invalid_choice: "模型未返回有效的 Choice 分类。",
    adapter_incomplete_choice: "模型的 Choice 分类被截断。",
  };
  return messages[code] ?? (error instanceof Error ? error.message : "操作失败，请稍后重试。");
}

function percentile(values: number[], percentileRank: number): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const index = Math.max(0, Math.ceil(sorted.length * percentileRank) - 1);
  return sorted[index];
}

function formatNumber(value: number): string {
  return new Intl.NumberFormat("zh-CN").format(value);
}

function formatTimestamp(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

function formatMetric(value: number | null, suffix = ""): string {
  return value === null ? "暂无数据" : `${formatNumber(value)}${suffix}`;
}

export function JevPanel() {
  const [dashboard, setDashboard] = useState<JevDashboard>(EMPTY_DASHBOARD);
  const [provider, setProvider] = useState<DecisionProvider>("typesafe");
  const [models, setModels] = useState<Record<DecisionProvider, string>>(INITIAL_MODELS);
  const [catalogs, setCatalogs] = useState<Record<string, string[]>>({});
  const [catalogStatuses, setCatalogStatuses] = useState<Record<string, CatalogStatus>>({});
  const [catalogRefresh, setCatalogRefresh] = useState(0);
  const [customProtocol, setCustomProtocol] = useState<AdapterProtocol>("open_ai");
  const [goProtocol, setGoProtocol] = useState<AdapterProtocol>("open_ai");
  const [customUrl, setCustomUrl] = useState("");
  const [urlMode, setUrlMode] = useState<UrlMode>("full");
  const [regions, setRegions] = useState<Record<RegionalProvider, string>>({
    qwen: "cn-beijing",
    byteplus: "ap-southeast",
    aws_bedrock_mantle: "us-east-1",
    tencent_tokenhub: "guangzhou",
  });
  const [workspaceId, setWorkspaceId] = useState("");
  const [catalogWorkspaceId, setCatalogWorkspaceId] = useState("");
  const [manualModels, setManualModels] = useState<Partial<Record<DecisionProvider, boolean>>>({});
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [key, setKey] = useState("");
  const [keyBusy, setKeyBusy] = useState(false);
  const [text, setText] = useState("");
  const [consent, setConsent] = useState(false);
  const [evaluating, setEvaluating] = useState(false);
  const initialLoadStarted = useRef(false);
  const evaluateInFlight = useRef(false);
  const model = models[provider];
  const workspaceInput = workspaceId.trim();
  const workspaceValid = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?$/.test(workspaceInput);
  const region = provider in REGION_OPTIONS ? regions[provider as RegionalProvider] : "";
  const endpoint = provider === "custom" ? validCustomUrl(customUrl, customProtocol, urlMode) : builtInEndpoint(provider, goProtocol, region, workspaceValid ? workspaceInput : "");
  const protocol = provider === "custom" ? customProtocol : provider === "opencode_go" ? goProtocol : provider === "anthropic" ? "anthropic" : "open_ai";
  const config = {
    provider,
    protocol: provider === "custom" || provider === "opencode_go" ? protocol : null,
    url: provider === "custom" ? endpoint : null,
    model: provider === "typesafe" ? "jev-latest" : model,
    ...(region ? { region } : {}),
    ...(provider === "qwen" && workspaceValid ? { workspaceId: workspaceInput } : {}),
  };
  const localWithoutKey = provider === "custom" && endpoint !== null && endpoint.startsWith("http://");
  const keyConfigured = provider === "typesafe" ? dashboard.keyConfigured : dashboard.configuredTargets.some((target) => target.provider === provider && target.protocol === protocol && target.url === endpoint);
  const providerReady = provider === "typesafe" ? keyConfigured : !!endpoint && !!model.trim() && (keyConfigured || localWithoutKey);
  const catalogKey = `${provider}:${provider === "opencode_go" ? goProtocol : provider === "qwen" ? `${region}:${catalogWorkspaceId}` : region || "default"}`;
  const offlineModels = provider === "typesafe" || provider === "custom" ? [] : provider === "opencode_go" && goProtocol === "anthropic" ? GO_MESSAGES_MODELS : OFFLINE_MODELS[provider];
  const catalogCanSync = provider !== "typesafe" && provider !== "custom" && REMOTE_CATALOG_PROVIDERS.has(provider) && (provider === "opencode_go" || provider === "openrouter" || keyConfigured) && (provider !== "qwen" || (workspaceValid && catalogWorkspaceId === workspaceInput)) && !manualModels[provider];
  const availableModels = catalogs[catalogKey] ?? offlineModels;
  const catalogStatus = catalogStatuses[catalogKey];
  const useManualModel = provider === "custom" || !!manualModels[provider] || availableModels.length === 0;
  let catalogHint = "当前显示官网预设。";
  if (provider !== "typesafe" && provider !== "custom") {
    if (!REMOTE_CATALOG_PROVIDERS.has(provider)) catalogHint = "此平台没有可核实的完整模型列表接口；可用官网预设或填写账户模型 ID。";
    else if (provider === "qwen" && !workspaceInput) catalogHint = "填写 Workspace ID 后可读取阿里云官方模型目录。";
    else if (provider === "qwen" && !workspaceValid) catalogHint = "Workspace ID 只能包含字母、数字和连字符，且不能以连字符开头或结尾。";
    else if (catalogStatus === "loading") catalogHint = "正在读取官方模型目录...";
    else if (catalogStatus === "online") catalogHint = `已读取官方模型目录，共 ${availableModels.length} 项；具体可用模型取决于账户权限。`;
    else if (catalogStatus === "offline") catalogHint = "官方模型目录暂不可用，正在使用离线预设或手动 ID。";
    else if (!keyConfigured && provider !== "opencode_go" && provider !== "openrouter") catalogHint = "设置此提供方的 Key 后可读取官方模型目录。";
  }

  const loadDashboard = useCallback(async (initial = false) => {
    if (initial && initialLoadStarted.current) return;
    if (initial) initialLoadStarted.current = true;
    if (initial) setLoading(true);
    else setRefreshing(true);
    try {
      const next = await invoke<JevDashboard>("get_jev_dashboard");
      setDashboard({ ...next, records: next.records.slice(-MAX_RECORDS) });
      setError(null);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      if (initial) setLoading(false);
      else setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    void loadDashboard(true);
  }, [loadDashboard]);

  useEffect(() => {
    const timeout = window.setTimeout(() => setCatalogWorkspaceId(workspaceId.trim()), 500);
    return () => window.clearTimeout(timeout);
  }, [workspaceId]);

  useEffect(() => {
    if (provider === "typesafe" || provider === "custom" || !catalogCanSync) return;
    const selectedProvider = provider;
    const selectedCatalogKey = catalogKey;
    let active = true;
    setCatalogStatuses((current) => ({ ...current, [selectedCatalogKey]: "loading" }));
    void invoke<string[]>("get_jev_models", {
      config: {
        provider: selectedProvider,
        protocol: selectedProvider === "opencode_go" ? goProtocol : null,
        url: null,
        model: "",
        ...(region ? { region } : {}),
        ...(selectedProvider === "qwen" && catalogWorkspaceId ? { workspaceId: catalogWorkspaceId } : {}),
      },
    }).then((items) => {
      if (!active) return;
      if (!Array.isArray(items) || items.length === 0) throw new Error("empty catalog");
      setCatalogs((current) => ({ ...current, [selectedCatalogKey]: items }));
      setCatalogStatuses((current) => ({ ...current, [selectedCatalogKey]: "online" }));
      setConsent(false);
      setModels((current) => {
        if (items.includes(current[selectedProvider])) return current;
        return { ...current, [selectedProvider]: items[0] };
      });
    }).catch(() => {
      if (!active) return;
      setCatalogs((current) => {
        const next = { ...current };
        delete next[selectedCatalogKey];
        return next;
      });
      setCatalogStatuses((current) => ({ ...current, [selectedCatalogKey]: "offline" }));
      const presets = selectedProvider === "opencode_go" && goProtocol === "anthropic" ? GO_MESSAGES_MODELS : OFFLINE_MODELS[selectedProvider];
      setConsent(false);
      setModels((current) => {
        if (presets.includes(current[selectedProvider])) return current;
        return { ...current, [selectedProvider]: presets[0] ?? "" };
      });
    });
    return () => { active = false; };
  }, [provider, goProtocol, region, catalogWorkspaceId, catalogKey, catalogCanSync, catalogRefresh]);

  const saveKey = async () => {
    if (keyBusy || evaluating || dashboard.busy || (provider !== "typesafe" && !endpoint)) return;
    setKeyBusy(true);
    try {
      if (provider === "typesafe") await invoke("set_jev_key", { key });
      else await invoke("set_adapter_key", { config, key });
      setKey("");
      setDashboard(await invoke<JevDashboard>("get_jev_dashboard"));
      setCatalogRefresh((current) => current + 1);
      setError(null);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setKeyBusy(false);
    }
  };

  const disconnect = async () => {
    if (keyBusy || evaluating || dashboard.busy) return;
    setKeyBusy(true);
    try {
      if (provider === "typesafe") await invoke("set_jev_key", { key: "" });
      else await invoke("set_adapter_key", { config, key: "" });
      setDashboard(await invoke<JevDashboard>("get_jev_dashboard"));
      if (provider !== "typesafe" && provider !== "custom") {
        setCatalogs((current) => {
          const next = { ...current };
          delete next[catalogKey];
          return next;
        });
        setCatalogStatuses((current) => ({ ...current, [catalogKey]: "offline" }));
      }
      setError(null);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setKeyBusy(false);
    }
  };

  const evaluate = async () => {
    if (!providerReady || !consent || !text.trim() || new TextEncoder().encode(text).length > MAX_INPUT_BYTES || evaluating || dashboard.busy || evaluateInFlight.current) return;
    evaluateInFlight.current = true;
    setEvaluating(true);
    try {
      const next = await invoke<JevDashboard>("evaluate_jev", { text, config });
      setDashboard({ ...next, records: next.records.slice(-MAX_RECORDS) });
      setError(null);
    } catch (err) {
      setError(errorMessage(err));
      try {
        const next = await invoke<JevDashboard>("get_jev_dashboard");
        setDashboard({ ...next, records: next.records.slice(-MAX_RECORDS) });
      } catch (refreshError) {
        console.error("Failed to read decision records after evaluation error", refreshError);
      }
    } finally {
      evaluateInFlight.current = false;
      setEvaluating(false);
    }
  };

  const records = dashboard.records.filter((record) => record.provider === provider && (provider === "typesafe" || record.endpoint === endpoint)).slice(-MAX_RECORDS);
  const latencyValues = records.map((record) => record.latencyMs).filter((value) => Number.isFinite(value));
  const successfulRecords = records.filter((record) => record.status === "success");
  const tokenRecords = records.filter((record) => record.inputTokens !== null);
  const tokenSum = tokenRecords.reduce((sum, record) => sum + (record.inputTokens ?? 0), 0);
  const missingTokenCount = records.length - tokenRecords.length;
  const outputTokenRecords = records.filter((record) => record.outputTokens !== null);
  const outputTokenSum = outputTokenRecords.reduce((sum, record) => sum + (record.outputTokens ?? 0), 0);
  const inputBytes = new TextEncoder().encode(text).length;
  const successRate = records.length === 0 ? null : (successfulRecords.length / records.length) * 100;

  return (
    <div className="jev-panel">
      <SectionCard
        title="类型化决策"
        subtitle="TypeSafe Jev 与兼容模型的手动分类决策。仅保留本次会话最近 200 条记录。"
      >
        <div className="jev-panel__toolbar">
        <span>决策提供方</span>
        <JevProviderPicker
          value={provider}
          options={[
            { id: "typesafe", label: "TypeSafe Jev（原生）" },
            { id: "custom", label: "自定义模型" },
            ...Object.entries(PROVIDERS).filter(([id]) => id !== "typesafe").map(([id, item]) => ({ id, label: item.label })),
          ]}
          disabled={evaluating || dashboard.busy || keyBusy}
          onChange={(id) => {
            const next = id as DecisionProvider;
            setProvider(next);
            setKey("");
            setConsent(false);
            setError(null);
          }}
        />
          <span className={`jev-panel__status ${providerReady ? "jev-panel__status--ready" : ""}`}>
            {keyConfigured ? "已配置 Key" : localWithoutKey ? "本机接口可直连" : "未配置 Key"}
          </span>
          <button type="button" className="jev-panel__button jev-panel__button--quiet" onClick={() => void loadDashboard()} disabled={loading || refreshing}>
            {refreshing ? "刷新中..." : "刷新"}
          </button>
        </div>

        {error ? <p className="jev-panel__error" role="alert">{error}</p> : null}
        {loading ? <p className="jev-panel__loading" role="status">正在读取会话数据...</p> : null}

        {provider !== "typesafe" ? (
          <div className="jev-panel__connection">
            {provider === "opencode_go" ? (
              <>
                <label htmlFor="jev-go-protocol">OpenCode Go 接口协议</label>
                <select id="jev-go-protocol" value={goProtocol} onChange={(event) => {
                  const next = event.target.value as AdapterProtocol;
                  setGoProtocol(next);
                  const presets = next === "open_ai" ? OFFLINE_MODELS.opencode_go : GO_MESSAGES_MODELS;
                  setModels((current) => ({ ...current, opencode_go: (catalogs[`opencode_go:${next}`] ?? presets)[0] }));
                  setConsent(false);
                  setKey("");
                }}>
                  <option value="open_ai">Chat Completions（如 DeepSeek V4.1 Flash）</option>
                  <option value="anthropic">Messages（如 MiniMax M3）</option>
                </select>
              </>
            ) : null}
          {provider === "custom" ? (
              <>
                <label htmlFor="jev-protocol">接口协议</label>
                <select id="jev-protocol" value={customProtocol} onChange={(event) => { setCustomProtocol(event.target.value as AdapterProtocol); setConsent(false); setKey(""); }}>
                  <option value="open_ai">Chat Completions 兼容</option>
                  <option value="anthropic">Anthropic Messages 兼容</option>
                </select>
                <label htmlFor="jev-url-mode">地址格式</label>
                <select id="jev-url-mode" value={urlMode} onChange={(event) => { setUrlMode(event.target.value as UrlMode); setConsent(false); setKey(""); }}>
                  <option value="full">完整请求 URL</option>
                  <option value="base">基础地址（自动拼接路径）</option>
                </select>
                <label htmlFor="jev-url">{urlMode === "full" ? "请求 URL（完整接口地址）" : "基础地址（例如含 /v1）"}</label>
                <input id="jev-url" type="url" value={customUrl} onChange={(event) => { setCustomUrl(event.target.value); setConsent(false); setKey(""); }} placeholder={urlMode === "base" ? "https://example.com/v1" : customProtocol === "open_ai" ? "https://example.com/v1/chat/completions" : "https://example.com/v1/messages"} autoComplete="off" />
                {customUrl && !endpoint ? <p className="jev-panel__error" role="alert">请填写有效的接口地址。远程须为 HTTPS；本机 HTTP 仅允许 localhost。</p> : null}
                {endpoint ? <p className="jev-panel__hint">实际请求地址：<span className="jev-panel__endpoint">{endpoint}</span></p> : null}
              </>
          ) : <p className="jev-panel__hint">{endpoint ? <>官方请求地址：<span className="jev-panel__endpoint">{endpoint}</span></> : "填写有效 Workspace ID 后显示官方请求地址。"}</p>}
          {provider in REGION_OPTIONS ? (
            <>
              <label htmlFor="jev-region">地区</label>
              <select id="jev-region" value={region} onChange={(event) => {
                const selected = provider as RegionalProvider;
                setRegions((current) => ({ ...current, [selected]: event.target.value }));
                setKey("");
                setConsent(false);
              }}>
                {REGION_OPTIONS[provider as RegionalProvider].map((value) => <option key={value} value={value}>{value}</option>)}
              </select>
            </>
          ) : null}
          {provider === "qwen" ? (
            <>
              <label htmlFor="jev-workspace">高级：阿里云 Workspace ID（用于官方模型目录）</label>
              <input id="jev-workspace" value={workspaceId} onChange={(event) => { setWorkspaceId(event.target.value); setConsent(false); }} placeholder="从阿里云百炼控制台复制" autoComplete="off" maxLength={63} aria-invalid={!!workspaceInput && !workspaceValid} />
            </>
          ) : null}
          <label htmlFor="jev-model">{provider === "volcengine" || provider === "byteplus" ? "模型 / Endpoint ID" : useManualModel ? "模型 ID" : "选择模型"}</label>
          {useManualModel ? (
            <input id="jev-model" value={model} onChange={(event) => { setModels((current) => ({ ...current, [provider]: event.target.value })); setConsent(false); }} maxLength={128} placeholder={provider === "volcengine" || provider === "byteplus" ? "填写账户专属 Endpoint ID 或模型 ID" : "填写提供方给出的模型 ID"} autoComplete="off" />
          ) : (
            <select id="jev-model" value={model} onChange={(event) => { setModels((current) => ({ ...current, [provider]: event.target.value })); setConsent(false); }}>
              {availableModels.map((id) => <option key={id} value={id}>{id}</option>)}
            </select>
          )}
          {provider !== "custom" ? (
            <>
              <p className="jev-panel__hint" role="status" aria-live="polite">{catalogHint}</p>
              {availableModels.length > 0 ? <button type="button" className="jev-panel__button jev-panel__button--quiet jev-panel__catalog-refresh" onClick={() => {
                const next = !manualModels[provider];
                setManualModels((current) => ({ ...current, [provider]: next }));
                if (!next) setModels((current) => ({ ...current, [provider]: availableModels[0] }));
                setConsent(false);
              }}>{manualModels[provider] ? "返回模型列表" : "高级：填写模型 ID"}</button> : null}
              {catalogCanSync ? <button type="button" className="jev-panel__button jev-panel__button--quiet jev-panel__catalog-refresh" disabled={catalogStatus === "loading"} onClick={() => setCatalogRefresh((current) => current + 1)}>刷新模型目录</button> : null}
            </>
          ) : null}
            <p className="jev-panel__hint">{protocol === "open_ai" ? "使用 Chat Completions 兼容协议" : "使用 Anthropic Messages 协议"}。{provider === "custom" ? "模型 ID 请以你的服务商控制台为准。" : "模型列表按当前提供方与协议筛选。"}</p>
            {provider === "opencode_go" ? <p className="jev-panel__hint">请使用 OpenCode Go 的 API Key，模型 ID 不加 opencode-go/ 前缀。Go 面向编码代理；这里仅手动发送单次分类请求，Responses-only 模型暂不支持。</p> : null}
          </div>
        ) : <p className="jev-panel__hint">TypeSafe 原生 System One 接口，模型 jev-latest。</p>}

        <div className="jev-panel__key-row">
          {keyConfigured ? <span className="jev-panel__hint">{provider === "typesafe" ? "TypeSafe 官方 Key" : `${provider === "custom" ? "自定义接口" : PROVIDERS[provider].label} Key`} 已连接，无需重复设置。</span> : (
            <>
              <label htmlFor="jev-key">{provider === "typesafe" ? "TypeSafe Key" : provider === "opencode_go" ? "OpenCode Go API Key" : "提供方 API Key"}</label>
              <input
                id="jev-key"
                type="password"
                value={key}
                onChange={(event) => setKey(event.target.value)}
                autoComplete="new-password"
                placeholder={provider === "typesafe" && dashboard.keyPersistent ? "保存到系统钥匙串" : "仅保留在当前会话"}
                disabled={keyBusy || evaluating || dashboard.busy}
              />
              <button type="button" className="jev-panel__button" onClick={() => void saveKey()} disabled={keyBusy || evaluating || dashboard.busy || !key.trim() || (provider !== "typesafe" && !endpoint)}>
                {keyBusy ? "处理中..." : "设置 key"}
              </button>
            </>
          )}
          {keyConfigured ? (
            <button type="button" className="jev-panel__button jev-panel__button--danger" onClick={() => void disconnect()} disabled={keyBusy || evaluating || dashboard.busy}>
              断开
            </button>
          ) : null}
        </div>
        <p className="jev-panel__hint">{dashboard.keyPersistent ? "TypeSafe 官方 Key 保存在 macOS 钥匙串，重启后自动恢复，点“断开”可删除；" : "此构建的 TypeSafe Key 只保留在当前运行期间，退出后需重新设置；"}其他提供方的 Key 仍只保留在本次运行期间。TypeSafe Key 不会用于其他提供方；切换自定义 URL 不会复用旧地址的 Key。本机无鉴权接口可留空。</p>
        {provider !== "typesafe" ? <p className="jev-panel__hint">兼容模型生成的是 Jev 式 Choice，不是 TypeSafe Jev 原模型；不提供经校准的置信度。</p> : null}

        <div className="jev-panel__metrics" aria-label="会话指标">
          <div><span>请求数</span><strong>{formatNumber(records.length)}</strong></div>
          <div><span>问题数</span><strong>{formatNumber(records.length)}</strong></div>
          <div><span>成功率</span><strong>{successRate === null ? "暂无数据" : `${successRate.toFixed(1)}%`}</strong></div>
          <div><span>延迟 p50 / p95</span><strong>{formatMetric(percentile(latencyValues, 0.5), " ms")} / {formatMetric(percentile(latencyValues, 0.95), " ms")}</strong></div>
            <div><span>输入 token 总数</span><strong>{tokenRecords.length === 0 ? "暂无数据" : formatNumber(tokenSum)}</strong><small>{missingTokenCount > 0 ? `${missingTokenCount} 条缺少 token 数据` : "完整记录"}</small></div>
            <div><span>输出 token（已上报）</span><strong>{outputTokenRecords.length === 0 ? "暂无数据" : formatNumber(outputTokenSum)}</strong><small>{outputTokenRecords.length}/{records.length} 条有数据；未返回不计为 0</small></div>
          <div><span>质量</span><strong>NOT EVALUATED</strong><small>confidence != accuracy</small></div>
          <div><span>成本</span><strong>未提供账单金额</strong></div>
          <div><span>节省</span><strong>无基线</strong></div>
        </div>

        <form className="jev-panel__evaluate" onSubmit={(event) => { event.preventDefault(); void evaluate(); }}>
          <div className="jev-panel__evaluate-heading">
            <div>
              <h3>手动评估</h3>
              <p>不会自动发送。请输入文本并明确同意后，才会发送到所选提供方。</p>
            </div>
            <button type="button" className="jev-panel__button jev-panel__button--quiet" onClick={() => setText(EXAMPLE_TEXT)}>
              使用内置示例
            </button>
          </div>
          <label htmlFor="jev-text">待评估文本</label>
          <textarea id="jev-text" value={text} onChange={(event) => setText(event.target.value)} maxLength={MAX_TEXT_LENGTH} rows={5} />
          <div className="jev-panel__text-meta"><span>最多 16 KiB (UTF-8)</span><span>{inputBytes.toLocaleString()} / {MAX_INPUT_BYTES.toLocaleString()} 字节</span></div>
          {inputBytes > MAX_INPUT_BYTES ? <p className="jev-panel__error" role="alert">文本超过 16 KiB，请缩短后再发送。</p> : null}
          <label className="jev-panel__consent"><input type="checkbox" checked={consent} onChange={(event) => setConsent(event.target.checked)} /> <span>我同意将这段文本发送到 <span className="jev-panel__endpoint">{endpoint ?? "未设置的接口"}</span>；评估可能产生费用。</span></label>
          <button type="submit" className="jev-panel__button jev-panel__button--primary" disabled={!providerReady || !consent || !text.trim() || inputBytes > MAX_INPUT_BYTES || evaluating || dashboard.busy}>
            {evaluating ? "评估中..." : "发送并评估"}
          </button>
        </form>

        <div className="jev-panel__records">
          <div className="jev-panel__records-heading"><h3>最近记录</h3><span>显示最近 {Math.min(records.length, RECENT_RECORDS)} / {records.length} 条，会话上限 200</span></div>
          {records.length === 0 ? <p className="jev-panel__empty">暂无评估记录。</p> : (
            <div className="jev-panel__table-wrap">
              <table>
                <caption className="jev-panel__sr-only">最近 20 条类型化决策记录</caption>
                <thead><tr><th scope="col">时间</th><th scope="col">模型</th><th scope="col">状态</th><th scope="col">延迟</th><th scope="col">输入 token</th><th scope="col">输出 token</th><th scope="col">choice</th><th scope="col">confidence</th><th scope="col">错误</th></tr></thead>
                <tbody>{records.slice(-RECENT_RECORDS).reverse().map((record) => (
                  <tr key={record.id}>
                    <td>{formatTimestamp(record.timestamp)}</td>
                    <td>{record.model ?? "暂无"}</td>
                    <td><span className={`jev-panel__record-status jev-panel__record-status--${record.status}`}>{record.status === "success" ? "成功" : "失败"}</span></td>
                    <td>{formatMetric(record.latencyMs, " ms")}</td>
                      <td>{record.inputTokens === null ? "缺失" : formatNumber(record.inputTokens)}</td>
                      <td>{record.outputTokens === null ? "未返回" : formatNumber(record.outputTokens)}</td>
                    <td>{record.choice ?? "暂无"}</td>
                    <td>{record.confidence === null ? "暂无" : record.confidence}</td>
                    <td>{record.error ?? "-"}</td>
                  </tr>
                ))}</tbody>
              </table>
            </div>
          )}
        </div>
      </SectionCard>
    </div>
  );
}
