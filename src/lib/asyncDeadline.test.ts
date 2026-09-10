import { describe, expect, it, vi } from "vitest";

import { withDeadline } from "./asyncDeadline";

describe("withDeadline", () => {
  it("passes a settled value straight through", async () => {
    await expect(withDeadline(Promise.resolve("done"), 1000)).resolves.toBe("done");
  });

  it("resolves null when the promise never settles", async () => {
    vi.useFakeTimers();
    try {
      const pending = withDeadline(new Promise<string>(() => undefined), 5000);
      await vi.advanceTimersByTimeAsync(5000);
      await expect(pending).resolves.toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps waiting past the deadline for a slow-but-answering call", async () => {
    vi.useFakeTimers();
    try {
      let resolveCall: (value: string) => void = () => undefined;
      const call = new Promise<string>((resolve) => {
        resolveCall = resolve;
      });
      const bounded = withDeadline(call, 5000);
      await vi.advanceTimersByTimeAsync(4000);
      resolveCall("late");
      await expect(bounded).resolves.toBe("late");
      // The deadline timer was cleared, not left to fire afterwards.
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("propagates a rejection instead of reporting a timeout", async () => {
    await expect(withDeadline(Promise.reject(new Error("nope")), 1000)).rejects.toThrow("nope");
  });
});
