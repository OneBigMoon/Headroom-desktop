import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { SetupStallModal } from "./SetupStallModal";
import { setupStallNoTrafficMinutes } from "../lib/setupHealthAlert";

describe("SetupStallModal", () => {
  it("tells a no-traffic user to restart their agent", () => {
    render(<SetupStallModal kind="no_traffic" onClose={vi.fn()} onOpenSettings={vi.fn()} onContact={vi.fn()} />);

    expect(
      screen.getByText(new RegExp(`${setupStallNoTrafficMinutes()} minutes`))
    ).toBeInTheDocument();
    expect(
      screen.getByText(/Quit your terminal, editor, or coding agent completely/i)
    ).toBeInTheDocument();
  });

  // The restart-your-terminal steps are actively wrong for someone whose only
  // Claude Code is the one inside the Claude desktop app.
  it("warns the no-traffic user about Claude Code in the Claude desktop app", () => {
    render(<SetupStallModal kind="no_traffic" onClose={vi.fn()} onOpenSettings={vi.fn()} onContact={vi.fn()} />);

    expect(screen.getByText(/Claude desktop app/i)).toBeInTheDocument();
    expect(screen.getByText(/doesn't allow setting a proxy URL/i)).toBeInTheDocument();
    // Naming whose limitation this is is the point: without it the note reads
    // as Headroom being broken.
    expect(screen.getByText(/design decision by Anthropic/i)).toBeInTheDocument();
    // Structured rather than one paragraph: a scannable title and a separate
    // actionable line, so the way out isn't buried at the end of the prose.
    expect(
      screen.getByText(/Running Claude Code in the Claude desktop app\?/i)
    ).toHaveClass("setup-stall__note-title");
    expect(screen.getByText(/Run Claude Code from a terminal/i)).toHaveClass(
      "setup-stall__note-action"
    );
  });

  it("leaves that warning off the no-savings branch, where traffic already flows", () => {
    render(<SetupStallModal kind="no_savings" onClose={vi.fn()} onOpenSettings={vi.fn()} onContact={vi.fn()} />);

    expect(screen.queryByText(/Claude desktop app/i)).not.toBeInTheDocument();
  });

  it("points a no-savings user at the optimization controls instead", () => {
    render(<SetupStallModal kind="no_savings" onClose={vi.fn()} onOpenSettings={vi.fn()} onContact={vi.fn()} />);

    expect(screen.getByText(/none of them have been optimized/i)).toBeInTheDocument();
    expect(screen.getByText(/Restart your coding agent/i)).toBeInTheDocument();
    expect(screen.getByText(/codexbox is not paused/i)).toBeInTheDocument();
  });

  // evaluateSetupStall suppresses the alert entirely when the account gate has
  // optimization off, so the plan cannot be the cause by the time this renders.
  // Telling the user to go check it sends them after something already ruled out.
  it("never tells a no-savings user to check their plan", () => {
    render(
      <SetupStallModal kind="no_savings" onClose={vi.fn()} onOpenSettings={vi.fn()} onContact={vi.fn()} />
    );

    expect(screen.queryByText(/plan/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/Upgrade/i)).not.toBeInTheDocument();
  });

  it("puts reconnecting first, ahead of the pause check", () => {
    render(
      <SetupStallModal kind="no_savings" onClose={vi.fn()} onOpenSettings={vi.fn()} onContact={vi.fn()} />
    );

    const steps = screen.getAllByRole("listitem").map((item) => item.textContent ?? "");
    expect(steps[0]).toMatch(/Restart your coding agent/i);
  });

  it.each(["no_traffic", "no_savings"] as const)(
    "offers a support escape hatch on the %s branch",
    async (kind) => {
      const onContact = vi.fn();
      render(
        <SetupStallModal kind={kind} onClose={vi.fn()} onOpenSettings={vi.fn()} onContact={onContact} />
      );

      await userEvent.click(screen.getByRole("button", { name: "Email us" }));

      expect(onContact).toHaveBeenCalledTimes(1);
    }
  );

  it("dismisses via the Dismiss button", async () => {
    const onClose = vi.fn();
    render(<SetupStallModal kind="no_traffic" onClose={onClose} onOpenSettings={vi.fn()} onContact={vi.fn()} />);

    await userEvent.click(screen.getByRole("button", { name: "Dismiss" }));

    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("dismisses when the backdrop is clicked but not the card itself", async () => {
    const onClose = vi.fn();
    render(<SetupStallModal kind="no_traffic" onClose={onClose} onOpenSettings={vi.fn()} onContact={vi.fn()} />);

    await userEvent.click(screen.getByRole("heading", { level: 3 }));
    expect(onClose).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole("dialog"));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("hands the user to settings", async () => {
    const onOpenSettings = vi.fn();
    render(
      <SetupStallModal kind="no_savings" onClose={vi.fn()} onOpenSettings={onOpenSettings} onContact={vi.fn()} />
    );

    await userEvent.click(screen.getByRole("button", { name: "Open settings" }));

    expect(onOpenSettings).toHaveBeenCalledTimes(1);
  });
});
