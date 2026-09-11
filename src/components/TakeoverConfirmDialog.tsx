import { useI18n } from "../lib/i18n";
import type { TakeoverPrompt } from "../lib/takeoverConfirm";

export interface TakeoverConfirmDialogProps {
  prompt: TakeoverPrompt;
  onAnswer: (confirmed: boolean) => void;
}

/// In-app replacement for the native `window.confirm` this flow used to call.
/// See `useTakeoverConfirm` for why the native one could not be used here.
export function TakeoverConfirmDialog({ prompt, onAnswer }: TakeoverConfirmDialogProps) {
  const { t } = useI18n();
  return (
    <div
      className="modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="takeover-confirm-title"
      onClick={() => onAnswer(false)}
    >
      <div className="modal-card" onClick={(event) => event.stopPropagation()}>
        <h3 id="takeover-confirm-title">
          {t("connections.enableConnector", { name: prompt.label })}
        </h3>
        <p>{t("connections.setup.takeoverConfirm", { provider: prompt.provider })}</p>
        <div className="modal-actions">
          <button
            className="secondary-button"
            onClick={() => onAnswer(false)}
            type="button"
          >
            {t("actions.cancel")}
          </button>
          <button
            className="primary-button"
            onClick={() => onAnswer(true)}
            type="button"
          >
            {t("actions.continue")}
          </button>
        </div>
      </div>
    </div>
  );
}
