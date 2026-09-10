import type { ResolvedLocale } from "./i18n";

export type ToolCategory =
  | "core"
  | "guardrails"
  | "learning"
  | "workflow"
  | "automation"
  | "code_intelligence"
  | "documents"
  | "efficiency"
  | "other";

type Localized = Record<ResolvedLocale, string>;

export const TOOL_CATEGORY_ORDER: ToolCategory[] = [
 "workflow",
 "automation",
 "guardrails",
 "code_intelligence",
 "efficiency",
 "documents",
 "learning",
 "core",
 "other",
];

export function groupToolsByCategory<T extends { category?: string | null }>(items: T[]): Map<ToolCategory, T[]> {
  const groups = new Map(TOOL_CATEGORY_ORDER.map((category) => [category, [] as T[]]));
  for (const item of items) groups.get(toolCategory(item.category))?.push(item);
  return groups;
}

export const toolCategoryCopy: Record<
  ToolCategory,
  { title: Localized; description: Localized }
> = {
  core: {
    title: { en: "Core", "zh-CN": "核心", "zh-TW": "核心", ja: "コア", ko: "핵심" },
    description: {
      en: "Provides Headroom's local proxy, request routing, and managed runtime; the system foundation for every other group.",
      "zh-CN": "提供 Headroom 本地代理、请求路由和受管运行时，是其他分组工作的系统基础。",
      "zh-TW": "提供 Headroom 本機代理、請求路由與受管執行階段，是其他分組運作的系統基礎。",
      ja: "Headroom のローカルプロキシ、リクエストルーティング、管理ランタイムを提供し、他のすべてのグループの基盤になります。",
      ko: "Headroom 로컬 프록시, 요청 라우팅, 관리형 런타임을 제공하는 다른 모든 그룹의 시스템 기반입니다.",
    },
  },
  guardrails: {
    title: {
      en: "Safety constraints",
      "zh-CN": "安全约束",
      "zh-TW": "安全約束",
      ja: "安全制約",
      ko: "안전 제약",
    },
    description: {
      en: "Independent guardrails can stack and coexist with workflow tools.",
      "zh-CN": "独立安全约束可以叠加，也能与工作流工具共存。",
      "zh-TW": "獨立安全約束可以疊加，也能與工作流程工具共存。",
      ja: "独立したガードレールは併用でき、ワークフローツールとも共存できます。",
      ko: "독립 안전 장치는 함께 사용할 수 있고 워크플로 도구와도 공존합니다.",
    },
  },
  learning: {
    title: { en: "Learning", "zh-CN": "学习理解", "zh-TW": "學習理解", ja: "学習", ko: "학습" },
    description: {
      en: "Tools for understanding code and stress-testing assumptions.",
      "zh-CN": "用于理解代码和检验假设的工具。",
      "zh-TW": "用於理解程式碼和檢驗假設的工具。",
      ja: "コード理解と前提検証のためのツール。",
      ko: "코드 이해와 가정 검증을 위한 도구입니다.",
    },
  },
  workflow: {
    title: {
      en: "Primary workflow",
      "zh-CN": "主工作流",
      "zh-TW": "主要工作流程",
      ja: "主ワークフロー",
      ko: "주 워크플로",
    },
    description: {
      en: "OpenSpec, Superpowers, and gstack are mutually exclusive. Enabling one disables the active peer.",
      "zh-CN": "OpenSpec、Superpowers、gstack 互斥单选；启用一个会关闭同组已启用项。",
      "zh-TW": "OpenSpec、Superpowers、gstack 互斥單選；啟用一個會停用同組已啟用項。",
      ja: "OpenSpec、Superpowers、gstack は排他的です。1つを有効にすると同じグループの有効項目を無効にします。",
      ko: "OpenSpec, Superpowers, gstack은 상호 배타적이며 하나를 켜면 같은 그룹의 활성 항목을 끕니다.",
    },
  },
  automation: {
    title: {
      en: "Automation executor",
      "zh-CN": "自动执行器",
      "zh-TW": "自動執行器",
      ja: "自動実行",
      ko: "자동 실행기",
    },
    description: {
      en: "All in Luna and Ralph Loop are mutually exclusive. Both require an explicit user start.",
      "zh-CN": "All in Luna 与 Ralph Loop 互斥单选，且都必须由用户明确启动。",
      "zh-TW": "All in Luna 與 Ralph Loop 互斥單選，且都必須由使用者明確啟動。",
      ja: "All in Luna と Ralph Loop は排他的で、どちらもユーザーの明示的な開始が必要です。",
      ko: "All in Luna와 Ralph Loop는 상호 배타적이며 모두 사용자가 명시적으로 시작해야 합니다.",
    },
  },
  code_intelligence: {
    title: {
      en: "Code intelligence",
      "zh-CN": "代码智能",
      "zh-TW": "程式碼智慧",
      ja: "コードインテリジェンス",
      ko: "코드 인텔리전스",
    },
    description: {
      en: "Improves lookup and navigation: Serena finds symbols, Codebase Memory reuses project context, and Context7 fetches versioned docs; all can be enabled together.",
      "zh-CN": "增强代码与资料检索：Serena 做符号级导航，Codebase Memory 复用项目上下文，Context7 查询版本化文档；可同时启用。",
      "zh-TW": "增強程式碼與資料檢索：Serena 進行符號級導覽，Codebase Memory 重用專案上下文，Context7 查詢版本化文件；可同時啟用。",
      ja: "検索とナビゲーションを強化します。Serena はシンボルを探し、Codebase Memory はプロジェクト文脈を再利用し、Context7 はバージョン別ドキュメントを取得します。すべて併用できます。",
      ko: "검색과 탐색을 강화합니다. Serena는 심볼을 찾고, Codebase Memory는 프로젝트 컨텍스트를 재사용하며, Context7은 버전별 문서를 조회합니다. 모두 함께 켤 수 있습니다.",
    },
  },
  documents: {
    title: { en: "Documents", "zh-CN": "文档", "zh-TW": "文件", ja: "文書", ko: "문서" },
    description: {
      en: "Converts local PDF and Office files to Markdown for agents to read; it can coexist with every other group.",
      "zh-CN": "把本地 PDF、Office 等文档转换为便于代理读取的 Markdown；可与其他分组同时启用。",
      "zh-TW": "把本機 PDF、Office 等文件轉換為便於代理讀取的 Markdown；可與其他分組同時啟用。",
      ja: "ローカルの PDF や Office 文書を、エージェントが読みやすい Markdown に変換します。他のすべてのグループと併用できます。",
      ko: "로컬 PDF와 Office 문서를 에이전트가 읽기 쉬운 Markdown으로 변환하며 다른 모든 그룹과 함께 켤 수 있습니다.",
    },
  },
  efficiency: {
    title: { en: "Efficiency & Expression", "zh-CN": "效率与表达", "zh-TW": "效率與表達", ja: "効率と表現", ko: "효율과 표현" },
    description: {
      en: "Reduces token use or controls response style: RTK compresses shell output, Ponytail favors minimal code, and Caveman shortens replies; all can be enabled together, but styles may stack.",
      "zh-CN": "减少 Token 使用或控制回答风格：RTK 压缩终端输出，Ponytail 倾向最小代码，Caveman 缩短回答；可同时启用，但风格会叠加。",
      "zh-TW": "減少 Token 使用或控制回答風格：RTK 壓縮終端輸出，Ponytail 傾向最小程式碼，Caveman 縮短回答；可同時啟用，但風格會疊加。",
      ja: "トークン使用量や回答スタイルを調整します。RTK はシェル出力を圧縮し、Ponytail は最小限のコードを優先し、Caveman は回答を短くします。すべて併用できますが、スタイルは重なる場合があります。",
      ko: "토큰 사용량이나 답변 스타일을 조절합니다. RTK는 셸 출력을 압축하고, Ponytail은 최소 코드를 선호하며, Caveman은 답변을 줄입니다. 모두 함께 켤 수 있지만 스타일은 겹칠 수 있습니다.",
    },
  },
  other: {
    title: { en: "Other", "zh-CN": "其他", "zh-TW": "其他", ja: "その他", ko: "기타" },
    description: {
      en: "Uncategorized tools; follow the compatibility and activation rules shown on each card.",
      "zh-CN": "尚未归类的工具；是否能同时启用及何时生效，以各工具卡片说明为准。",
      "zh-TW": "尚未歸類的工具；是否能同時啟用及何時生效，以各工具卡片說明為準。",
      ja: "未分類のツールです。併用可否と反映タイミングは各ツールカードの説明に従います。",
      ko: "아직 분류되지 않은 도구이며 동시 사용 가능 여부와 적용 시점은 각 도구 카드의 안내를 따릅니다.",
    },
  },
};

