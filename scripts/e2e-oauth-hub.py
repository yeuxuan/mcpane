#!/usr/bin/env python3
"""远程 MCP OAuth 的真实进程端到端（ADR-024）。

真实 mcpane-hub 二进制 + 独立的假 OAuth / MCP 服务器进程；像桌面端那样走控制面 UDS
（oauth-begin → 浏览器 → 回环回调 → oauth-complete），再像 Claude Code 那样走数据面调工具。
覆盖：未登录 needs-auth 且不重连、登录、重启保持登录、退出登录、令牌过期自动刷新、孤儿令牌回收。

用法（仓库根目录；仅 macOS / Linux，用 UDS 控制面）：
    cargo build -p mcpane-hub --bin mcpane-hub
    cargo build -p mcpane-hub-downstream --example oauth_fake_server
    python3 scripts/e2e-oauth-hub.py

不碰你正在跑的 Hub：数据目录是临时目录，数据面用 127.0.0.1:34860。
"""
import http.server, json, os, shutil, socket, sqlite3, stat, subprocess, sys, threading, time, urllib.parse, urllib.request

import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WORK = tempfile.mkdtemp(prefix="mcpane-e2e-")
HOME = os.path.join(WORK, "home")
BIND = "127.0.0.1:34860"
TOKEN = "e2e-claude-token"
procs = []
results = []

def check(name, cond, detail=""):
    results.append((name, bool(cond)))
    print(("  ✓ " if cond else "  ✗ ") + name + ("" if cond else f"  <-- {detail}"))
    if not cond:
        raise AssertionError(name + " " + str(detail))

def start_fake(expires=None):
    env = dict(os.environ)
    if expires: env["FAKE_EXPIRES_IN"] = str(expires)
    p = subprocess.Popen([f"{ROOT}/target/debug/examples/oauth_fake_server"], stdout=subprocess.PIPE, text=True, env=env)
    procs.append(p)
    line = p.stdout.readline().strip()
    assert line.startswith("MCP_URL="), line
    return line[len("MCP_URL="):], p

def start_hub():
    env = dict(os.environ, MCPANE_HOME=HOME, MCPANE_HUB_BIND=BIND, MCPANE_CLAUDE_TOKEN=TOKEN,
               MCPANE_HEALTH_INTERVAL_SECS="2", RUST_LOG="info")
    log = open(os.path.join(WORK, "hub.log"), "ab")
    p = subprocess.Popen([f"{ROOT}/target/debug/mcpane-hub"], env=env, stdout=log, stderr=log)
    procs.append(p)
    for _ in range(100):
        try:
            socket.create_connection(("127.0.0.1", 34860), timeout=0.2).close()
            if os.path.exists(f"{HOME}/hub-control.sock"): return p
        except OSError: pass
        time.sleep(0.1)
    raise RuntimeError("hub 未启动")

def stop(p):
    p.terminate()
    try: p.wait(5)
    except subprocess.TimeoutExpired: p.kill()
    if p in procs: procs.remove(p)

def control(cmd, timeout=120):
    s = socket.socket(socket.AF_UNIX); s.settimeout(timeout)
    s.connect(f"{HOME}/hub-control.sock")
    s.sendall((json.dumps(cmd) + "\n").encode())
    data = b""
    while not data.endswith(b"\n"):
        chunk = s.recv(65536)
        if not chunk: break
        data += chunk
    s.close()
    return json.loads(data)

def diag(server_id):
    return control({"cmd": "diagnostics"})["downstreams"].get(server_id, {})

def wait_status(server_id, want, secs=15):
    end = time.time() + secs
    while time.time() < end:
        d = diag(server_id)
        if d.get("status") == want: return d
        time.sleep(0.3)
    return diag(server_id)

def seed_server(server_id, slug, url):
    db = sqlite3.connect(f"{HOME}/mcpane.db", timeout=10)
    db.execute("INSERT OR REPLACE INTO mcp_servers (id,slug,name,transport,url,headers,runtime_policy,enabled,created_at) VALUES (?,?,?,?,?,'{}','shared',1,?)",
               (server_id, slug, slug, "http", url, int(time.time())))
    db.commit(); db.close()

def delete_server(server_id):
    db = sqlite3.connect(f"{HOME}/mcpane.db", timeout=10)
    db.execute("DELETE FROM mcp_servers WHERE id=?", (server_id,)); db.commit(); db.close()

def reload(): return control({"cmd": "reload-servers"})

