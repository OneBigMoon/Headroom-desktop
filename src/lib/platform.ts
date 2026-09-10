import type { Translate } from "./i18n";

export const PROJECT_ISSUES_NEW_URL =
  "https://github.com/OneBigMoon/Headroom-desktop/issues/new";

export function platformPreviewNoticeFor(
  platform: string | undefined,
  supportTier: string | undefined,
  t: Translate,
): string | null {
  if (supportTier !== "experimental") {
    return null;
  }
  if (platform === "linux" || platform === "windows" || platform === "win32") {
    const name = platform === "linux" ? "Linux" : "Windows";
    return t("platform.previewNamed", { platform: name });
  }
  return t("platform.previewGeneric");
}

/** Pre-filled GitHub issue for preview-platform reports. */
export function platformPreviewSupportIssueUrl(
  context: {
    platform: string | undefined;
    appVersion: string;
    headroomVersion: string;
  },
  t: Translate,
): string {
  const platformName = context.platform ?? t("platform.unknown");
  const subject = t("platform.issueSubject", { platform: platformName });
  const body =
    `${t("platform.issuePrompt")}\n\n\n` +
    "---\n" +
    `${t("platform.issueDiagnostics")}\n` +
    [
      t("platform.issuePlatform", { platform: platformName }),
      t("platform.issueAppVersion", { version: context.appVersion }),
      t("platform.issueCliVersion", { version: context.headroomVersion }),
    ].join("\n");
  return `${PROJECT_ISSUES_NEW_URL}?title=${encodeURIComponent(
    subject,
  )}&body=${encodeURIComponent(body)}`;
}
