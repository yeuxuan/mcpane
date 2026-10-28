import { describe, expect, it } from "vitest";
import indexHtml from "../index.html?raw";
import tauriConf from "../src-tauri/tauri.conf.json";

/**
 * index.html 的内联主题脚本靠 CSP 里的 sha256 放行：脚本文本改一个空格哈希就变，
 * 生产构建里会被静默拦掉（不崩、只是暗色启动重新闪白）。这里把两边锁在一起。
 */
describe("index.html 内联脚本与 CSP 哈希", () => {
  it("tauri.conf.json 的 script-src 含内联脚本当前内容的 sha256", async () => {
    const scripts = [...indexHtml.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((m) => m[1]);
    expect(scripts).toHaveLength(1);
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(scripts[0]));
    const hash = btoa(String.fromCharCode(...new Uint8Array(digest)));

    expect(tauriConf.app.security.csp).toContain(`script-src 'self' 'sha256-${hash}'`);
  });
});