# ---- 数据面：像 Claude Code 那样的 MCP 客户端 ----
def mcp(method, params=None, session=None, rid=1):
    body = {"jsonrpc": "2.0", "id": rid, "method": method}
    if params is not None: body["params"] = params
    headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream",
               "Authorization": f"Bearer {TOKEN}"}
    if session: headers["Mcp-Session-Id"] = session
    req = urllib.request.Request(f"http://{BIND}/mcp", json.dumps(body).encode(), headers)
    resp = urllib.request.urlopen(req, timeout=60)
    text = resp.read().decode()
    out = None
    for line in text.splitlines():
        if line.startswith("data:"):
            try: out = json.loads(line[5:]); break
            except ValueError: pass
    if out is None:
        try: out = json.loads(text)
        except ValueError: pass
    return resp.headers.get("Mcp-Session-Id"), out

def client_session():
    sid, _ = mcp("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "claude-code", "version": "2"}})
    req = urllib.request.Request(f"http://{BIND}/mcp", json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}).encode(),
        {"Content-Type": "application/json", "Accept": "application/json, text/event-stream", "Authorization": f"Bearer {TOKEN}", **({"Mcp-Session-Id": sid} if sid else {})})
    urllib.request.urlopen(req, timeout=30).read()
    return sid

def tool_names(session):
    _, out = mcp("tools/list", {}, session, 2)
    return [t["name"] for t in out["result"]["tools"]]

def call_echo(session, slug, text):
    _, out = mcp("tools/call", {"name": f"{slug}__echo", "arguments": {"text": text}}, session, 3)
    return out

# ---- 模拟桌面端：本机回环回调 + 「浏览器」----
class Callback(http.server.BaseHTTPRequestHandler):
    captured = None
    def do_GET(self):
        q = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        if urllib.parse.urlparse(self.path).path == "/callback":
            Callback.captured = {k: v[0] for k, v in q.items()}
        self.send_response(200); self.end_headers(); self.wfile.write(b"ok")
    def log_message(self, *a): pass

def desktop_login(server_id):
    """桌面端 server_oauth_login 的流程：绑回环端口 → oauth-begin → 浏览器授权 → 回调 → oauth-complete。"""
    Callback.captured = None
    srv = http.server.HTTPServer(("127.0.0.1", 0), Callback)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    redirect = f"http://127.0.0.1:{srv.server_address[1]}/callback"
    begun = control({"cmd": "oauth-begin", "server_id": server_id, "redirect_uri": redirect})
    if not begun.get("ok"): srv.shutdown(); return begun, None
    # 「浏览器」打开授权链接；授权服务器 302 到我们的回环地址（urllib 自动跟随）。
    urllib.request.urlopen(begun["authorizationUrl"], timeout=30).read()
    for _ in range(50):
        if Callback.captured: break
        time.sleep(0.1)
    srv.shutdown()
    cb = Callback.captured
    done = control({"cmd": "oauth-complete", "server_id": server_id, "code": cb["code"], "state": cb["state"]})
    return begun, done

def fake_debug(url):
    base = url.rsplit("/mcp", 1)[0]
    return json.load(urllib.request.urlopen(base + "/__debug", timeout=10))

def mode(path): return stat.S_IMODE(os.stat(path).st_mode)

