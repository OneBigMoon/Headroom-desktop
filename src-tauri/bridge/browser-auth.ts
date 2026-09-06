// Headroom browser authorization v2. OAuth consent is always an explicit user action.
import { Router, json, type Request, type Response } from "express";
import { randomBytes, createHash } from "node:crypto";
import type { OAuthDeps } from "./oauth.js";
import type { PairingVerifyResult } from "../pairing/manager.js";
import { escapeHtml, setAuthSecurityHeaders } from "./html.js";

const COOKIE = "headroom_browser_auth";
const TTL = 15 * 60_000;
const CONNECTOR_URL = "https://chatgpt.com/plugins#settings/Connectors?create-connector=true&redirectAfter=%2Fplugins";
const COPY = {
  en: { title: "Connect ChatGPT", preparing: "Verifying this computer…", ready: "This computer is verified", setup: "First connection: copy the address below, create a ChatGPT connector with OAuth, then choose Allow on the authorization page.", existing: "Already added this connector? Open ChatGPT and reconnect it.", next: "Copy address and open ChatGPT", open: "Open ChatGPT", failed: "This connection link has expired. Return to Headroom and start the connection again.", copyFailed: "Copy the address below, then open ChatGPT.", consent: "Allow ChatGPT to read this workspace?", workspace: "Workspace", client: "Return address", read: "Read files", search: "Search workspace", git: "Read Git status and diffs", execution: "Read execution output", scope: "Read-only access. This connection cannot edit files or run commands.", allow: "Allow", deny: "Cancel", hint: "Only allow access if you started this connection in Headroom.", expired: "Computer verification expired. Return to Headroom and reconnect." },
  "zh-CN": { title: "连接 ChatGPT", preparing: "正在验证本机身份…", ready: "本机身份已验证", setup: "首次连接：复制下方地址，在 ChatGPT 中创建连接器并选择 OAuth，然后在授权页点击“允许”。", existing: "已经添加过这个连接器？打开 ChatGPT，重新连接即可。", next: "复制地址并前往 ChatGPT", open: "打开 ChatGPT", failed: "连接链接已失效，请回到 Headroom 重新点击连接。", copyFailed: "请复制下方地址，然后打开 ChatGPT。", consent: "允许 ChatGPT 读取这个工作区吗？", workspace: "工作区", client: "授权后返回", read: "读取文件", search: "搜索工作区", git: "读取 Git 状态与差异", execution: "读取执行输出", scope: "仅限只读访问，不能修改文件或执行命令。", allow: "允许", deny: "取消", hint: "请确认这是你刚刚在 Headroom 中发起的连接。", expired: "本机验证已过期，请回到 Headroom 重新连接。" },
  "zh-TW": { title: "連線 ChatGPT", preparing: "正在驗證本機身分…", ready: "本機身分已驗證", setup: "首次連線：複製下方位址，在 ChatGPT 建立連接器並選擇 OAuth，再於授權頁按「允許」。", existing: "已加入此連接器？開啟 ChatGPT 並重新連線即可。", next: "複製位址並前往 ChatGPT", open: "開啟 ChatGPT", failed: "連線連結已失效，請回到 Headroom 重新連線。", copyFailed: "請複製下方位址，再開啟 ChatGPT。", consent: "允許 ChatGPT 讀取此工作區嗎？", workspace: "工作區", client: "授權後返回", read: "讀取檔案", search: "搜尋工作區", git: "讀取 Git 狀態與差異", execution: "讀取執行輸出", scope: "僅限唯讀存取，無法修改檔案或執行命令。", allow: "允許", deny: "取消", hint: "請確認這是你剛才在 Headroom 發起的連線。", expired: "本機驗證已過期，請回到 Headroom 重新連線。" },
  ja: { title: "ChatGPT に接続", preparing: "このコンピューターを確認中…", ready: "コンピューターの確認が完了しました", setup: "初回は下のアドレスをコピーして ChatGPT で OAuth コネクターを作成し、認証画面で「許可」を選択してください。", existing: "追加済みの場合は ChatGPT で再接続してください。", next: "アドレスをコピーして ChatGPT を開く", open: "ChatGPT を開く", failed: "リンクの期限が切れました。Headroom から再接続してください。", copyFailed: "下のアドレスをコピーして ChatGPT を開いてください。", consent: "ChatGPT にこのワークスペースの読み取りを許可しますか？", workspace: "ワークスペース", client: "認証後の戻り先", read: "ファイルの読み取り", search: "ワークスペースの検索", git: "Git の状態と差分の読み取り", execution: "実行結果の読み取り", scope: "読み取り専用です。ファイルの編集やコマンドの実行はできません。", allow: "許可", deny: "キャンセル", hint: "Headroom で開始した接続であることを確認してください。", expired: "確認の期限が切れました。Headroom から再接続してください。" },
  ko: { title: "ChatGPT 연결", preparing: "이 컴퓨터를 확인하는 중…", ready: "컴퓨터가 확인되었습니다", setup: "처음 연결할 때 아래 주소를 복사해 ChatGPT에서 OAuth 커넥터를 만들고 인증 화면에서 허용을 선택하세요.", existing: "이미 추가했다면 ChatGPT에서 다시 연결하세요.", next: "주소 복사 후 ChatGPT 열기", open: "ChatGPT 열기", failed: "연결 링크가 만료되었습니다. Headroom에서 다시 연결하세요.", copyFailed: "아래 주소를 복사한 다음 ChatGPT를 여세요.", consent: "ChatGPT가 이 작업공간을 읽도록 허용할까요?", workspace: "작업공간", client: "인증 후 돌아갈 주소", read: "파일 읽기", search: "작업공간 검색", git: "Git 상태 및 변경 사항 읽기", execution: "실행 결과 읽기", scope: "읽기 전용입니다. 파일 수정이나 명령 실행은 허용되지 않습니다.", allow: "허용", deny: "취소", hint: "Headroom에서 시작한 연결인지 확인하세요.", expired: "컴퓨터 확인이 만료되었습니다. Headroom에서 다시 연결하세요." },
};
type Locale = keyof typeof COPY;

