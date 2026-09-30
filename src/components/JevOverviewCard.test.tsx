import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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
});
