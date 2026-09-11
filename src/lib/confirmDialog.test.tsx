import { describe, expect, it } from "vitest";
import { act, renderHook } from "@testing-library/react";

import { useConfirmDialog, type ConfirmPrompt } from "./confirmDialog";

const prompt: ConfirmPrompt = {
  title: "Remove this Community edition's local configuration?",
  body: "This does not change the official Headroom app.",
  confirmLabel: "Uninstall and quit",
  cancelLabel: "Cancel",
};

describe("useConfirmDialog", () => {
  it("resolves the caller's promise with the answer", async () => {
    const { result } = renderHook(() => useConfirmDialog());

    let pending: Promise<boolean> | null = null;
    act(() => {
      pending = result.current.request(prompt);
    });
    expect(result.current.prompt).toEqual(prompt);

    act(() => result.current.answer(true));
    await expect(pending).resolves.toBe(true);
    expect(result.current.prompt).toBeNull();
  });

  it("releases an unanswered prompt as a decline instead of hanging", async () => {
    const { result } = renderHook(() => useConfirmDialog());

    let first: Promise<boolean> | null = null;
    act(() => {
      first = result.current.request(prompt);
    });
    let second: Promise<boolean> | null = null;
    act(() => {
      second = result.current.request(prompt);
    });

    await expect(first).resolves.toBe(false);
    act(() => result.current.answer(true));
    await expect(second).resolves.toBe(true);
  });

  it("treats Escape as declining", async () => {
    const { result } = renderHook(() => useConfirmDialog());

    let pending: Promise<boolean> | null = null;
    act(() => {
      pending = result.current.request(prompt);
    });
    act(() => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });

    await expect(pending).resolves.toBe(false);
    expect(result.current.prompt).toBeNull();
  });
});
