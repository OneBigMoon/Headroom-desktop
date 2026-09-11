import { useCallback } from "react";

import { useConfirmDialog, type ConfirmPrompt } from "./confirmDialog";
import { useI18n } from "./i18n";

export interface TakeoverConfirm {
  prompt: ConfirmPrompt | null;
  /// Resolves true only when the user accepts. Both callers -- the settings
  /// switch and the launcher's auto-configure loop -- await this instead of
  /// `window.confirm`.
  request: (label: string, provider: string) => Promise<boolean>;
  answer: (confirmed: boolean) => void;
}

/// Ask before a connector takes over the route another tool owns right now.
///
/// The question is posed by the shared in-app dialog; see `useConfirmDialog`
/// for why the native `window.confirm` cannot be used here.
export function useTakeoverConfirm(): TakeoverConfirm {
  const { t } = useI18n();
  const { prompt, request, answer } = useConfirmDialog();

  const ask = useCallback(
    (label: string, provider: string) =>
      request({
        title: t("connections.enableConnector", { name: label }),
        body: t("connections.setup.takeoverConfirm", { provider }),
        confirmLabel: t("actions.continue"),
        cancelLabel: t("actions.cancel"),
      }),
    [request, t],
  );

  return { prompt, request: ask, answer };
}
