import { useCallback, useEffect, useRef, useState } from "react";
import { useI18n, type ResolvedLocale } from "./i18n";

const en = {
  searchMcp: "Search MCP servers", searchSkills: "Search installed skills", noMatches: "No matching entries.",
  skillViews: "Skill views", installedTab: "Installed", discoverTab: "Discover", searchDiscoveries: "Search available skills", discoveryHint: "Choose a public repository or a local folder. Skill directories are detected automatically and installation uses the global Codex location.", refreshRepository: "Refresh repository", importLocal: "Import local folder", loadRepository: "Load repository", repositorySource: "Repository", installSkill: "Install",
  jsonExample: "Configuration example", importPreview: "Import preview", newEntry: "New", existingEntry: "Name already exists",
  previewHint: "Preview shows names and transports only. Credentials are not displayed here.",
  importBlocked: "Change conflicting names before importing. The entire batch is rejected if any name already exists.",
  savedResult: "Configuration saved.", mcpImportResult: "Imported {count} MCP server(s).",
  skillImportResult: "Imported {success}; failed {failed}.", failedSelections: "Failed items remain selected so you can retry.",
  mcpActivation: "Reload MCPs in Codex or restart Codex to apply the saved configuration.",
  skillActivation: "Start a new Codex session to use these skill changes.",
  invalidSkill: "This skill is incomplete. SKILL.md must include valid name and description metadata.",
  mcpTitle: "MCP servers", mcpSubtitle: "Codex configuration is detected automatically. Manage connections and custom servers without setting a directory.",
  skillsTitle: "Skills", skillsSubtitle: "Installed Codex skills are detected automatically. Discover and install skills without configuring directories.",
  refresh: "Refresh", loading: "Loading…", busy: "Working…", emptyMcp: "No MCP servers configured.",
  emptySkills: "No skills found.", add: "Add server", edit: "Edit", save: "Save", cancel: "Cancel",
  enable: "Enable", disable: "Disable", enabled: "Enabled", disabled: "Disabled", remove: "Delete",
  name: "Name", transport: "Transport", command: "Command", args: "Arguments (JSON array)",
  env: "Environment values", variable: "Variable", value: "Value", addVariable: "Add variable",
  envVars: "Forwarded environment variable names", envVarsHint: "One name per line; values stay in your environment.",
  url: "Server URL", bearerEnv: "Bearer token environment variable", headers: "HTTP headers", addHeader: "Add header",
  importJson: "Import JSON", importAction: "Import", jsonHint: "Paste an MCP configuration. Existing names are preserved when a conflict occurs.",
  invalidJson: "Enter valid JSON.", invalidFields: "Check the required fields and JSON argument array.",
  conflict: "A conflicting entry already exists. Your changes have been kept; choose another name or destination.",
  failed: "The operation failed. Your current data has been kept. Try again.",
  deleteTitle: "Delete this server?", deleteBody: "The server configuration will be removed. This cannot be undone here.",
  managedBelow: "Managed by CodexBox. Use the tool cards below.", readonly: "This configuration is read-only here. Edit it in the Codex configuration.",
  close: "Close", project: "Project directory", projectHint: "Optional absolute path. Scan the selected project's skill directories.",
  scan: "Scan", localSource: "Local source directory", scanSource: "Scan source", sourceSkills: "Skills in source",
  selectSkills: "Select skills to import", importSelected: "Import selected", target: "Import destination",
  global: "Global", projectTarget: "Project", repository: "Public Git repository (HTTPS)", subdirectory: "Skill subdirectory",
  importGit: "Import from Git", readSkill: "View SKILL.md", skillFile: "SKILL.md content", path: "Path",
  selectionRequired: "Select at least one source skill.", projectRequired: "Enter the destination project directory.",
  sourceRequired: "Enter a public HTTPS repository URL without credentials.",
  select: "Select", warning: "Scan notices", unavailable: "This skill is managed elsewhere and cannot be toggled here.",
  globalScope: "Global", projectScope: "Project", builtInScope: "Built-in", adminScope: "Administrator", localScope: "Local source",
} as const;

