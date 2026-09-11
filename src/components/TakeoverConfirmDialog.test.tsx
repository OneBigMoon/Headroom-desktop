import { describe, expect, it, vi } from "vitest";
import { act, render, renderHook, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { TakeoverConfirmDialog } from "./TakeoverConfirmDialog";
import { I18nProvider } from "../lib/i18n";
import { useTakeoverConfirm } from "../lib/takeoverConfirm";

function renderDialog(onAnswer: (confirmed: boolean) => void) {
  return render(
    <I18nProvider>
      <TakeoverConfirmDialog
        prompt={{ label: "Codex integration", provider: "Cockpit (codex_local_access)" }}
        onAnswer={onAnswer}
      />
    </I18nProvider>
  );
}

/// The native call this replaced cannot work here: WebKit answers
/// `window.confirm` with "cancel" when the UI delegate does not implement the
/// panel, so the dialog has to be real DOM.
describe("TakeoverConfirmDialog", () => {
  it("names the client and the tool that is routing right now", () => {
    renderDialog(vi.fn());

    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveTextContent("Cockpit (codex_local_access)");
    expect(dialog).toHaveTextContent("Codex integration");
  });

  it("answers true only from the continue button", async () => {
    const onAnswer = vi.fn();
    const user = userEvent.setup();
    renderDialog(onAnswer);

    await user.click(screen.getByRole("button", { name: "Continue" }));

    expect(onAnswer).toHaveBeenCalledWith(true);
  });

  it("treats cancel and a backdrop click as declining", async () => {
    const onAnswer = vi.fn();
    const user = userEvent.setup();
    const { container } = renderDialog(onAnswer);

    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onAnswer).toHaveBeenLastCalledWith(false);

    // The backdrop is the dialog's own element, not the card inside it.
    await user.click(container.querySelector(".modal-backdrop") as HTMLElement);
    expect(onAnswer).toHaveBeenLastCalledWith(false);
    expect(onAnswer).not.toHaveBeenCalledWith(true);
  });
});

describe("useTakeoverConfirm", () => {
  function wrapper({ children }: { children: React.ReactNode }) {
    return <I18nProvider>{children}</I18nProvider>;
  }

  it("resolves the caller's promise with the answer", async () => {
    const { result } = renderHook(() => useTakeoverConfirm(), { wrapper });

    let pending: Promise<boolean> | null = null;
    act(() => {
      pending = result.current.request("Codex integration", "Cockpit");
    });
    expect(result.current.prompt).toMatchObject({ provider: "Cockpit" });

    act(() => result.current.answer(true));
    await expect(pending).resolves.toBe(true);
    expect(result.current.prompt).toBeNull();
  });

  it("releases an unanswered prompt as a decline instead of hanging", async () => {
    const { result } = renderHook(() => useTakeoverConfirm(), { wrapper });

    let first: Promise<boolean> | null = null;
    act(() => {
      first = result.current.request("Codex integration", "Cockpit");
    });
    let second: Promise<boolean> | null = null;
    act(() => {
      second = result.current.request("Codex integration", "Cockpit");
    });

    await expect(first).resolves.toBe(false);
    act(() => result.current.answer(true));
    await expect(second).resolves.toBe(true);
  });

  it("treats Escape as declining", async () => {
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