Object.assign(toolCategoryCopy.guardrails.description, {
  en: "Controls risky or out-of-scope actions: Stop That Shit enforces task boundaries, while Agent Guard checks secrets and dangerous commands; both can be enabled.",
  "zh-CN": "限制越界和危险操作：Stop That Shit 约束任务范围，Agent Guard 检查密钥与高风险命令；两者可同时启用。",
  "zh-TW": "限制越界和危險操作：Stop That Shit 約束任務範圍，Agent Guard 檢查密鑰與高風險命令；兩者可同時啟用。",
  ja: "危険または範囲外の操作を制御します。Stop That Shit はタスク境界を守り、Agent Guard は秘密情報と危険なコマンドを検査します。両方を併用できます。",
  ko: "위험하거나 범위를 벗어난 작업을 제어합니다. Stop That Shit은 작업 경계를 지키고 Agent Guard는 비밀 정보와 위험한 명령을 검사합니다. 둘 다 함께 켤 수 있습니다.",
});
Object.assign(toolCategoryCopy.learning.description, {
  en: "Verifies code understanding: Grill Me quizzes against the current implementation and corrects knowledge gaps; it can coexist with every other group.",
  "zh-CN": "帮助理解代码并校验认知：Grill Me 会依据当前实现逐题提问并纠正理解偏差；可与其他分组同时启用。",
  "zh-TW": "協助理解程式碼並校驗認知：Grill Me 會依據目前實作逐題提問並修正理解偏差；可與其他分組同時啟用。",
  ja: "コード理解を確認します。Grill Me は現在の実装に基づいて一問ずつ出題し、理解のずれを修正します。他のすべてのグループと併用できます。",
  ko: "코드 이해를 확인합니다. Grill Me는 현재 구현을 기준으로 한 문제씩 질문하고 이해의 빈틈을 바로잡습니다. 다른 모든 그룹과 함께 켤 수 있습니다.",
});
Object.assign(toolCategoryCopy.workflow.description, {
  en: "Chooses how a development task moves from requirements to delivery: OpenSpec emphasizes specs and acceptance, Superpowers planning and TDD, and gstack product-to-release coverage; only one can be enabled in this group.",
  "zh-CN": "决定开发任务如何从需求推进到交付：OpenSpec 偏规格与验收，Superpowers 偏计划与 TDD，gstack 偏产品到发布全流程；本组只能启用 1 个。",
  "zh-TW": "決定開發任務如何從需求推進到交付：OpenSpec 偏規格與驗收，Superpowers 偏計畫與 TDD，gstack 偏產品到發布全流程；本組只能啟用 1 個。",
  ja: "開発タスクを要件から納品までどう進めるかを選びます。OpenSpec は仕様と受け入れ、Superpowers は計画と TDD、gstack は製品からリリースまでを重視します。このグループでは 1 つだけ有効化できます。",
  ko: "개발 작업을 요구사항부터 전달까지 어떻게 진행할지 선택합니다. OpenSpec은 명세와 인수 기준, Superpowers는 계획과 TDD, gstack은 제품부터 출시까지의 전체 과정을 중시합니다. 이 그룹에서는 하나만 켤 수 있습니다.",
});
Object.assign(toolCategoryCopy.automation.description, {
  en: "Chooses what continually drives execution: All in Luna coordinates multi-agent goals, while Ralph Loop repeats work until completion conditions; only one can be enabled, and both require an explicit start.",
  "zh-CN": "决定由谁持续推动任务执行：All in Luna 负责多代理协作与持久目标，Ralph Loop 负责循环执行到完成条件；本组只能启用 1 个，且都需用户明确启动。",
  "zh-TW": "決定由誰持續推動任務執行：All in Luna 負責多代理協作與持久目標，Ralph Loop 負責循環執行到完成條件；本組只能啟用 1 個，且都需使用者明確啟動。",
  ja: "何が継続的に実行を進めるかを選びます。All in Luna はマルチエージェントの目標を調整し、Ralph Loop は完了条件まで処理を繰り返します。1 つだけ有効化でき、どちらも明示的な開始が必要です。",
  ko: "무엇이 작업 실행을 계속 이끌지 선택합니다. All in Luna는 멀티 에이전트 목표를 조율하고 Ralph Loop는 완료 조건까지 작업을 반복합니다. 하나만 켤 수 있으며 둘 다 명시적으로 시작해야 합니다.",
});

