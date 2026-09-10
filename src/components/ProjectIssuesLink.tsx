import { useId, useState, type ReactNode } from "react";
import { ArrowSquareOut } from "@phosphor-icons/react";
import { useI18n } from "../lib/i18n";

export const PROJECT_ISSUES_URL =
  "https://github.com/OneBigMoon/Headroom-desktop/issues/new";

export interface ProjectIssuesLinkProps {
  children: ReactNode;
  onOpen: (url: string) => void | Promise<void>;
}

export function ProjectIssuesLink({ children, onOpen }: ProjectIssuesLinkProps) {
  const { t } = useI18n();
  const errorId = useId();
  const [openError, setOpenError] = useState<string | null>(null);

  async function openProjectIssues() {
    setOpenError(null);
    try {
      await onOpen(PROJECT_ISSUES_URL);
    } catch (error) {
      console.error("Failed to open project issues", error);
      setOpenError(t("issues.openFailed"));
    }
  }

  return (
    <>
      <button
        aria-describedby={openError ? errorId : undefined}
        className="addon-card__link project-issues-link"
        onClick={() => void openProjectIssues()}
        type="button"
      >
        <span>{children}</span>
        <ArrowSquareOut aria-hidden="true" size={13} weight="bold" />
      </button>
      {openError ? (
        <span className="project-issues-link__error" id={errorId} role="alert">
          {openError}
        </span>
      ) : null}
    </>
  );
}
