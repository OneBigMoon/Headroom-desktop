import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import path from "node:path";
import { startBridge, type Bridge } from "../src/bridge/server.js";
import { makeTmpDir, cleanup, write, isolateStateDir, pkceVerifierAndChallenge } from "./helpers.js";

const REDIRECT = "https://chatgpt.com/connector_platform_oauth_redirect";
let bridge: Bridge, base: string, root: string;

beforeEach(async () => {
  isolateStateDir();
  root = makeTmpDir("browser-auth");
  write(root, "hello.txt", "browser authorization verified");
  bridge = await startBridge({ workspaceRoot: root, port: 0, persistRuntime: false, authStoreFile: path.join(root, "auth.json") });
  base = bridge.localBaseUrl();
});
afterEach(async () => { vi.useRealTimers(); await bridge.close(); cleanup(root); });

async function prepare() {
  const pairing = bridge.pairing.create();
  const response = await fetch(base + "/headroom/browser-session?lang=zh-CN", {
    method: "POST", headers: { "Content-Type": "application/json", Origin: base },
    body: JSON.stringify({ pairing_code: pairing.code }),
  });
  expect(response.status).toBe(200);
  const header = response.headers.get("set-cookie")!;
  expect(header).toContain("HttpOnly");
  expect(header).toContain("SameSite=Lax");
  expect(header).toContain("Max-Age=900");
  return { cookie: header.split(";")[0], pairing };
}

async function begin(cookie: string, redirect = REDIRECT) {
  const registration = await fetch(base + "/oauth/register", {
    method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ client_name: "ChatGPT", redirect_uris: [redirect] }),
  });
  const client = await registration.json() as { client_id: string };
  const pkce = pkceVerifierAndChallenge();
  const url = new URL(base + "/oauth/authorize");
  url.search = new URLSearchParams({
    client_id: client.client_id, redirect_uri: redirect, response_type: "code",
    state: "browser-test-state", code_challenge: pkce.challenge, code_challenge_method: "S256",
    scope: "workspace.read workspace.search git.read execution.read offline_access",
  }).toString();
  const response = await fetch(url, { headers: { Cookie: cookie }, redirect: "manual" });
  expect(response.status).toBe(200);
  expect(response.headers.get("location")).toBeNull();
  const html = await response.text();
  const id = html.match(/name="request_id" value="([a-f0-9]+)"/)?.[1];
  expect(id).toBeTruthy();
  return { id: id!, html, clientId: client.client_id, verifier: pkce.verifier };
}

async function decide(id: string, cookie: string, decision = "allow", origin = base) {
  return fetch(base + "/oauth/authorize", {
    method: "POST", redirect: "manual",
    headers: { "Content-Type": "application/x-www-form-urlencoded", Cookie: cookie, Origin: origin, "Accept-Language": "zh-CN" },
    body: new URLSearchParams({ request_id: id, decision }),
  });
}

async function exchange(location: string, clientId: string, verifier: string) {
  return fetch(base + "/oauth/token", {
    method: "POST", headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ grant_type: "authorization_code", code: new URL(location).searchParams.get("code")!, client_id: clientId, redirect_uri: REDIRECT, code_verifier: verifier }),
  });
}