export type ExtensionCopyKey = keyof typeof en;
type ExtensionCopy = Record<ExtensionCopyKey, string>;
export const extensionCopy: Record<ResolvedLocale, ExtensionCopy> = {
  en,
  "zh-CN": {
    searchMcp: "搜索 MCP 服务器", searchSkills: "搜索已安装技能", noMatches: "没有匹配的条目。",
    skillViews: "技能视图", installedTab: "已安装", discoverTab: "发现", searchDiscoveries: "搜索可安装技能", discoveryHint: "请选择公开仓库或本地文件夹。自动识别其中的技能目录，安装到 Codex 全局位置。", refreshRepository: "刷新技能库", importLocal: "导入本地文件夹", loadRepository: "加载仓库", repositorySource: "当前仓库", installSkill: "安装",
    jsonExample: "配置示例", importPreview: "导入预览", newEntry: "新增", existingEntry: "名称已存在",
    previewHint: "预览仅显示名称和连接方式，不展示凭据内容。",
    importBlocked: "请先修改冲突名称。任一名称已存在时，整批导入都会取消。",
    savedResult: "配置已保存。", mcpImportResult: "已导入 {count} 个 MCP 服务器。",
    skillImportResult: "成功导入 {success} 项，失败 {failed} 项。", failedSelections: "失败项仍保持选中，可修正后重试。",
    mcpActivation: "请在 Codex 中重载 MCP，或重启 Codex，使保存的配置生效。",
    skillActivation: "请开启新的 Codex 会话使用这些技能变更。",
    invalidSkill: "技能内容不完整，SKILL.md 必须包含有效的 name 和 description 元数据。",
    mcpTitle: "MCP 管理", mcpSubtitle: "自动识别 Codex 配置，管理连接与自定义服务器，无需配置目录。", skillsTitle: "Skills 管理", skillsSubtitle: "自动扫描已安装的 Codex 技能，发现和安装技能均无需配置目录。",
    refresh: "刷新", loading: "正在读取…", busy: "处理中…", emptyMcp: "暂无 MCP 服务器。", emptySkills: "未找到技能。",
    add: "添加服务器", edit: "编辑", save: "保存", cancel: "取消", enable: "启用", disable: "停用", enabled: "已启用", disabled: "已停用", remove: "删除",
    name: "名称", transport: "连接方式", command: "命令", args: "参数（JSON 数组）", env: "环境变量值", variable: "变量名", value: "值", addVariable: "添加变量",
    envVars: "转发的环境变量名称", envVarsHint: "每行一个名称，值保留在运行环境中。", url: "服务器 URL", bearerEnv: "Bearer Token 环境变量名称", headers: "HTTP 请求头", addHeader: "添加请求头",
    importJson: "导入 JSON", importAction: "导入", jsonHint: "粘贴 MCP 配置。名称冲突时保留已有配置。", invalidJson: "请输入有效的 JSON。", invalidFields: "请检查必填项及 JSON 参数数组。",
    conflict: "存在同名或目标冲突。已保留本次输入，请更换名称或目标位置。", failed: "操作失败，当前数据已保留，请重试。",
    deleteTitle: "删除此服务器？", deleteBody: "将移除服务器配置，此操作无法在此撤销。", managedBelow: "由 CodexBox 管理，请使用下方工具卡片。", readonly: "此配置在这里仅可读取，请在 Codex 配置中编辑。",
    close: "关闭", project: "项目目录", projectHint: "可选的绝对路径，仅扫描所选项目的技能目录。", scan: "扫描", localSource: "本地来源目录", scanSource: "扫描来源", sourceSkills: "来源中的技能",
    selectSkills: "选择要导入的技能", importSelected: "导入所选", target: "导入位置", global: "全局", projectTarget: "项目", repository: "公开 Git 仓库（HTTPS）", subdirectory: "技能子目录", importGit: "从 Git 导入",
    readSkill: "查看 SKILL.md", skillFile: "SKILL.md 内容", path: "路径", selectionRequired: "请至少选择一个来源技能。", projectRequired: "请输入目标项目目录。", sourceRequired: "请输入不含账号或凭据的公开 HTTPS 仓库地址。",
    select: "选择", warning: "扫描提示", unavailable: "此技能由其他位置管理，无法在这里切换状态。", globalScope: "全局", projectScope: "项目", builtInScope: "内置", adminScope: "管理员", localScope: "本地来源",
  },
  "zh-TW": {
    searchMcp: "搜尋 MCP 伺服器", searchSkills: "搜尋已安裝技能", noMatches: "沒有符合的項目。",
    skillViews: "技能檢視", installedTab: "已安裝", discoverTab: "探索", searchDiscoveries: "搜尋可安裝技能", discoveryHint: "請選擇公開儲存庫或本機資料夾。自動辨識其中的技能目錄，安裝至 Codex 全域位置。", refreshRepository: "重新整理技能庫", importLocal: "匯入本機資料夾", loadRepository: "載入儲存庫", repositorySource: "目前儲存庫", installSkill: "安裝",
    jsonExample: "設定範例", importPreview: "匯入預覽", newEntry: "新增", existingEntry: "名稱已存在",
    previewHint: "預覽僅顯示名稱與連線方式，不顯示憑證內容。",
    importBlocked: "請先修改衝突名稱。任一名稱已存在時，整批匯入都會取消。",
    savedResult: "設定已儲存。", mcpImportResult: "已匯入 {count} 個 MCP 伺服器。",
    skillImportResult: "成功匯入 {success} 項，失敗 {failed} 項。", failedSelections: "失敗項目仍保持勾選，可修正後重試。",
    mcpActivation: "請在 Codex 中重新載入 MCP，或重新啟動 Codex，讓儲存的設定生效。",
    skillActivation: "請開啟新的 Codex 工作階段使用這些技能變更。",
    invalidSkill: "技能內容不完整，SKILL.md 必須包含有效的 name 與 description 中繼資料。",
    mcpTitle: "MCP 管理", mcpSubtitle: "自動辨識 Codex 設定，管理連線與自訂伺服器，無需設定目錄。", skillsTitle: "Skills 管理", skillsSubtitle: "自動掃描已安裝的 Codex 技能，探索與安裝技能均無需設定目錄。",
    refresh: "重新整理", loading: "正在讀取…", busy: "處理中…", emptyMcp: "尚無 MCP 伺服器。", emptySkills: "找不到技能。",
    add: "新增伺服器", edit: "編輯", save: "儲存", cancel: "取消", enable: "啟用", disable: "停用", enabled: "已啟用", disabled: "已停用", remove: "刪除",
    name: "名稱", transport: "連線方式", command: "命令", args: "參數（JSON 陣列）", env: "環境變數值", variable: "變數名稱", value: "值", addVariable: "新增變數",
    envVars: "轉送的環境變數名稱", envVarsHint: "每行一個名稱，值保留在執行環境中。", url: "伺服器 URL", bearerEnv: "Bearer Token 環境變數名稱", headers: "HTTP 標頭", addHeader: "新增標頭",
    importJson: "匯入 JSON", importAction: "匯入", jsonHint: "貼上 MCP 設定。名稱衝突時保留原有設定。", invalidJson: "請輸入有效的 JSON。", invalidFields: "請檢查必填欄位及 JSON 參數陣列。",
    conflict: "存在同名或目標衝突。已保留本次輸入，請更換名稱或目標位置。", failed: "操作失敗，目前資料已保留，請重試。",
    deleteTitle: "刪除此伺服器？", deleteBody: "將移除伺服器設定，此操作無法在此復原。", managedBelow: "由 CodexBox 管理，請使用下方工具卡片。", readonly: "此設定在這裡僅供讀取，請在 Codex 設定中編輯。",
    close: "關閉", project: "專案目錄", projectHint: "選填絕對路徑，僅掃描所選專案的技能目錄。", scan: "掃描", localSource: "本機來源目錄", scanSource: "掃描來源", sourceSkills: "來源中的技能",
    selectSkills: "選擇要匯入的技能", importSelected: "匯入所選", target: "匯入位置", global: "全域", projectTarget: "專案", repository: "公開 Git 儲存庫（HTTPS）", subdirectory: "技能子目錄", importGit: "從 Git 匯入",
    readSkill: "檢視 SKILL.md", skillFile: "SKILL.md 內容", path: "路徑", selectionRequired: "請至少選擇一個來源技能。", projectRequired: "請輸入目標專案目錄。", sourceRequired: "請輸入不含帳號或憑證的公開 HTTPS 儲存庫網址。",
    select: "選擇", warning: "掃描提示", unavailable: "此技能由其他位置管理，無法在這裡切換狀態。", globalScope: "全域", projectScope: "專案", builtInScope: "內建", adminScope: "管理員", localScope: "本機來源",
  },
  ja: {
    searchMcp: "MCP サーバーを検索", searchSkills: "インストール済みスキルを検索", noMatches: "一致する項目はありません。",
    skillViews: "スキル表示", installedTab: "インストール済み", discoverTab: "見つける", searchDiscoveries: "利用可能なスキルを検索", discoveryHint: "公開リポジトリまたはローカルフォルダーを選択してください。スキルのディレクトリを自動検出し、Codex 全体にインストールします。", refreshRepository: "リポジトリを更新", importLocal: "ローカルフォルダーを取り込む", loadRepository: "リポジトリを読み込む", repositorySource: "現在のリポジトリ", installSkill: "インストール",
    jsonExample: "設定例", importPreview: "取り込みプレビュー", newEntry: "新規", existingEntry: "名前が既に存在します",
    previewHint: "名前と接続方式のみを表示します。認証情報は表示しません。",
    importBlocked: "競合する名前を変更してください。既存の名前がある場合、取り込み全体を中止します。",
    savedResult: "設定を保存しました。", mcpImportResult: "MCP サーバーを {count} 件取り込みました。",
    skillImportResult: "成功 {success} 件、失敗 {failed} 件。", failedSelections: "失敗した項目は選択を保持しているため、修正後に再試行できます。",
    mcpActivation: "保存した設定を適用するには、Codex で MCP を再読み込みするか Codex を再起動してください。",
    skillActivation: "スキルの変更を使用するには、新しい Codex セッションを開始してください。",
    invalidSkill: "スキルが不完全です。SKILL.md に有効な name と description のメタデータが必要です。",
    mcpTitle: "MCP 管理", mcpSubtitle: "Codex 設定を自動検出します。ディレクトリを設定せずに接続とカスタムサーバーを管理できます。", skillsTitle: "Skills 管理", skillsSubtitle: "インストール済みの Codex スキルを自動検出します。ディレクトリ設定なしでスキルを探してインストールできます。",
    refresh: "更新", loading: "読み込み中…", busy: "処理中…", emptyMcp: "MCP サーバーはありません。", emptySkills: "スキルが見つかりません。",
    add: "サーバーを追加", edit: "編集", save: "保存", cancel: "キャンセル", enable: "有効にする", disable: "無効にする", enabled: "有効", disabled: "無効", remove: "削除",
    name: "名前", transport: "接続方式", command: "コマンド", args: "引数（JSON 配列）", env: "環境変数の値", variable: "変数名", value: "値", addVariable: "変数を追加",
    envVars: "転送する環境変数名", envVarsHint: "1 行に 1 件。値は実行環境に保持されます。", url: "サーバー URL", bearerEnv: "Bearer Token の環境変数名", headers: "HTTP ヘッダー", addHeader: "ヘッダーを追加",
    importJson: "JSON を取り込む", importAction: "取り込む", jsonHint: "MCP 設定を貼り付けます。名前の競合時は既存の設定を保持します。", invalidJson: "有効な JSON を入力してください。", invalidFields: "必須項目と JSON 引数配列を確認してください。",
    conflict: "名前または保存先が競合しています。入力は保持されています。名前や保存先を変更してください。", failed: "操作に失敗しました。現在のデータは保持されています。再試行してください。",
    deleteTitle: "このサーバーを削除しますか？", deleteBody: "サーバー設定を削除します。この画面では元に戻せません。", managedBelow: "CodexBox が管理しています。下のツールカードを使用してください。", readonly: "ここでは読み取り専用です。Codex の設定で編集してください。",
    close: "閉じる", project: "プロジェクトディレクトリ", projectHint: "任意の絶対パス。選択したプロジェクトのスキルディレクトリを検索します。", scan: "検索", localSource: "ローカルの取り込み元", scanSource: "取り込み元を検索", sourceSkills: "取り込み元のスキル",
    selectSkills: "取り込むスキルを選択", importSelected: "選択したスキルを取り込む", target: "保存先", global: "グローバル", projectTarget: "プロジェクト", repository: "公開 Git リポジトリ（HTTPS）", subdirectory: "スキルのサブディレクトリ", importGit: "Git から取り込む",
    readSkill: "SKILL.md を表示", skillFile: "SKILL.md の内容", path: "パス", selectionRequired: "取り込むスキルを選択してください。", projectRequired: "保存先プロジェクトを入力してください。", sourceRequired: "認証情報を含まない公開 HTTPS リポジトリの URL を入力してください。",
    select: "選択", warning: "検索結果の注意事項", unavailable: "このスキルは別の場所で管理されているため、ここでは切り替えられません。", globalScope: "グローバル", projectScope: "プロジェクト", builtInScope: "組み込み", adminScope: "管理者", localScope: "ローカルの取り込み元",
  },
  ko: {
    searchMcp: "MCP 서버 검색", searchSkills: "설치된 스킬 검색", noMatches: "일치하는 항목이 없습니다.",
    skillViews: "스킬 보기", installedTab: "설치됨", discoverTab: "탐색", searchDiscoveries: "사용 가능한 스킬 검색", discoveryHint: "공개 저장소나 로컬 폴더를 선택하세요. 스킬 디렉터리를 자동으로 찾고 Codex 전역 위치에 설치합니다.", refreshRepository: "저장소 새로 고침", importLocal: "로컬 폴더 가져오기", loadRepository: "저장소 불러오기", repositorySource: "현재 저장소", installSkill: "설치",
    jsonExample: "설정 예시", importPreview: "가져오기 미리보기", newEntry: "새 항목", existingEntry: "이미 존재하는 이름",
    previewHint: "이름과 연결 방식만 표시합니다. 인증 정보는 표시하지 않습니다.",
    importBlocked: "충돌하는 이름을 변경하세요. 기존 이름이 있으면 전체 가져오기를 취소합니다.",
    savedResult: "설정을 저장했습니다.", mcpImportResult: "MCP 서버 {count}개를 가져왔습니다.",
    skillImportResult: "성공 {success}개, 실패 {failed}개.", failedSelections: "실패한 항목은 선택된 상태로 유지되어 수정 후 다시 시도할 수 있습니다.",
    mcpActivation: "저장된 설정을 적용하려면 Codex에서 MCP를 다시 로드하거나 Codex를 재시작하세요.",
    skillActivation: "스킬 변경 사항을 사용하려면 새 Codex 세션을 시작하세요.",
    invalidSkill: "스킬이 불완전합니다. SKILL.md에 유효한 name과 description 메타데이터가 필요합니다.",
    mcpTitle: "MCP 관리", mcpSubtitle: "Codex 설정을 자동으로 감지합니다. 디렉터리 설정 없이 연결과 사용자 지정 서버를 관리하세요.", skillsTitle: "Skills 관리", skillsSubtitle: "설치된 Codex 스킬을 자동으로 찾습니다. 디렉터리 설정 없이 스킬을 탐색하고 설치하세요.",
    refresh: "새로 고침", loading: "불러오는 중…", busy: "처리 중…", emptyMcp: "MCP 서버가 없습니다.", emptySkills: "스킬을 찾지 못했습니다.",
    add: "서버 추가", edit: "편집", save: "저장", cancel: "취소", enable: "활성화", disable: "비활성화", enabled: "활성", disabled: "비활성", remove: "삭제",
    name: "이름", transport: "연결 방식", command: "명령", args: "인수(JSON 배열)", env: "환경 변수 값", variable: "변수 이름", value: "값", addVariable: "변수 추가",
    envVars: "전달할 환경 변수 이름", envVarsHint: "한 줄에 하나씩 입력합니다. 값은 실행 환경에 유지됩니다.", url: "서버 URL", bearerEnv: "Bearer Token 환경 변수 이름", headers: "HTTP 헤더", addHeader: "헤더 추가",
    importJson: "JSON 가져오기", importAction: "가져오기", jsonHint: "MCP 설정을 붙여 넣으세요. 이름이 충돌하면 기존 설정을 유지합니다.", invalidJson: "올바른 JSON을 입력하세요.", invalidFields: "필수 항목과 JSON 인수 배열을 확인하세요.",
    conflict: "이름이나 대상이 충돌합니다. 입력은 유지됩니다. 다른 이름이나 대상을 선택하세요.", failed: "작업이 실패했습니다. 현재 데이터는 유지됩니다. 다시 시도하세요.",
    deleteTitle: "이 서버를 삭제할까요?", deleteBody: "서버 설정을 삭제합니다. 이 화면에서는 되돌릴 수 없습니다.", managedBelow: "CodexBox에서 관리합니다. 아래 도구 카드를 사용하세요.", readonly: "여기서는 읽기 전용입니다. Codex 설정에서 편집하세요.",
    close: "닫기", project: "프로젝트 디렉터리", projectHint: "선택적 절대 경로입니다. 선택한 프로젝트의 스킬 디렉터리만 검색합니다.", scan: "검색", localSource: "로컬 소스 디렉터리", scanSource: "소스 검색", sourceSkills: "소스의 스킬",
    selectSkills: "가져올 스킬 선택", importSelected: "선택 항목 가져오기", target: "가져올 위치", global: "전역", projectTarget: "프로젝트", repository: "공개 Git 저장소(HTTPS)", subdirectory: "스킬 하위 디렉터리", importGit: "Git에서 가져오기",
    readSkill: "SKILL.md 보기", skillFile: "SKILL.md 내용", path: "경로", selectionRequired: "가져올 스킬을 하나 이상 선택하세요.", projectRequired: "대상 프로젝트 디렉터리를 입력하세요.", sourceRequired: "인증 정보가 없는 공개 HTTPS 저장소 URL을 입력하세요.",
    select: "선택", warning: "검색 안내", unavailable: "다른 위치에서 관리하는 스킬이므로 여기서는 상태를 변경할 수 없습니다.", globalScope: "전역", projectScope: "프로젝트", builtInScope: "기본 제공", adminScope: "관리자", localScope: "로컬 소스",
  },
};