export type ActivationScope = "immediate" | "new_session" | "client_restart" | "unknown";

export const activationScopeCopyByScope: Record<ActivationScope, Localized> = {
  immediate: {
    en: "Takes effect immediately; no Headroom restart is needed.",
    "zh-CN": "即时生效；无需重启 Headroom。",
    "zh-TW": "即時生效；無需重新啟動 Headroom。",
    ja: "すぐに反映されます。Headroom の再起動は不要です。",
    ko: "즉시 적용되며 Headroom을 다시 시작할 필요가 없습니다.",
  },
  new_session: {
    en: "Takes effect in a new Codex session; no Headroom restart is needed.",
    "zh-CN": "新 Codex 会话生效；无需重启 Headroom。",
    "zh-TW": "新的 Codex 工作階段生效；無需重新啟動 Headroom。",
    ja: "新しい Codex セッションで反映されます。Headroom の再起動は不要です。",
    ko: "새 Codex 세션에서 적용되며 Headroom을 다시 시작할 필요가 없습니다.",
  },
  client_restart: {
    en: "Configuration is installed; restart Codex or the connected client to replace MCPs in open sessions. Headroom does not need a restart.",
    "zh-CN": "配置已安装；需重启 Codex 或对应客户端，才能替换已打开会话中的 MCP。无需重启 Headroom。",
    "zh-TW": "設定已安裝；需重新啟動 Codex 或對應用戶端，才能替換已開啟工作階段中的 MCP。無需重新啟動 Headroom。",
    ja: "設定はインストール済みです。開いているセッションの MCP を置き換えるには Codex または接続先クライアントを再起動してください。Headroom の再起動は不要です。",
    ko: "구성은 설치되었습니다. 열린 세션의 MCP를 교체하려면 Codex 또는 연결된 클라이언트를 다시 시작해야 합니다. Headroom 재시작은 필요 없습니다.",
  },
  unknown: {
    en: "Activation timing is tool-specific; check the tool details.",
    "zh-CN": "生效时间取决于具体工具，请查看工具详情。",
    "zh-TW": "生效時間取決於具體工具，請查看工具詳情。",
    ja: "反映時期はツールごとに異なります。詳細を確認してください。",
    ko: "적용 시점은 도구마다 다르므로 도구 세부 정보를 확인하세요.",
  },
};

