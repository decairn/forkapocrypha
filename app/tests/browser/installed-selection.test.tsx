import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vite-plus/test";
import type { Addon, InstalledAddon } from "@/lib/types";
import { useInstalledScan } from "@/lib/useInstalledScan";
import { InstalledPage } from "@/pages/InstalledPage";

function listing(id: string, version: string): Addon {
  return {
    id,
    name: `Listing ${id}`,
    version,
    file_info_url: `https://www.esoui.com/downloads/info${id}.html`,
    author: "Author",
    category_id: null,
    last_updated: 0,
    download_total: 0,
    download_monthly: 0,
    favorite_total: 0,
    directories: ["Example"],
    thumbnail_url: null,
    images: [],
  };
}

function installed(addon: Addon | null): InstalledAddon {
  return {
    dir_name: "Example",
    title: "Example",
    version: "1",
    author: null,
    description: null,
    depends_on: [],
    optional_depends_on: [],
    catalog_id: addon?.id ?? null,
    catalog_version: addon?.version ?? null,
    category_id: null,
    thumbnail_url: null,
    update_available: addon !== null,
    catalog_last_updated: null,
    installed_at: null,
    unused_lib: false,
  };
}

const original = listing("101", "2");
const replacement = listing("202", "3");
const successor = listing("303", "4");
const onInstall = vi.fn(async (_id: string) => [] as string[]);
const onUninstall = vi.fn(async (_dir: string) => true);
const openUrl = vi.fn();
let container: HTMLDivElement;
let root: Root;
let rows: InstalledAddon[];
let requests: string[];
let pending: Map<string, (addon: Addon | null) => void>;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.body.appendChild(document.createElement("div"));
  root = createRoot(container);
  rows = [installed(original)];
  requests = [];
  pending = new Map();
  onInstall.mockClear();
  onUninstall.mockClear();
  openUrl.mockClear();
  mockIPC((command, args) => {
    switch (command) {
      case "list_installed": return rows;
      case "refresh_catalog_for_rescan": return 0;
      case "get_addon_details": return null;
      case "plugin:opener|open_url": return openUrl(args);
      case "get_addon_by_id": {
        const id = (args as { id: string }).id;
        requests.push(id);
        return new Promise<Addon | null>((resolve) => pending.set(id, resolve));
      }
      default: throw new Error(`Unexpected IPC command ${command}`);
    }
  }, { shouldMockEvents: true });
});

afterEach(async () => {
  await act(() => root.unmount());
  clearMocks();
  container.remove();
  vi.unstubAllGlobals();
});

async function click(element: HTMLElement | null | undefined) {
  expect(element).toBeInstanceOf(HTMLElement);
  await act(() => element!.click());
}

function button(label: string, scope: ParentNode = container) {
  return [...scope.querySelectorAll("button")].find((element) => element.textContent?.trim() === label);
}

function panel() {
  return container.querySelector("aside");
}

async function selectRow() {
  await act(() => root.render(<InstalledPageFixture />));
  await click(container.querySelector("article[role=button]"));
}

function InstalledPageFixture() {
  const scan = useInstalledScan(true, 0);
  return <InstalledPage scan={scan} onInstall={onInstall} onUninstall={onUninstall} />;
}

async function resolveListing(addon: Addon, result: Addon | null = addon) {
  const resolve = pending.get(addon.id);
  expect(resolve).toBeTypeOf("function");
  await act(() => resolve!(result));
}

async function rescan(addon: Addon | null) {
  rows = [installed(addon)];
  await click(button("Re-scan"));
}

test("re-scan hides the previous listing while loading and updates the replacement listing", async () => {
  await selectRow();
  await resolveListing(original);
  expect(panel()?.querySelector("h2")?.textContent).toBe(original.name);

  await rescan(replacement);
  expect(panel()?.querySelector("h2")?.textContent).not.toBe(original.name);
  expect(panel() && button("Update to v3", panel()!)).toBeFalsy();
  expect(requests).toEqual([original.id, replacement.id]);

  await resolveListing(replacement);
  expect(panel()?.querySelector("h2")?.textContent).toBe(replacement.name);
  await click(button("Source", panel()!));
  await click(button("Open on ESOUI", panel()!));
  expect(openUrl).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ url: replacement.file_info_url }));
  await click(button("Update to v3", panel()!));
  expect(onInstall).toHaveBeenCalledExactlyOnceWith(replacement.id);

  await click(panel()?.querySelector('[aria-label="Close"]'));
  expect(panel()).toBeNull();
});

test("a late response cannot replace the details for the latest catalog match", async () => {
  await selectRow();
  await resolveListing(original);
  await rescan(replacement);
  await rescan(successor);
  expect(requests).toEqual([original.id, replacement.id, successor.id]);

  await resolveListing(successor);
  await resolveListing(replacement);
  expect(panel()?.querySelector("h2")?.textContent).toBe(successor.name);
  await click(button("Update to v4", panel()!));
  expect(onInstall).toHaveBeenCalledExactlyOnceWith(successor.id);
});

test.each([
  { reason: "loses its catalog match", refreshedRows: [installed(null)] },
  { reason: "disappears", refreshedRows: [] },
])("re-scan closes details when the selected row $reason", async ({ refreshedRows }) => {
  await selectRow();
  await resolveListing(original);
  expect(panel()).not.toBeNull();

  rows = refreshedRows;
  await click(button("Re-scan"));
  expect(panel()?.querySelector("h2")?.textContent).toBeUndefined();
  expect(container.querySelector("article[role=button]")).toBeNull();
  await rescan(replacement);
  expect(panel()).toBeNull();
  expect(requests).toEqual([original.id]);
});

test("selecting the same row retries an unavailable listing", async () => {
  await selectRow();
  await resolveListing(original, null);
  expect(panel()).toBeNull();

  await click(container.querySelector("article[role=button]"));
  expect(requests).toEqual([original.id, original.id]);
  await resolveListing(original);
  expect(panel()?.querySelector("h2")?.textContent).toBe(original.name);
});
