import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import type { RuntimeStatus } from "../lib/types";
import { RuntimeStatusIndicator } from "./RuntimeStatusIndicator";

const healthy: RuntimeStatus = {
  platform: "macOS",
  supportTier: "native",
  installed: true,
  running: true,
  starting: false,
  paused: false,
  autoPaused: false,
  bypassed: false,
  proxyReachable: true,
  headroomLearnSupported: true,
  rtk: { installed: true, enabled: true, pathConfigured: true, hookConfigured: true },
};

function indicator(runtime: RuntimeStatus | null) {
  return <RuntimeStatusIndicator runtime={runtime} label="Runs on this Mac" className="tray-panel__local-status" />;
}

describe("RuntimeStatusIndicator", () => {
  it.each([
    ["healthy", {}, "running"],
    ["starting before health checks finish", { starting: true }, "starting"],
    ["retrying an earlier failure", { starting: true, startupError: "old failure", autoPaused: true }, "starting"],
    ["startup failed", { running: false, proxyReachable: false, startupError: "failed" }, "error"],
    ["watchdog auto-paused", { paused: true, autoPaused: true }, "error"],
    ["process alive but proxy unreachable", { proxyReachable: false }, "error"],
    ["proxy reachable without the managed process", { running: false }, "error"],
    ["manually paused with a lingering process", { paused: true }, "stopped"],
    ["intentionally bypassed", { bypassed: true }, "stopped"],
    ["not installed", { installed: false, running: false, proxyReachable: false }, "stopped"],
    ["stopped", { running: false, proxyReachable: false }, "stopped"],
  ] as const)("shows the correct state when %s", (_description, patch, expected) => {
    render(indicator({ ...healthy, ...patch }));
    expect(screen.getByRole("status")).toHaveAttribute("data-runtime-state", expected);
  });

  it("shows checking rather than a false healthy state before the first response", () => {
    render(indicator(null));
    const status = screen.getByRole("status");
    expect(status).toHaveAttribute("data-runtime-state", "starting");
    expect(status).toHaveAttribute("title", "Checking local proxy");
  });

  it("updates through stopped, starting, running, and failed polling responses", () => {
    const { rerender } = render(indicator({ ...healthy, running: false, proxyReachable: false }));
    expect(screen.getByRole("status")).toHaveAttribute("data-runtime-state", "stopped");
    rerender(indicator({ ...healthy, starting: true }));
    expect(screen.getByRole("status")).toHaveAttribute("data-runtime-state", "starting");
    rerender(indicator(healthy));
    expect(screen.getByRole("status", { name: "Runs on this Mac: Proxy online" })).toHaveAttribute("data-runtime-state", "running");
    rerender(indicator({ ...healthy, startupError: "connection failed" }));
    expect(screen.getByRole("status")).toHaveAttribute("data-runtime-state", "error");
  });
});