export function getActivationScopeCopy(scope?: string | null): Localized {
  if (scope === "immediate" || scope === "new_session" || scope === "client_restart") {
    return activationScopeCopyByScope[scope];
  }
  return activationScopeCopyByScope.unknown;
}

export const workflowGroupCopy: Record<string, Localized> = {
  primary_workflow: {
    en: "Only one can be enabled · Primary workflow",
    "zh-CN": "本组只能启用 1 个 · 主工作流",
    "zh-TW": "本組只能啟用 1 個 · 主要工作流程",
    ja: "このグループでは 1 つだけ有効 · 主ワークフロー",
    ko: "이 그룹에서는 하나만 활성화 · 주 워크플로",
  },
  execution_engine: {
    en: "Only one can be enabled · Automation executor",
    "zh-CN": "本组只能启用 1 个 · 自动执行器",
    "zh-TW": "本組只能啟用 1 個 · 自動執行器",
    ja: "このグループでは 1 つだけ有効 · 自動実行",
    ko: "이 그룹에서는 하나만 활성화 · 자동 실행기",
  },
};

export const conflictHeadingCopy: Localized = {
  en: "Conflict and switching rules",
  "zh-CN": "冲突与切换规则",
  "zh-TW": "衝突與切換規則",
  ja: "競合と切り替えルール",
  ko: "충돌 및 전환 규칙",
};