function locale(req: Request): Locale {
  const requested = typeof req.query.lang === "string" ? req.query.lang : req.acceptsLanguages()[0] ?? "en";
  if (/^zh-(TW|HK|Hant)/i.test(requested)) return "zh-TW";
  if (/^zh/i.test(requested)) return "zh-CN";
  if (/^ja/i.test(requested)) return "ja";
  if (/^ko/i.test(requested)) return "ko";
  return "en";
}

function page(lang: Locale, body: string, script = "", nonce = ""): string {
  return `<!doctype html><html lang="${lang}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${COPY[lang].title} · Headroom</title><style>
  :root{color-scheme:light dark;font-family:system-ui,sans-serif}body{margin:0;min-height:100vh;display:grid;place-items:center;background:light-dark(#f4f6f8,#151719);color:light-dark(#17202a,#eee)}main{width:min(480px,calc(100% - 72px));margin:24px;padding:32px;border-radius:20px;background:light-dark(white,#23272c);box-shadow:0 8px 40px #0001}h1{font-size:23px;line-height:1.4}p,li{line-height:1.65}small{color:light-dark(#52616b,#b5c0c8)}input{box-sizing:border-box;width:100%;padding:12px;border:1px solid #8886;border-radius:9px;background:transparent;color:inherit}button,.button{box-sizing:border-box;display:block;width:100%;margin-top:16px;padding:13px;border:0;border-radius:10px;font:inherit;text-align:center;text-decoration:none;background:#146bdd;color:white;cursor:pointer}.secondary{background:transparent;color:inherit;border:1px solid #8886}a{color:light-dark(#146bdd,#82b9ff)}.identity{overflow-wrap:anywhere;padding:14px;background:light-dark(#f4f6f8,#151719);border-radius:10px}.brand{font-weight:600;color:light-dark(#52616b,#b5c0c8)}[hidden]{display:none!important}
  </style></head><body><main><div class="brand">Headroom · ChatGPT</div>${body}</main>${script ? `<script nonce="${nonce}">${script}</script>` : ""}</body></html>`;
}

export class BrowserAuth {
  readonly router = Router();
  private sessions = new Map<string, { sessionId: string; expiresAt: number; lang: Locale }>();

  constructor(private deps: OAuthDeps) {
    this.router.get("/headroom/connect", (req, res) => {
      const lang = locale(req), copy = COPY[lang], nonce = randomBytes(16).toString("hex");
      setAuthSecurityHeaders(res);
      res.setHeader("X-Headroom-Browser-Auth", "2");
      res.setHeader("Content-Security-Policy", `default-src 'none'; script-src 'nonce-${nonce}'; connect-src 'self'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'`);
      res.type("html").send(page(lang, `<h1>${copy.title}</h1><p id="status" role="status">${copy.preparing}</p><section id="ready" hidden><p class="identity" id="workspace"></p><p>${copy.setup}</p><label for="endpoint">MCP URL</label><input id="endpoint" readonly spellcheck="false"><button id="continue" type="button">${copy.next}</button><p>${copy.existing}</p><a href="https://chatgpt.com/" rel="noreferrer">${copy.open}</a></section>`, `
      const params = new URLSearchParams(location.hash.slice(1));
      const pairingCode = params.get('pairing_code');
      history.replaceState(null, '', location.pathname + location.search);
      const status = document.getElementById('status');
      async function prepare() {
        try {
          const response = await fetch('/headroom/browser-session?lang=${lang}', {method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify({pairing_code:pairingCode})});
          if (!response.ok) throw new Error('expired');
          const result = await response.json();
          status.textContent = ${JSON.stringify(copy.ready)};
          document.getElementById('workspace').textContent = ${JSON.stringify(copy.workspace + "：")} + result.workspaceName;
          document.getElementById('endpoint').value = result.mcpUrl;
          document.getElementById('ready').hidden = false;
          document.getElementById('continue').onclick = async () => {
            try { await navigator.clipboard.writeText(result.mcpUrl); }
            catch { status.textContent = ${JSON.stringify(copy.copyFailed)}; document.getElementById('endpoint').select(); return; }
            location.assign(${JSON.stringify(CONNECTOR_URL)});
          };
        } catch { status.textContent = ${JSON.stringify(copy.failed)}; }
      }
      void prepare();`, nonce));
    });

    this.router.post("/headroom/browser-session", json({ limit: "1kb" }), (req, res) => {
      setAuthSecurityHeaders(res);
      if (!this.sameOrigin(req)) { res.status(403).json({ error: "invalid_origin" }); return; }
      this.prune();
      // A reload may reuse the already verified browser without redeeming its one-time code again.
      const existing = this.sessionFor(req);
      if (existing) { this.ready(req, res); return; }
      const code = req.body?.pairing_code;
      if (typeof code !== "string" || code.length > 32) { res.status(400).json({ error: "invalid_pairing" }); return; }
      const verdict = deps.pairing.verify(code, req.ip);
      if (!verdict.ok) { res.status(401).json({ error: "invalid_pairing" }); return; }
      const token = randomBytes(32).toString("base64url");
      this.sessions.clear();
      this.sessions.set(this.hash(token), { sessionId: verdict.sessionId, expiresAt: Date.now() + TTL, lang: locale(req) });
      res.cookie(COOKIE, token, { httpOnly: true, secure: new URL(deps.getBaseUrl(req)).protocol === "https:", sameSite: "lax", path: "/", maxAge: TTL });
      this.ready(req, res);
    });
  }

