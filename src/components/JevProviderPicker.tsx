import { useEffect, useRef, useState } from "react";

import "./JevProviderPicker.css";

export type JevProviderOption = {
  id: string;
  label: string;
};

export function JevProviderPicker({
  value,
  options,
  onChange,
  disabled = false,
}: {
  value: string;
  options: JevProviderOption[];
  onChange: (id: string) => void;
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const selectedOption = options.find((option) => option.id === value);

  const close = () => {
    setOpen(false);
    triggerRef.current?.focus();
  };

  useEffect(() => {
    if (!open) return;

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        close();
      } else if (event.key === "Tab") {
        const buttons = dialogRef.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)");
        if (!buttons?.length) return;
        const first = buttons[0];
        const last = buttons[buttons.length - 1];
        if (event.shiftKey && document.activeElement === first) {
          event.preventDefault();
          last.focus();
        } else if (!event.shiftKey && document.activeElement === last) {
          event.preventDefault();
          first.focus();
        }
      }
    };

    document.addEventListener("keydown", handleKeyDown);
    const frame = requestAnimationFrame(() => {
      dialogRef.current?.querySelector<HTMLButtonElement>(".jev-provider-picker__close")?.focus();
    });

    return () => {
      document.removeEventListener("keydown", handleKeyDown);
      cancelAnimationFrame(frame);
    };
  }, [open]);

  useEffect(() => {
    if (disabled && open) close();
  }, [disabled, open]);

  const selectOption = (id: string) => {
    onChange(id);
    close();
  };

  return (
    <div className="jev-provider-picker">
      <button
        ref={triggerRef}
        aria-expanded={open}
        aria-haspopup="dialog"
        className="jev-provider-picker__trigger"
        disabled={disabled}
        onClick={() => setOpen(true)}
        type="button"
      >
        <span className="jev-provider-picker__trigger-label">
          {selectedOption?.label ?? value}
        </span>
        <span aria-hidden="true" className="jev-provider-picker__chevron">⌄</span>
      </button>

      {open ? (
        <div
          aria-label="选择 Jev 提供方"
          className="jev-provider-picker__backdrop"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget) close();
          }}
        >
          <div
            aria-labelledby="jev-provider-picker-title"
            aria-modal="true"
            className="jev-provider-picker__dialog"
            ref={dialogRef}
            role="dialog"
          >
            <div className="jev-provider-picker__header">
              <div>
                <p className="jev-provider-picker__eyebrow">Jev</p>
                <h2 id="jev-provider-picker-title">选择提供方</h2>
              </div>
              <button
                aria-label="关闭提供方选择"
                className="jev-provider-picker__close"
                onClick={close}
                type="button"
              >
                ×
              </button>
            </div>

            <div aria-label="Jev 提供方" className="jev-provider-picker__options" role="radiogroup">
              {options.map((option) => (
                <button
                  aria-checked={option.id === value}
                  className={`jev-provider-picker__option${option.id === value ? " is-selected" : ""}${option.id === "typesafe" || option.id === "custom" ? " is-wide" : ""}`}
                  key={option.id}
                  onClick={() => selectOption(option.id)}
                  role="radio"
                  type="button"
                >
                  <span className="jev-provider-picker__option-mark" aria-hidden="true">
                    {option.id === value ? "✓" : ""}
                  </span>
                  <span>{option.label}</span>
                </button>
              ))}
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