export const sourceLinkCopy: Localized = {
  en: "Source and acknowledgements",
  "zh-CN": "源码与致谢",
  "zh-TW": "原始碼與致謝",
  ja: "ソースと謝辞",
  ko: "소스 및 감사",
};

export const toolCopy: Record<string, Localized> = {
  openspec: {
    en: "Best for specification-driven, cross-module changes with explicit requirements and acceptance evidence. Includes the OpenSpec CLI. Thanks to Fission AI and contributors.",
    "zh-CN": "适合需求、规格和验收证据清楚的跨模块改动；同时安装 OpenSpec CLI。感谢 Fission AI 与贡献者。",
    "zh-TW": "適合需求、規格和驗收證據清楚的跨模組改動；同時安裝 OpenSpec CLI。感謝 Fission AI 與貢獻者。",
    ja: "明確な要件・仕様・受け入れ証拠を伴う複数モジュール変更向け。OpenSpec CLI も導入します。Fission AI と貢献者に感謝します。",
    ko: "명확한 요구사항, 사양, 수용 증거가 있는 모듈 간 변경에 적합하며 OpenSpec CLI도 설치합니다. Fission AI와 기여자에게 감사합니다.",
  },
  superpowers: {
    en: "Uses Codex's official curated plugin. Best for disciplined brainstorming, planning, TDD, debugging, review, and delivery. Thanks to Jesse Vincent and contributors.",
    "zh-CN": "复用 Codex 官方 curated 插件，避免重复 marketplace。适合强调 brainstorm、计划、TDD、系统调试、审查与交付纪律的开发。感谢 Jesse Vincent 与贡献者。",
    "zh-TW": "重用 Codex 官方 curated 外掛，避免重複 marketplace。適合強調 brainstorm、規劃、TDD、系統除錯、審查與交付紀律的開發。感謝 Jesse Vincent 與貢獻者。",
    ja: "Codex 公式 curated プラグインを再利用し、marketplace の重複を避けます。計画、TDD、体系的デバッグ、レビュー、納品を重視する開発向け。Jesse Vincent と貢献者に感謝します。",
    ko: "Codex 공식 curated 플러그인을 재사용해 marketplace 중복을 피합니다. 계획, TDD, 체계적 디버깅, 리뷰, 전달 규율을 중시하는 개발에 적합합니다. Jesse Vincent와 기여자에게 감사합니다.",
  },
  gstack: {
    en: "Best for a broad product-to-ship workflow with browser, design, QA, review, and release skills. Requires Bun and Node.js. Thanks to Garry Tan and contributors.",
    "zh-CN": "适合覆盖产品、设计、浏览器验证、QA、审查到发布的完整流程；需要 Bun 与 Node.js。感谢 Garry Tan 与贡献者。",
    "zh-TW": "適合涵蓋產品、設計、瀏覽器驗證、QA、審查到發布的完整流程；需要 Bun 與 Node.js。感謝 Garry Tan 與貢獻者。",
    ja: "プロダクト、デザイン、ブラウザ検証、QA、レビュー、リリースまでの広い工程向け。Bun と Node.js が必要です。Garry Tan と貢献者に感謝します。",
    ko: "제품, 디자인, 브라우저 검증, QA, 리뷰, 릴리스까지 폭넓은 흐름에 적합하며 Bun과 Node.js가 필요합니다. Garry Tan과 기여자에게 감사합니다.",
  },
  "ralph-loop": {
    en: "Best for bounded unattended loops with explicit completion signals. It never starts automatically. Review Ralph's .ralph/.env before running because upstream defaults may bypass agent approvals. Thanks to Santander AI and contributors.",
    "zh-CN": "适合有明确完成信号的有界无人值守循环；不会自动启动。运行前请检查 Ralph 的 .ralph/.env，因为上游默认配置可能绕过代理审批。感谢 Santander AI 与贡献者。",
    "zh-TW": "適合有明確完成訊號的有界無人值守循環；不會自動啟動。執行前請檢查 Ralph 的 .ralph/.env，因為上游預設設定可能略過代理審批。感謝 Santander AI 與貢獻者。",
    ja: "明確な完了シグナルを持つ制限付き無人ループ向け。自動起動はしません。上流の既定値が承認を回避する場合があるため、実行前に .ralph/.env を確認してください。Santander AI と貢献者に感謝します。",
    ko: "명확한 완료 신호가 있는 제한된 무인 루프에 적합하며 자동 시작되지 않습니다. 업스트림 기본값이 승인 절차를 우회할 수 있으므로 실행 전에 .ralph/.env를 확인하세요. Santander AI와 기여자에게 감사합니다.",
  },
  "stop-that-shit": {
    en: "Checks explicit scope and execution boundaries. Review and trust its guard in /hooks before automatic hook execution. Thanks to @lennney and contributors.",
    "zh-CN": "按明确范围和执行边界进行检查；自动执行 Hook 前，请先在 /hooks 中审查并信任防护。感谢 @lennney 与贡献者。",
    "zh-TW": "依明確範圍和執行邊界進行檢查；自動執行 Hook 前，請先在 /hooks 中審查並信任防護。感謝 @lennney 與貢獻者。",
    ja: "明示した範囲と実行境界を検査します。Hook の自動実行前に /hooks で確認して信頼してください。@lennney と貢献者に感謝します。",
    ko: "명시한 범위와 실행 경계를 검사합니다. Hook 자동 실행 전에 /hooks에서 검토하고 신뢰하세요. @lennney와 기여자에게 감사합니다.",
  },
  "agent-guard": {
    en: "Runs local secret and safety checks. Run $setup-agent-guard, then review and trust it in /hooks. Thanks to @JeongJaeSoon and contributors.",
    "zh-CN": "执行本地密钥与安全检查。先运行 $setup-agent-guard，再通过 /hooks 审查并信任。感谢 @JeongJaeSoon 与贡献者。",
    "zh-TW": "執行本機密鑰與安全檢查。先執行 $setup-agent-guard，再透過 /hooks 審查並信任。感謝 @JeongJaeSoon 與貢獻者。",
    ja: "ローカルの秘密情報と安全性を検査します。$setup-agent-guard を実行し、/hooks で確認して信頼してください。@JeongJaeSoon と貢献者に感謝します。",
    ko: "로컬 비밀정보와 안전을 검사합니다. $setup-agent-guard를 실행한 뒤 /hooks에서 검토하고 신뢰하세요. @JeongJaeSoon과 기여자에게 감사합니다.",
  },
  "grill-me": {
    en: "Read-only Q&A that checks whether the developer understands maintained code. Trigger with $grill-me or /grill-me; it does not auto-run. Thanks to Joshua Wheelock.",
    "zh-CN": "通过只读问答检查开发者是否理解正在维护的代码。显式使用 $grill-me 或 /grill-me；默认不会自动触发。感谢 Joshua Wheelock。",
    "zh-TW": "透過唯讀問答檢查開發者是否理解正在維護的程式碼。請明確使用 $grill-me 或 /grill-me；預設不會自動觸發。感謝 Joshua Wheelock。",
    ja: "保守中のコード理解を読み取り専用 Q&A で確認します。$grill-me または /grill-me で明示的に起動し、自動実行はしません。Joshua Wheelock に感謝します。",
    ko: "유지 관리 중인 코드 이해도를 읽기 전용 Q&A로 확인합니다. $grill-me 또는 /grill-me로 명시적으로 실행하며 자동 실행되지 않습니다. Joshua Wheelock에게 감사합니다.",
  },
};