  private hash(token: string): string { return createHash("sha256").update(token).digest("hex"); }

  private prune(): void {
    for (const [key, value] of this.sessions) if (value.expiresAt <= Date.now()) this.sessions.delete(key);
  }

  private sameOrigin(req: Request): boolean {
    return req.get("origin") === new URL(this.deps.getBaseUrl(req)).origin;
  }

  private ready(req: Request, res: Response): void {
    res.json({ ready: true, workspaceName: this.deps.workspaceName, mcpUrl: `${this.deps.getBaseUrl(req).replace(/\/$/, "")}/mcp` });
  }

  sessionFor(req: Request, redirectUri?: string): { key: string; lang: Locale } | undefined {
    if (redirectUri) {
      const target = new URL(redirectUri);
      if (target.origin !== "https://chatgpt.com" || target.username || target.password || !(target.pathname === "/connector_platform_oauth_redirect" || target.pathname.startsWith("/connector/oauth/"))) return;
    }
    this.prune();
    const token = req.headers.cookie?.split(";").map(part => part.trim()).find(part => part.startsWith(`${COOKIE}=`))?.slice(COOKIE.length + 1);
    if (!token || !/^[A-Za-z0-9_-]{43}$/.test(token)) return;
    const key = this.hash(token), session = this.sessions.get(key);
    return session ? { key, lang: session.lang } : undefined;
  }

  consentPage(request: { id: string; scopes: string[]; redirectUri: string }, lang: Locale): string {
    const copy = COPY[lang];
    const offline = { en: "Keep this connection until you revoke access", "zh-CN": "保持连接，直到你撤销访问权限", "zh-TW": "保持連線，直到你撤銷存取權限", ja: "アクセスを取り消すまで接続を維持", ko: "접근을 취소할 때까지 연결 유지" };
    const labels: Record<string, string> = { "workspace.read": copy.read, "workspace.search": copy.search, "git.read": copy.git, "execution.read": copy.execution, offline_access: offline[lang] };
    return page(lang, `<h1>${copy.consent}</h1><p class="identity">${copy.workspace}：<strong>${escapeHtml(this.deps.workspaceName)}</strong><br><small>${copy.client}：${escapeHtml(new URL(request.redirectUri).origin)}</small></p><ul>${request.scopes.map(scope => `<li>${escapeHtml(labels[scope] ?? scope)}</li>`).join("")}</ul><p>${copy.scope}</p><form method="POST" action="/oauth/authorize"><input type="hidden" name="request_id" value="${escapeHtml(request.id)}"><button name="decision" value="allow" type="submit">${copy.allow}</button><button name="decision" value="deny" type="submit" class="secondary">${copy.deny}</button></form><p><small>${copy.hint}</small></p>`);
  }

  approve(req: Request, expectedSession: string): PairingVerifyResult {
    const session = this.sessionFor(req);
    if (!this.sameOrigin(req) || !session || session.key !== expectedSession || !["allow", "deny"].includes(req.body?.decision)) return { ok: false, reason: "invalid" };
    const record = this.sessions.get(session.key)!;
    this.sessions.delete(session.key);
    return { ok: true, sessionId: record.sessionId };
  }

  expiredPage(req: Request, request: { redirectUri: string; state?: string }): string {
    const lang = locale(req);
    const target = new URL(request.redirectUri);
    target.searchParams.set("error", "access_denied");
    target.searchParams.set("error_description", "Local computer verification expired. Reconnect from Headroom.");
    if (request.state) target.searchParams.set("state", request.state);
    return page(lang, `<h1>${COPY[lang].title}</h1><p role="alert">${COPY[lang].expired}</p><a class="button" href="${escapeHtml(target.toString())}" rel="noreferrer">${COPY[lang].open}</a>`);
  }
}
