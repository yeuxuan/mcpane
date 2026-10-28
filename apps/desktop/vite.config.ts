/// <reference types="vitest/config" />
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Tauri devUrl 固定 1420 端口（见 src-tauri/tauri.conf.json）
export default defineConfig({
  plugins: [react(), tailwindcss()],
  // 与 tsconfig paths 保持一致：shadcn 组件之间用 @/ 互相引用。
  resolve: {
    alias: { "@": new URL("./src", import.meta.url).pathname },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    globals: false,
    // e2e/*.spec.ts 是 Playwright 用例，走 `pnpm e2e`；vitest 只跑单测。
    exclude: ["**/node_modules/**", "**/dist/**", "e2e/**"],
  },
});