toolCopy.superpowers["zh-CN"] = "复用 Codex 官方精选插件，避免重复安装市场版本。适合强调头脑风暴、计划、TDD、系统调试、审查与交付纪律的开发。感谢 Jesse Vincent 与贡献者。";

export const conflictMatrixCopy: Localized = {
  en: "Safety constraints can stack; Grill Me can coexist with every workflow. OpenSpec, Superpowers, and gstack are single-select. All in Luna and Ralph Loop are single-select. Switching is explicit and affects new sessions only; Headroom and port 6867 stay running.",
  "zh-CN": "安全约束可叠加，Grill Me 可与所有工作流共存。OpenSpec、Superpowers、gstack 主工作流单选；All in Luna、Ralph Loop 自动执行器单选。切换必须由用户明确触发，只影响新会话；Headroom 与 6867 端口保持运行。",
  "zh-TW": "安全約束可疊加，Grill Me 可與所有工作流程共存。OpenSpec、Superpowers、gstack 主要工作流程單選；All in Luna、Ralph Loop 自動執行器單選。切換必須由使用者明確觸發，只影響新工作階段；Headroom 與 6867 連接埠保持運行。",
  ja: "安全制約は併用でき、Grill Me はすべてのワークフローと共存できます。OpenSpec、Superpowers、gstack は単一選択、All in Luna と Ralph Loop も単一選択です。切り替えは明示操作で新しいセッションだけに反映され、Headroom と 6867 番ポートは動作を続けます。",
  ko: "안전 제약은 함께 사용할 수 있고 Grill Me는 모든 워크플로와 공존합니다. OpenSpec, Superpowers, gstack은 단일 선택이며 All in Luna와 Ralph Loop도 단일 선택입니다. 전환은 명시적으로 실행되어 새 세션에만 적용되고 Headroom과 6867 포트는 계속 실행됩니다.",
};

export function toolCategory(category?: string | null): ToolCategory {
  return TOOL_CATEGORY_ORDER.includes(category as ToolCategory)
    ? (category as ToolCategory)
    : "other";
}

/** Minimal shape the switch rule needs: every tool list in the UI satisfies it. */
export interface SwitchableTool {
  id: string;
  name: string;
  enabled: boolean;
  status: string;
  workflowGroup?: string | null;
}

/**
 * Peers that enabling `id` would switch off.
 *
 * The backend enforces the single-select rule by silently disabling the active
 * peer (see `enforce_exclusive_plugin_group`), so the UI asks first instead of
 * letting a workflow disappear without explanation. Returns an empty list for
 * tools outside a group, so callers can use the result as the gate itself.
 */
export function workflowSwitchPeers<T extends SwitchableTool>(
  tools: readonly T[],
  id: string
): T[] {
  const group = tools.find((tool) => tool.id === id)?.workflowGroup;
  if (!group) return [];
  return tools.filter(
    (tool) =>
      tool.id !== id &&
      tool.workflowGroup === group &&
      tool.enabled &&
      tool.status !== "not_installed"
  );
}
