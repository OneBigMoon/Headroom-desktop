import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { JevBenchmarkPanel, type JevBenchmarkSnapshot } from "./JevBenchmarkPanel";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }));

const empty: JevBenchmarkSnapshot = {
  datasetVersion: "intent-v1",
  samples: [
    { id: "code", text: "Fix this Rust compiler error.", expected: "coding" },
    { id: "write", text: "请写一首关于秋天的诗。", expected: "writing" },
    { id: "other", text: "你好！", expected: "other" },
  ],
  busy: false,
  result: null,
};

const measured: JevBenchmarkSnapshot = {
  ...empty,
  result: {
    id: "measured-1", datasetVersion: "intent-v1", startedAt: "2026-10-24T01:00:00Z", completedAt: "2026-10-24T01:00:05Z",
    provider: "typesafe", requestedModel: "jev-latest", total: 3, successful: 2, failed: 1, correct: 1, accuracy: 100 / 3,
    cases: [
      { caseId: "code", expected: "coding", predicted: "coding", correct: true, status: "success", model: "jev-1.14.0", latencyMs: 100, inputTokens: 12, outputTokens: 0, confidence: 0.99, error: null },
      { caseId: "write", expected: "writing", predicted: "research", correct: false, status: "success", model: "jev-1.14.0", latencyMs: 200, inputTokens: 8, outputTokens: null, confidence: 0.98, error: null },
      { caseId: "other", expected: "other", predicted: null, correct: false, status: "error", model: null, latencyMs: 300, inputTokens: null, outputTokens: null, confidence: null, error: "jev_timeout" },
    ],
  },
};

function renderPanel(overrides: Partial<Parameters<typeof JevBenchmarkPanel>[0]> = {}) {
  const onRunningChange = vi.fn();
  const onComplete = vi.fn();
  return { ...render(<JevBenchmarkPanel keyConfigured disabled={false} onRunningChange={onRunningChange} onComplete={onComplete} {...overrides} />), onRunningChange, onComplete };
}

describe("JevBenchmarkPanel", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(empty);
  });
  afterEach(() => vi.useRealTimers());

  it("reads saved results and previews fixed samples without sending a model request", async () => {
    renderPanel();
    expect(await screen.findByText(/尚无测试结果/)).toBeInTheDocument();
    expect(screen.getByText(/可能产生费用/)).toHaveTextContent("3 条公开测试文本");
    expect(screen.getByText("Fix this Rust compiler error.")).toBeInTheDocument();
    expect(invokeMock.mock.calls).toEqual([["get_jev_benchmark"]]);
  });

  it("shows persisted measured accuracy including failures without substituting confidence", async () => {
    invokeMock.mockResolvedValue(measured);
    renderPanel();
    const metrics = await screen.findByLabelText("最近一次分类测试结果");
    expect(within(metrics).getByText("33.3%")).toBeInTheDocument();
    expect(within(metrics).getByText("正确 1/3；请求失败也计入总数")).toBeInTheDocument();
    expect(within(metrics).getByText("1 / 1")).toBeInTheDocument();
    expect(screen.queryByText("99.0%")).not.toBeInTheDocument();
    expect(screen.getByText(/实际返回模型：jev-1.14.0/)).toBeInTheDocument();
    expect(screen.getByText(/实际：research/)).toHaveTextContent("分类错误");
    expect(screen.getByText(/错误：jev_timeout/)).toBeInTheDocument();
    expect(within(metrics).getByText("测试输入 token（已上报）").parentElement).toHaveTextContent("20");
    expect(within(metrics).getByText("测试输出 token（已上报）").parentElement).toHaveTextContent("0");
    expect(screen.getByText("1/3 条有数据；未返回不计为 0")).toBeInTheDocument();
  });

  it("runs only after a click and prevents duplicate paid runs", async () => {
    let finish!: (value: JevBenchmarkSnapshot) => void;
    invokeMock.mockImplementation((command: string) => command === "run_jev_benchmark"
      ? new Promise((resolve) => { finish = resolve; }) : Promise.resolve(empty));
    const { onRunningChange, onComplete } = renderPanel();
    const button = await screen.findByRole("button", { name: "运行分类测试" });
    await waitFor(() => expect(button).toBeEnabled());
    fireEvent.click(button);
    fireEvent.click(button);
    expect(invokeMock.mock.calls.filter(([name]) => name === "run_jev_benchmark")).toHaveLength(1);
    expect(screen.getByRole("button", { name: "测试进行中…" })).toBeDisabled();
    expect(onRunningChange).toHaveBeenLastCalledWith(true);
    await act(async () => finish(measured));
    expect(screen.getByText("33.3%")).toBeInTheDocument();
    expect(onRunningChange).toHaveBeenLastCalledWith(false);
    expect(onComplete).toHaveBeenCalledOnce();
  });

  it("blocks model calls without credentials or while another operation is busy", async () => {
    const { rerender } = renderPanel({ keyConfigured: false });
    await screen.findByText(/尚无测试结果/);
    expect(screen.getByRole("button", { name: "运行分类测试" })).toBeDisabled();
    rerender(<JevBenchmarkPanel keyConfigured disabled onRunningChange={vi.fn()} onComplete={vi.fn()} />);
    expect(screen.getByRole("button", { name: "运行分类测试" })).toBeDisabled();
    expect(invokeMock).not.toHaveBeenCalledWith("run_jev_benchmark");
  });

  it("reports errors without automatically repeating an inference request", async () => {
    invokeMock.mockImplementation((command: string) => command === "run_jev_benchmark"
      ? Promise.reject("jev_benchmark_write") : Promise.resolve(empty));
    renderPanel();
    const button = await screen.findByRole("button", { name: "运行分类测试" });
    await waitFor(() => expect(button).toBeEnabled());
    fireEvent.click(button);
    expect(await screen.findByRole("alert")).toHaveTextContent("测试结果未能保存");
    fireEvent.click(screen.getByRole("button", { name: "重新读取测试结果" }));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    expect(invokeMock.mock.calls.filter(([name]) => name === "run_jev_benchmark")).toHaveLength(1);
  });

  it("recovers a running test after navigation by polling reads only", async () => {
    vi.useFakeTimers();
    invokeMock.mockResolvedValueOnce({ ...empty, busy: true }).mockResolvedValue(measured);
    const { onComplete } = renderPanel();
    await act(async () => {});
    expect(screen.getByRole("button", { name: "测试进行中…" })).toBeDisabled();
    await act(async () => { await vi.advanceTimersByTimeAsync(1500); });
    expect(screen.getByText("33.3%")).toBeInTheDocument();
    expect(onComplete).toHaveBeenCalledOnce();
    expect(invokeMock.mock.calls).toEqual([["get_jev_benchmark"], ["get_jev_benchmark"]]);
  });
});
