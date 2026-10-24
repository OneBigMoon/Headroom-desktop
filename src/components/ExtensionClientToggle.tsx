import { ConnectorIcon } from "./ConnectorIcon";
import "./ExtensionClientToggle.css";

export interface ExtensionClientToggleProps {
  enabled: boolean;
  disabled?: boolean;
  onToggle: () => void;
  enabledLabel: string;
  disabledLabel: string;
  enableLabel: string;
  disableLabel: string;
}

export function ExtensionClientToggle({
  enabled,
  disabled = false,
  onToggle,
  enabledLabel,
  disabledLabel,
  enableLabel,
  disableLabel,
}: ExtensionClientToggleProps) {
  const actionLabel = `${enabled ? disableLabel : enableLabel}: Codex`;
  return (
    <button
      className={`extension-client-toggle${enabled ? " is-enabled" : ""}`}
      type="button"
      role="switch"
      aria-checked={enabled}
      aria-label={actionLabel}
      title={actionLabel}
      disabled={disabled}
      onClick={onToggle}
    >
      <span className="extension-client-toggle__icon"><ConnectorIcon clientId="codex" size={22} /></span>
      <span className="extension-client-toggle__copy">
        <strong>Codex</strong>
        <small>{enabled ? enabledLabel : disabledLabel}</small>
      </span>
    </button>
  );
}
