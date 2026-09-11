import type { ReactNode } from "react";
import { describe, expect, it } from "vitest";
import { act, renderHook } from "@testing-library/react";

import { I18nProvider } from "./i18n";
import { useTakeoverConfirm } from "./takeoverConfirm";

function wrapper({ children }: { children: ReactNode }) {
  return <I18nProvider>{children}</I18nProvider>;
}

/// The question is posed in-app; see `useConfirmDialog` for why the native
/// `window.confirm` cannot be used in this shell.
describe("useTakeoverConfirm", () => {
  it("names the client and the tool that owns the route right now", () => {
    const { result } = renderHook(() => useTakeoverConfirm(), { wrapper });

    act(() => {
      void result.current.request("Codex integration", "Cockpit (codex_local_access)");
    });

    expect(result.current.prompt?.title).toContain("Codex integration");
    expect(result.current.prompt?.body).toContain("Cockpit (codex_local_access)");
    expect(result.current.prompt?.confirmLabel).toBe("Continue");
    expect(result.current.prompt?.cancelLabel).toBe("Cancel");
  });

  it("resolves the caller's promise with the answer", async () => {
    const { result } = renderHook(() => useTakeoverConfirm(), { wrapper });

    let pending: Promise<boolean> | null = null;
    act(() => {
      pending = result.current.request("Codex integration", "Cockpit");
    });
    act(() => result.current.answer(true));

    await expect(pending).resolves.toBe(true);
    expect(result.current.prompt).toBeNull();
  });

  it("leaves the route alone when the question is declined", async () => {
    const { result } = renderHook(() => useTakeoverConfirm(), { wrapper });

    let pending: Promise<boolean> | null = null;
    act(() => {
      pending = result.current.request("Codex integration", "Cockpit");
    });
    act(() => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });

    await expect(pending).resolves.toBe(false);
    expect(result.current.prompt).toBeNull();
  });
});
