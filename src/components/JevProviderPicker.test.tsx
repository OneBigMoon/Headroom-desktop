import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { JevProviderPicker, type JevProviderOption } from "./JevProviderPicker";

const options: JevProviderOption[] = [
  { id: "typesafe", label: "TypeSafe Jev" },
  { id: "anthropic", label: "Anthropic" },
  { id: "openrouter", label: "OpenRouter" },
  { id: "custom", label: "Custom endpoint" },
];

function renderPicker(overrides: Partial<Parameters<typeof JevProviderPicker>[0]> = {}) {
  return render(
    <JevProviderPicker
      onChange={vi.fn()}
      options={options}
      value="anthropic"
      {...overrides}
    />,
  );
}

describe("JevProviderPicker", () => {
  it("opens from the native trigger and exposes the dialog options", async () => {
    const user = userEvent.setup();
    renderPicker();

    await user.click(screen.getByRole("button", { name: "Anthropic" }));

    expect(screen.getByRole("dialog", { name: "选择提供方" })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Anthropic" })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("radio", { name: "TypeSafe Jev" })).toBeInTheDocument();
  });

  it("selects an option, closes, and returns focus to the trigger", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    renderPicker({ onChange });
    const trigger = screen.getByRole("button", { name: "Anthropic" });

    await user.click(trigger);
    await user.click(screen.getByRole("radio", { name: "OpenRouter" }));

    expect(onChange).toHaveBeenCalledWith("openrouter");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });

  it("closes on Escape and outside click", async () => {
    const user = userEvent.setup();
    renderPicker();
    const trigger = screen.getByRole("button", { name: "Anthropic" });

    await user.click(trigger);
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();

    await user.click(trigger);
    await user.click(screen.getByLabelText("选择 Jev 提供方"));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("does not open when disabled", async () => {
    const user = userEvent.setup();
    renderPicker({ disabled: true });
    const trigger = screen.getByRole("button", { name: "Anthropic" });

    expect(trigger).toBeDisabled();
    await user.click(trigger);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("supports keyboard activation", async () => {
    const user = userEvent.setup();
    renderPicker();
    const trigger = screen.getByRole("button", { name: "Anthropic" });

    trigger.focus();
    await user.keyboard("{Enter}");
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });
});
