import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ExtensionClientToggle } from "./ExtensionClientToggle";

const labels = {
  enabledLabel: "Enabled",
  disabledLabel: "Disabled",
  enableLabel: "Enable",
  disableLabel: "Disable",
};

describe("ExtensionClientToggle", () => {
  it("shows the Codex icon and controlled state with the next action", async () => {
    const onToggle = vi.fn();
    const { rerender } = render(<ExtensionClientToggle {...labels} enabled={false} onToggle={onToggle} />);
    const toggle = screen.getByRole("switch", { name: "Enable: Codex" });

    expect(toggle).not.toBeChecked();
    expect(toggle).toHaveAttribute("title", "Enable: Codex");
    expect(toggle).toHaveTextContent("Codex");
    expect(toggle).toHaveTextContent("Disabled");
    expect(toggle.querySelector("svg")).toHaveAttribute("fill", "currentColor");
    expect(toggle).not.toHaveClass("is-enabled");

    await userEvent.setup().click(toggle);
    expect(onToggle).toHaveBeenCalledOnce();
    expect(toggle).not.toBeChecked();

    rerender(<ExtensionClientToggle {...labels} enabled onToggle={onToggle} />);
    expect(screen.getByRole("switch", { name: "Disable: Codex" })).toBeChecked();
    expect(toggle).toHaveClass("is-enabled");
    expect(toggle).toHaveTextContent("Enabled");
  });

  it("uses native keyboard activation and ignores input while disabled", async () => {
    const onToggle = vi.fn();
    const user = userEvent.setup();
    const { rerender } = render(<ExtensionClientToggle {...labels} enabled onToggle={onToggle} />);
    const toggle = screen.getByRole("switch");
    toggle.focus();
    await user.keyboard(" ");
    expect(onToggle).toHaveBeenCalledOnce();

    rerender(<ExtensionClientToggle {...labels} enabled disabled onToggle={onToggle} />);
    await user.click(toggle);
    await user.keyboard("{Enter}");
    expect(toggle).toBeDisabled();
    expect(toggle).toBeChecked();
    expect(onToggle).toHaveBeenCalledOnce();
  });
});
