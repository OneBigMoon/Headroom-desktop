import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { JevOverviewCard } from "./JevOverviewCard";
import type { JevRecord } from "./JevPanel";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }));

const nativeRecord: JevRecord = {
  id: "native-1",
  timestamp: "2026-09-24T01:00:00Z",
  provider: "typesafe",
  endpoint: null,
  model: "jev-latest",
  status: "success",
  latencyMs: 120,
  inputTokens: 12,
  outputTokens: 4,
  choice: "coding",
  confidence: 0.8,
  error: null,
};

describe("JevOverviewCard", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  afterEach(() => vi.useRealTimers());

  it("refreshes automatic records while active and stops on leaving the overview", async () => {
    vi.useFakeTimers();
    invokeMock.mockResolvedValue({ autoEnabled: true, records: [] });
    const { rerender } = render(<JevOverviewCard active onOpenDecisions={vi.fn()} />);
    await act(async () => {});
    expect(invokeMock).toHaveBeenCalledTimes(1);
    invokeMock.mockResolvedValue({ autoEnabled: true, records: [{ ...nativeRecord, source: "codex" }] });
    await act(async () => { await vi.advanceTimersByTimeAsync(4000); });
    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(screen.getByText("coding")).toBeInTheDocument();
    rerender(<JevOverviewCard active={false} onOpenDecisions={vi.fn()} />);
    await act(async () => { await vi.advanceTimersByTimeAsync(12000); });
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });

  it("shows the empty state without evaluating or exposing credentials", async () => {
    invokeMock.mockResolvedValue({ records: [] });
    const onOpenDecisions = vi.fn();
    render(<JevOverviewCard active onOpenDecisions={onOpenDecisions} />);

    expect(await screen.findByText(/还没有决策记录/)).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("get_jev_dashboard");
    fireEvent.click(screen.getByRole("button", { name: "打开决策" }));
    expect(onOpenDecisions).toHaveBeenCalledOnce();
  });

  it("separates reported token totals from missing usage and scopes choices to the latest target", async () => {
    invokeMock.mockResolvedValue({
      records: [
        nativeRecord,
        { ...nativeRecord, id: "other-1", provider: "deepseek", endpoint: "https://api.deepseek.com/chat/completions", model: "deepseek-chat", outputTokens: null, choice: "research", latencyMs: 180 },
        { ...nativeRecord, id: "failed-1", status: "error", outputTokens: null, choice: null, latencyMs: 300 },
        { ...nativeRecord, id: "native-2", outputTokens: 6, choice: "writing", latencyMs: 150 },
      ],
    });
    render(<JevOverviewCard active onOpenDecisions={vi.fn()} />);

    expect(await screen.findByText("75.0%")).toBeInTheDocument();
    expect(screen.getByText("调用成功率")).toBeInTheDocument();
    expect(screen.getByText(/Jev 仅分类观察，未替代 Codex 请求/)).toBeInTheDocument();
    expect(screen.getByText("300 ms")).toBeInTheDocument();
    expect(screen.getByText("输出 token（已上报）").parentElement).toHaveTextContent("10");
    expect(screen.getByText("2/4 条有数据；未返回不计为 0")).toBeInTheDocument();
    expect(screen.getByRole("img", { name: "成功 3 条，失败 1 条" })).toBeInTheDocument();
    expect(screen.getByText("jev-latest · TypeSafe Jev · 2 条")).toBeInTheDocument();
    expect(screen.getByText("coding")).toBeInTheDocument();
    expect(screen.getByText("writing")).toBeInTheDocument();
    expect(screen.queryByText("research")).not.toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalledWith("evaluate_jev", expect.anything());
  });

  it("keeps unknown output distinct from a reported zero", async () => {
    invokeMock.mockResolvedValueOnce({ records: [{ ...nativeRecord, outputTokens: null }] });
    const { rerender } = render(<JevOverviewCard active onOpenDecisions={vi.fn()} />);
    expect(await screen.findByText("暂无数据")).toBeInTheDocument();
    expect(screen.getByText("0/1 条有数据；未返回不计为 0")).toBeInTheDocument();

    rerender(<JevOverviewCard active={false} onOpenDecisions={vi.fn()} />);
    invokeMock.mockResolvedValueOnce({ records: [{ ...nativeRecord, outputTokens: 0 }] });
    rerender(<JevOverviewCard active onOpenDecisions={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("1/1 条有数据；未返回不计为 0")).toBeInTheDocument());
    expect(screen.getByText("输出 token（已上报）").parentElement).toHaveTextContent("0");
  });

  it("offers a retry after a dashboard read error", async () => {
    invokeMock.mockRejectedValueOnce(new Error("unavailable")).mockResolvedValueOnce({ records: [] });
    render(<JevOverviewCard active onOpenDecisions={vi.fn()} />);

    expect(await screen.findByRole("alert")).toHaveTextContent("暂时无法读取决策记录");
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByText(/还没有决策记录/)).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });

  it("warns when automatic records are incomplete while retaining the readable metrics", async () => {
    invokeMock.mockResolvedValue({
      autoError: "jev_auto_records_read",
      records: [nativeRecord],
    });
    render(<JevOverviewCard active onOpenDecisions={vi.fn()} />);

    expect(await screen.findByRole("alert")).toHaveTextContent("记录不完整");
    expect(screen.getByRole("alert")).toHaveTextContent("以下指标仅包含已读取的记录");
    expect(screen.getByText("请求数").parentElement).toHaveTextContent("1");
    expect(screen.getByText("100.0%")).toBeInTheDocument();
    expect(screen.getByText("输出 token（已上报）").parentElement).toHaveTextContent("4");
    expect(screen.getByRole("img", { name: "成功 1 条，失败 0 条" })).toBeInTheDocument();
    expect(screen.queryByText(/还没有决策记录/)).not.toBeInTheDocument();
    expect(screen.queryByText("jev_auto_records_read")).not.toBeInTheDocument();
  });

  it("does not claim history is empty when no records could be read and allows retry", async () => {
    invokeMock
      .mockResolvedValueOnce({ autoError: "jev_auto_records_read", records: [] })
      .mockResolvedValueOnce({ autoError: null, records: [] });
    render(<JevOverviewCard active onOpenDecisions={vi.fn()} />);

    expect(await screen.findByRole("alert")).toHaveTextContent("记录不完整");
    expect(screen.getByRole("alert")).toHaveTextContent("目前没有可显示的记录");
    expect(screen.queryByText(/还没有决策记录/)).not.toBeInTheDocument();
    expect(screen.queryByText("调用成功率")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByText(/还没有决策记录/)).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(invokeMock).toHaveBeenCalledWith("get_jev_dashboard");
  });
});
