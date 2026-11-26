// From app/: npx vp test run --config tests/browser/vitest.config.ts
import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite-plus";
import { preview } from "vite-plus/test/browser-preview";

export default defineConfig({
  plugins: [react()],
  resolve: { alias: { "@": fileURLToPath(new URL("../../src", import.meta.url)) } },
  define: { __APP_VERSION__: '"test"', __IS_FORK__: "false", __ABOUT_LINKS__: "[]" },
  optimizeDeps: { include: [
    "@tauri-apps/api/webview",
    "@tauri-apps/plugin-autostart",
    "@tauri-apps/plugin-dialog",
    "@tauri-apps/plugin-process",
    "@tauri-apps/plugin-updater",
    "clsx",
    "tailwind-merge",
  ] },
  test: {
    include: ["tests/browser/*.test.tsx"],
    browser: { enabled: true, provider: preview(), instances: [{ browser: "chromium" }] },
  },
});
