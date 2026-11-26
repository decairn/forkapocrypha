import { emit } from "@tauri-apps/api/event";
import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vite-plus/test";
import App from "@/App";

let container: HTMLDivElement;
let root: Root;
let refresh: ReturnType<typeof Promise.withResolvers<number>>;
let scan: ReturnType<typeof Promise.withResolvers<unknown[]>> | undefined;
const listInstalled = vi.fn(() => scan?.promise ?? []);
const refreshCatalog = vi.fn(() => refresh.promise);

beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  localStorage.setItem("apocrypha:welcome-shown", "true");
  container = document.body.appendChild(document.createElement("div"));
  root = createRoot(container);
  refresh = Promise.withResolvers<number>();
  scan = undefined;
  listInstalled.mockClear();
  refreshCatalog.mockClear();
  mockWindows("main");
  mockIPC((command) => {
    switch (command) {
      case "list_installed": return listInstalled();
      case "refresh_catalog_for_rescan": return refreshCatalog();
      case "plugin:webview|set_webview_zoom": return undefined;
      case "list_categories": return [];
      case "search_addons": return { total: 0, addons: [] };
      case "catalog_meta": return { addon_count: 0, last_synced: null };
      case "get_addons_dir": return "AddOns";
      case "get_close_to_tray": return false;
      case "get_auto_update_interval": return 0;
      case "plugin:autostart|is_enabled": return false;
      default: throw new Error(`Unexpected IPC command ${command}`);
    }
  }, { shouldMockEvents: true });
  await act(() => root.render(<App />));
  await click("Installed");
  expect(listInstalled).toHaveBeenCalled();
  listInstalled.mockClear();
});

afterEach(async () => {
  await act(() => root.unmount());
  clearMocks();
  container.remove();
  localStorage.removeItem("apocrypha:welcome-shown");
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function click(label: string) {
  const button = [...container.querySelectorAll("button")].find((element) => element.textContent?.trim() === label);
  expect(button).toBeInstanceOf(HTMLButtonElement);
  await act(() => button!.click());
}

async function catalogDone(revision = 1) {
  await act(() => emit("catalog:sync:done", { addon_count: 1, last_synced: "2026-11-25T17:00:00Z", revision }));
}

test.each(["before", "after"])("re-scan walks the folder once with completion %s the response", async (order) => {
  await click("Re-scan");
  expect(refreshCatalog).toHaveBeenCalledTimes(1);

  if (order === "before") await catalogDone();
  await act(() => refresh.resolve(1));
  if (order === "after") await catalogDone();

  expect(listInstalled).toHaveBeenCalledTimes(1);
});

test("re-scan joins an active sync and preserves a later external completion", async () => {
  await act(() => emit("catalog:sync:start"));
  await click("Re-scan");
  await catalogDone(1);
  await act(() => refresh.resolve(1));
  expect(listInstalled).toHaveBeenCalledTimes(1);

  await catalogDone(2);
  expect(listInstalled).toHaveBeenCalledTimes(2);
});

test("a newer external completion received during the local scan still reloads Installed", async () => {
  await click("Re-scan");
  scan = Promise.withResolvers<unknown[]>();
  await act(() => refresh.resolve(1));
  await catalogDone(2);
  expect(listInstalled).toHaveBeenCalledTimes(1);

  await act(() => scan!.resolve([]));
  expect(listInstalled).toHaveBeenCalledTimes(2);
  await catalogDone(1);
  expect(listInstalled).toHaveBeenCalledTimes(2);
});

test("re-scan uses fresh cache without a catalog event", async () => {
  await click("Re-scan");
  await act(() => refresh.resolve(0));
  expect(listInstalled).toHaveBeenCalledTimes(1);
  expect(container.textContent).not.toContain("Update availability may be out of date");
});

test("failed catalog refresh still scans and warns about stale update availability", async () => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  await click("Re-scan");
  await act(() => refresh.reject(new Error("ESOUI unavailable")));
  expect(listInstalled).toHaveBeenCalledTimes(1);
  expect(container.textContent).toContain("Update availability may be out of date");
});

test("an external catalog sync reloads the open Installed page", async () => {
  await catalogDone();
  expect(listInstalled).toHaveBeenCalledTimes(1);
  expect(refreshCatalog).not.toHaveBeenCalled();
});

test("returning to Installed during a re-scan shares both stages of the same request", async () => {
  await click("Re-scan");
  await click("Settings");
  await click("Installed");
  expect(container.textContent).toContain("Checking ESOUI");

  scan = Promise.withResolvers<unknown[]>();
  await act(() => refresh.resolve(1));
  await click("Settings");
  await catalogDone();
  await click("Installed");
  await act(() => scan!.resolve([]));

  expect(refreshCatalog).toHaveBeenCalledTimes(1);
  expect(listInstalled).toHaveBeenCalledTimes(1);
});