export function useExtensionCopy(): ExtensionCopy {
  const { resolvedLocale } = useI18n();
  return extensionCopy[resolvedLocale] ?? extensionCopy.en;
}

export function extensionMessage(template: string, values: Record<string, number>): string {
  return template.replace(/\{(\w+)\}/g, (placeholder, key: string) => key in values ? String(values[key]) : placeholder);
}

export function extensionErrorKey(error: unknown): ExtensionCopyKey {
  const code = typeof error === "string" ? error : error instanceof Error ? error.message : "";
  if (/conflict|already.exists|duplicate|destination.exists|已存在|不[会會]覆[盖蓋]|重复|重複/i.test(code)) return "conflict";
  if (/frontmatter|invalid.skill.metadata|技能.*(?:元数据|元資料|不完整)|技能.*name.*description|SKILL\.md.*(?:name|description)/i.test(code)) return "invalidSkill";
  if (/invalid.*json|json.*parse/i.test(code)) return "invalidJson";
  return "failed";
}

export function useExtensionRequest() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ExtensionCopyKey | null>(null);
  const locked = useRef(false);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const run = useCallback(async <T,>(task: () => Promise<T>, accept: (value: T) => void) => {
    if (locked.current) return false;
    locked.current = true;
    setBusy(true);
    setError(null);
    try {
      const result = await task();
      if (mounted.current) accept(result);
      return true;
    } catch (failure) {
      if (mounted.current) setError(extensionErrorKey(failure));
      return false;
    } finally {
      locked.current = false;
      if (mounted.current) setBusy(false);
    }
  }, []);
  return { busy, error, setError, run };
}

export interface McpServerInput {
  id: string; transport: "stdio" | "http" | "unsupported"; enabled: boolean;
  command: string | null; args: string[]; env: Record<string, string>; envVars: string[];
  url: string | null; bearerTokenEnvVar: string | null; httpHeaders: Record<string, string>;
}
export interface McpServer extends McpServerInput {
  editable: boolean; managedBy: string | null; readonlyReason: string | null;
}
export interface SkillSummary {
  name: string; description: string; skillPath: string; directory: string;
  scope: string; enabled: boolean; canToggle: boolean;
}
export interface SkillScan { skills: SkillSummary[]; warnings: string[] }
export interface SkillCatalog extends SkillScan { configPath: string }
export interface GitSkillSummary { name: string; description: string; subdirectory: string; installDirectory: string }
export interface GitSkillScan { skills: GitSkillSummary[]; warnings: string[] }

export function skillScopeLabel(scope: string, copy: ExtensionCopy): string {
  if (scope.startsWith("global_")) return copy.globalScope;
  if (scope.startsWith("project_")) return copy.projectScope;
  if (scope === "builtin") return copy.builtInScope;
  if (scope === "admin") return copy.adminScope;
  return copy.localScope;
}