describe("Headroom browser authorization", () => {
  it("serves a localized bootstrap without putting a credential in its HTML or query string", async () => {
    const response = await fetch(base + "/headroom/connect?lang=zh-CN");
    expect(response.headers.get("x-headroom-browser-auth")).toBe("2");
    expect(response.headers.get("content-security-policy")).toContain("script-src 'nonce-");
    expect(response.headers.get("referrer-policy")).toBe("no-referrer");
    const html = await response.text();
    expect(html).toContain("正在验证本机身份");
    expect(html).toContain("location.hash");
    expect(html).toContain("history.replaceState");
    expect(html).not.toContain('name="pairing_code"');
  });

  it("requires same-origin local proof and consumes the one-time credential", async () => {
    const pairing = bridge.pairing.create();
    const response = await fetch(base + "/headroom/browser-session", { method: "POST", headers: { "Content-Type": "application/json", Origin: "https://attacker.invalid" }, body: JSON.stringify({ pairing_code: pairing.code }) });
    expect(response.status).toBe(403);
    const { pairing: used } = await prepare();
    const replay = await fetch(base + "/headroom/browser-session", { method: "POST", headers: { "Content-Type": "application/json", Origin: base }, body: JSON.stringify({ pairing_code: used.code }) });
    expect(replay.status).toBe(401);
  });

  it("verifies the browser but still requires consent, then completes PKCE and an authenticated MCP call", async () => {
    const { cookie } = await prepare();
    const request = await begin(cookie);
    expect(request.html).toContain("允许 ChatGPT 读取这个工作区吗");
    expect(request.html).toContain("仅限只读访问");
    expect(request.html).toContain("保持连接");
    expect(request.html).not.toContain('name="pairing_code"');
    const unauthorized = await fetch(base + "/mcp", { method: "POST", headers: { "Content-Type": "application/json", Accept: "application/json, text/event-stream" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" }) });
    expect(unauthorized.status).toBe(401);
    const approval = await decide(request.id, cookie);
    expect(approval.status).toBe(302);
    const location = approval.headers.get("location")!;
    expect(new URL(location).searchParams.get("state")).toBe("browser-test-state");
    const tokens = await exchange(location, request.clientId, request.verifier);
    expect(tokens.status).toBe(200);
    const data = await tokens.json() as { access_token: string };
    const result = await fetch(base + "/mcp", { method: "POST", headers: { "Content-Type": "application/json", Accept: "application/json, text/event-stream", Authorization: "Bearer " + data.access_token }, body: JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list" }) });
    expect(result.status).toBe(200);
    expect(await result.text()).toContain("tools");
    expect((await decide(request.id, cookie)).status).toBe(400);
    expect((await exchange(location, request.clientId, request.verifier)).status).toBe(400);
  });

  it("does not approve on page load or without an explicit allow/deny decision", async () => {
    const { cookie } = await prepare();
    const request = await begin(cookie);
    expect((await decide(request.id, cookie, "")).status).toBe(401);
    expect((await decide(request.id, cookie)).status).toBe(302);
  });

  it("cancels without issuing an authorization code", async () => {
    const { cookie } = await prepare();
    const request = await begin(cookie);
    const response = await decide(request.id, cookie, "deny");
    expect(response.status).toBe(302);
    const location = new URL(response.headers.get("location")!);
    expect(location.searchParams.get("error")).toBe("access_denied");
    expect(location.searchParams.has("code")).toBe(false);
    expect((await decide(request.id, cookie)).status).toBe(400);
  });

  it("rejects cross-origin consent and a substituted browser session", async () => {
    const first = await prepare();
    const request = await begin(first.cookie);
    expect((await decide(request.id, first.cookie, "allow", "https://attacker.invalid")).status).toBe(401);
    const second = await prepare();
    expect((await decide(request.id, second.cookie)).status).toBe(401);
  });

  it("does not grant the browser shortcut to a lookalike ChatGPT return address", async () => {
    const { cookie } = await prepare();
    const request = await begin(cookie, "https://chatgpt.com.attacker.invalid/connector_platform_oauth_redirect");
    expect(request.html).toContain('name="pairing_code"');
    expect((await decide(request.id, cookie)).status).not.toBe(302);
  });

  it("expires browser verification without revealing a fallback pairing input", async () => {
    const { cookie } = await prepare();
    const original = Date.now();
    const now = vi.spyOn(Date, "now").mockReturnValue(original + 14 * 60_000);
    try {
      const request = await begin(cookie);
      now.mockReturnValue(original + 15 * 60_000 + 1);
      const response = await decide(request.id, cookie);
      expect(response.status).toBe(401);
      const html = await response.text();
      expect(html).toContain("本机验证已过期");
      expect(html).not.toContain('name="pairing_code"');
      expect(html).toContain("error=access_denied");
    } finally { now.mockRestore(); }
  });

  it("still rejects an incorrect PKCE verifier after browser consent", async () => {
    const { cookie } = await prepare();
    const request = await begin(cookie);
    const approval = await decide(request.id, cookie);
    const response = await exchange(approval.headers.get("location")!, request.clientId, "wrong-verifier".repeat(4));
    expect(response.status).toBe(400);
    expect((await response.json() as { error: string }).error).toBe("invalid_grant");
  });
});
