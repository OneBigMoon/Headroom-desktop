import { useId } from "react";

import type { ConfirmPrompt } from "../lib/confirmDialog";

export interface ConfirmDialogProps {
  prompt: ConfirmPrompt;
  onAnswer: (confirmed: boolean) => void;
}

/// In-app replacement for the native `window.confirm` this app used to call.
/// See `useConfirmDialog` for why the native one cannot work here.
export function ConfirmDialog({ prompt, onAnswer }: ConfirmDialogProps) {
  const titleId = `${useId()}-title`;
  return (
    <div
      className="modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby={titleId}
      onClick={() => onAnswer(false)}
    >
      <div className="modal-card" onClick={(event) => event.stopPropagation()}>
        <h3 id={titleId}>{prompt.title}</h3>
        <p>{prompt.body}</p>
        <div className="modal-actions">
          <button className="secondary-button" onClick={() => onAnswer(false)} type="button">
            {prompt.cancelLabel}
          </button>
          <button className="primary-button" onClick={() => onAnswer(true)} type="button">
            {prompt.confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
