import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ConfirmDialog } from "./ConfirmDialog";

const prompt = {
  title: "Switch to Superpowers?",
  body: "Only one tool in this group can be enabled. Switching turns off OpenSpec.",
  confirmLabel: "Switch to Superpowers",
  cancelLabel: "Cancel",
};

function renderDialog(onAnswer: (confirmed: boolean) => void) {
  return render(<ConfirmDialog prompt={prompt} onAnswer={onAnswer} />);
}

/// The native call this replaced cannot work here: WebKit answers
/// `window.confirm` with "cancel" when the UI delegate does not implement the
/// panel, so the question has to be real DOM.
describe("ConfirmDialog", () => {
  it("shows the question and names both answers", () => {
    renderDialog(vi.fn());

    const dialog = screen.getByRole("dialog", { name: prompt.title });
    expect(dialog).toHaveTextContent(prompt.body);
    expect(screen.getByRole("button", { name: "Switch to Superpowers" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeInTheDocument();
  });

  it("answers true only from the confirm button", async () => {
    const onAnswer = vi.fn();
    const user = userEvent.setup();
    renderDialog(onAnswer);

    await user.click(screen.getByRole("button", { name: "Switch to Superpowers" }));

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