def main():
    shutil.rmtree(HOME, ignore_errors=True); os.makedirs(HOME)
    fake_url, fake_proc = start_fake()
    hub = start_hub()
    try:
        print("\n[A] 未登录：受保护的远程服务器 → needs-auth（不是 failed）")
        seed_server("srv_oauth", "oauthfake", fake_url)
        check("reload-servers 成功", reload().get("ok") is True)
        d = wait_status("srv_oauth", "needs-auth")
        check("状态是 needs-auth", d.get("status") == "needs-auth", d)
        check("失败原因代号 needs_auth", d.get("lastErrorKind") == "needs_auth", d)
        check("signedIn=false", d.get("signedIn") is False, d)
        sid = client_session()
        check("未登录时客户端看不到该服务器的工具", not any(n.startswith("oauthfake__") for n in tool_names(sid)))
        hits = fake_debug(fake_url)["mcpHits"]
        time.sleep(7)  # 健康监控 2s 一轮，多走几轮
        check("需要登录期间 Hub 不再反复撞服务器（健康监控不重连）", fake_debug(fake_url)["mcpHits"] == hits,
              (hits, fake_debug(fake_url)["mcpHits"]))

        print("\n[B] 登录：begin → 浏览器授权 → 回环回调 → complete")
        begun, done = desktop_login("srv_oauth")
        check("oauth-begin 返回授权链接", begun.get("ok") and "/authorize" in begun.get("authorizationUrl", ""), begun)
        q = urllib.parse.parse_qs(urllib.parse.urlparse(begun["authorizationUrl"]).query)
        check("授权链接带 PKCE S256 与 state", q.get("code_challenge_method") == ["S256"] and "state" in q and "code_challenge" in q, q)
        check("oauth-complete 成功且服务器已 ready", done.get("ok") and done.get("status") == "ready", done)
        d = diag("srv_oauth")
        check("diagnostics: signedIn=true", d.get("signedIn") is True, d)
        sid = client_session()
        names = tool_names(sid)
        check("客户端经 Hub 看到受保护服务器的工具", "oauthfake__echo" in names, names)
        out = call_echo(sid, "oauthfake", "hello-through-hub")
        check("经 Hub 调用受保护的工具成功", "hello-through-hub" in json.dumps(out), out)
        tok = f"{HOME}/mcp-oauth/srv_oauth.json"
        check("令牌文件存在，权限 0600，目录 0700", os.path.isfile(tok) and mode(tok) == 0o600 and mode(os.path.dirname(tok)) == 0o700,
              (os.path.exists(tok), oct(mode(tok)) if os.path.exists(tok) else None))
        dbg = fake_debug(fake_url)
        check("授权服务器：注册 1 次、只发生授权码换令牌", dbg["registrations"] == 1 and dbg["grants"] == ["authorization_code"], dbg)
        check("令牌不在 SQLite 里", b"access_token" not in open(f"{HOME}/mcpane.db", "rb").read() + open(f"{HOME}/mcpane.db-wal", "rb").read() if os.path.exists(f"{HOME}/mcpane.db-wal") else True)

        print("\n[C] Hub 重启后仍保持登录（令牌持久化）")
        stop(hub); hub = start_hub()
        d = wait_status("srv_oauth", "ready", 20)
        check("重启后无需重新登录即 ready", d.get("status") == "ready" and d.get("signedIn") is True, d)
        sid = client_session()
        check("重启后仍能调用工具", "restart-ok" in json.dumps(call_echo(sid, "oauthfake", "restart-ok")))

        print("\n[D] 退出登录：令牌删除，回到 needs-auth")
        out = control({"cmd": "oauth-logout", "server_id": "srv_oauth"})
        check("oauth-logout 成功", out.get("ok") is True, out)
        d = wait_status("srv_oauth", "needs-auth")
        check("回到 needs-auth 且 signedIn=false", d.get("status") == "needs-auth" and d.get("signedIn") is False, d)
        check("令牌文件已删除", not os.path.exists(tok))
        sid = client_session()
        check("退出后客户端不再看到该服务器的工具", "oauthfake__echo" not in tool_names(sid))

        print("\n[E] 令牌过期：自动用 refresh_token 续期（令牌 5 秒过期）")
        fake2_url, fake2 = start_fake(expires=5)
        seed_server("srv_short", "shortlived", fake2_url); reload()
        wait_status("srv_short", "needs-auth")
        _, done = desktop_login("srv_short")
        check("短寿命令牌的服务器登录成功", done.get("ok") and done.get("status") == "ready", done)
        time.sleep(8)  # 令牌已过期
        sid = client_session()
        check("令牌过期后调用仍成功（自动刷新）", "after-expiry" in json.dumps(call_echo(sid, "shortlived", "after-expiry")))
        dbg = fake_debug(fake2_url)
        check("授权服务器确实收到了 refresh_token 请求", "refresh_token" in dbg["grants"], dbg)

        print("\n[F] 服务器被删除：孤儿令牌被回收")
        tok2 = f"{HOME}/mcp-oauth/srv_short.json"
        check("删除前令牌文件在", os.path.isfile(tok2))
        delete_server("srv_short"); delete_server("srv_oauth"); reload()
        check("删除服务器后令牌文件被回收", not os.path.exists(tok2))
        stop(fake2)
    finally:
        for p in list(procs): stop(p)
    print(f"\n全部 {len(results)} 项检查通过（日志与数据目录：{WORK}）")

if __name__ == "__main__":
    try: main()
    except Exception as e:
        print("\n失败：", repr(e), f"\nHub 日志：{os.path.join(WORK, 'hub.log')}")
        for p in list(procs): stop(p)
        sys.exit(1)
