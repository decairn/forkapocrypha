import { useCallback, useEffect, useMemo, useRef, useState, type ComponentProps } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  ArrowDownAZ,
  ArrowUpCircle,
  CalendarDays,
  Check,
  CircleMinus,
  Clock,
  Library,
  ListFilter,
  Loader2,
  RefreshCw,
  AlertTriangle,
  Search as SearchIcon,
} from "lucide-react";
import { PageHeader } from "@/components/PageHeader";
import { InstalledRow } from "@/components/InstalledRow";
import { AddonDetailPanel } from "@/components/AddonDetailPanel";
import { SearchSelect } from "@/components/SearchSelect";
import { api } from "@/lib/api";
import type { Addon, InstalledAddon } from "@/lib/types";

type InstalledStatus = "all" | "up-to-date" | "update-available" | "orphan" | "unused-library";
type InstalledSort = "name" | "last-updated" | "last-released";
type InstalledSelection = { dirName: string };

const installedNameCollator = new Intl.Collator(undefined, { sensitivity: "base" });

function compareInstalledNames(a: InstalledAddon, b: InstalledAddon): number {
  const byTitle = installedNameCollator.compare(a.title, b.title);
  if (byTitle) return byTitle;
  if (a.dir_name === b.dir_name) return 0;
  return a.dir_name < b.dir_name ? -1 : 1;
}

function localUpdateTime(addon: InstalledAddon): number {
  // The installer refreshes installed_at on each successful local install or update.
  const time = Date.parse(addon.installed_at ?? "");
  return Number.isNaN(time) ? -Infinity : time;
}

export function InstalledPage({ catalogTick, onInstall, onUninstall }: {
  catalogTick: number;
  onInstall: (addonId: string) => Promise<string[]>;
  onUninstall: (dirName: string) => Promise<boolean>;
}) {
  const [addons, setAddons] = useState<InstalledAddon[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [refreshingCatalog, setRefreshingCatalog] = useState(false);
  const [catalogRefreshFailed, setCatalogRefreshFailed] = useState(false);
  const latestScan = useRef(0);
  const [uninstalling, setUninstalling] = useState<Set<string>>(new Set());
  const [updatingCatalogIds, setUpdatingCatalogIds] = useState<Set<string>>(new Set());
  const [bulkProgress, setBulkProgress] = useState<{ done: number; total: number } | null>(null);
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<InstalledStatus>("all");
  const [sort, setSort] = useState<InstalledSort>("name");

  const [selection, setSelection] = useState<InstalledSelection | null>(null);
  const selectedDirName = selection?.dirName;

  const reload = useCallback(async () => {
    const scan = ++latestScan.current;
    setLoading(true);
    setError(null);
    try {
      const installed = await api.listInstalled();
      if (scan === latestScan.current) {
        setAddons(installed);
        setSelection((selected) =>
          installed.some((addon) => addon.dir_name === selected?.dirName && addon.catalog_id) ? selected : null,
        );
      }
    } catch (error) {
      if (scan === latestScan.current) setError(String(error));
    } finally {
      if (scan === latestScan.current) setLoading(false);
    }
  }, []);

  const rescan = useCallback(async () => {
    setRefreshingCatalog(true);
    setCatalogRefreshFailed(false);
    try {
      await api.refreshCatalogForRescan();
    } catch (error) {
      console.warn("Could not refresh the ESOUI catalog:", error);
      setCatalogRefreshFailed(true);
    } finally {
      setRefreshingCatalog(false);
    }
    await reload();
  }, [reload]);

  useEffect(() => {
    setCatalogRefreshFailed(false);
    reload();
  }, [catalogTick, reload]);

  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    (async () => {
      unlisteners.push(
        await listen<{ id: string }>("addon:install:start", (e) => {
          setUpdatingCatalogIds((prev) => {
            const next = new Set(prev);
            next.add(e.payload.id);
            return next;
          });
        }),
      );
      unlisteners.push(
        await listen<{ id: string }>("addon:install:done", (e) => {
          setUpdatingCatalogIds((prev) => {
            const next = new Set(prev);
            next.delete(e.payload.id);
            return next;
          });
          reload();
        }),
      );
      unlisteners.push(
        await listen<{ id: string; error: string }>("addon:install:error", (e) => {
          setUpdatingCatalogIds((prev) => {
            const next = new Set(prev);
            next.delete(e.payload.id);
            return next;
          });
          console.error("install/update failed:", e.payload.error);
          reload();
        }),
      );
      unlisteners.push(
        await listen<{ dir_name: string }>("addon:uninstall:done", (e) => {
          setUninstalling((prev) => {
            const next = new Set(prev);
            next.delete(e.payload.dir_name);
            return next;
          });
          setSelection((selected) => selected?.dirName === e.payload.dir_name ? null : selected);
          reload();
        }),
      );
      unlisteners.push(
        await listen<{ dir_name: string; error: string }>("addon:uninstall:error", (e) => {
          setUninstalling((prev) => {
            const next = new Set(prev);
            next.delete(e.payload.dir_name);
            return next;
          });
          console.error("uninstall failed:", e.payload.error);
        }),
      );
    })();
    return () => unlisteners.forEach((fn) => fn());
  }, [reload]);

  const handleUninstall = useCallback(async (addon: InstalledAddon) => {
    setUninstalling((prev) => {
      const next = new Set(prev);
      next.add(addon.dir_name);
      return next;
    });
    try {
      await onUninstall(addon.dir_name);
    } catch (e) {
      console.error(e);
    } finally {
      setUninstalling((prev) => {
        const next = new Set(prev);
        next.delete(addon.dir_name);
        return next;
      });
    }
  }, [onUninstall]);

  const handleUpdate = useCallback(async (addon: InstalledAddon) => {
    if (!addon.catalog_id) return;
    try {
      await onInstall(addon.catalog_id);
    } catch (e) {
      console.error(e);
    }
  }, [onInstall]);

  const closePanel = useCallback(() => {
    setSelection(null);
  }, []);

  const updatesAvailable = useMemo(
    () => addons?.filter((a) => a.update_available).length ?? 0,
    [addons],
  );

  const unusedLibs = useMemo(() => addons?.filter((a) => a.unused_lib).length ?? 0, [addons]);

  const filteredAddons = useMemo(() => {
    if (!addons) return null;
    const q = query.trim().toLowerCase();
    const matching = addons.filter((a) => {
      const rowStatus = a.update_available ? "update-available" : a.catalog_id ? "up-to-date" : "orphan";
      const matchesStatus = status === "unused-library" ? a.unused_lib : status === "all" || status === rowStatus;
      return matchesStatus && (!q || [a.title, a.dir_name, a.author].some((value) => value?.toLowerCase().includes(q)));
    });
    const timestamp = sort === "last-released"
      ? (addon: InstalledAddon) => addon.catalog_last_updated ?? 0
      : localUpdateTime;
    return matching.sort((a, b) => {
      if (sort !== "name") {
        const aTime = timestamp(a);
        const bTime = timestamp(b);
        if (aTime !== bTime) return bTime - aTime;
      }
      return compareInstalledNames(a, b);
    });
  }, [addons, query, status, sort]);

  const handleUpdateAll = useCallback(async () => {
    if (!addons) return;
    const toUpdate = addons.filter((a) => a.update_available && a.catalog_id);
    if (toUpdate.length === 0) return;
    setBulkProgress({ done: 0, total: toUpdate.length });
    for (let i = 0; i < toUpdate.length; i++) {
      const addon = toUpdate[i];
      try {
        await onInstall(addon.catalog_id!);
      } catch (e) {
        console.error(`update failed for ${addon.title}:`, e);
      }
      setBulkProgress({ done: i + 1, total: toUpdate.length });
    }
    setBulkProgress(null);
  }, [addons, onInstall]);

  // Resolve details and actions from the same row in the latest scan.
  const currentSelectedInstalled = useMemo(() => {
    if (selectedDirName === undefined) return null;
    return addons?.find((addon) => addon.dir_name === selectedDirName) ?? null;
  }, [selectedDirName, addons]);

  const panelAction = useMemo(() => {
    const inst = currentSelectedInstalled;
    if (!inst?.catalog_id) return null;
    if (uninstalling.has(inst.dir_name)) {
      return { kind: "uninstalling" } as const;
    }
    if (updatingCatalogIds.has(inst.catalog_id)) {
      return { kind: "updating" } as const;
    }
    if (inst.update_available && inst.catalog_version) {
      return {
        kind: "update",
        targetVersion: inst.catalog_version,
      } as const;
    }
    return { kind: "uninstall" } as const;
  }, [currentSelectedInstalled, uninstalling, updatingCatalogIds]);

  return (
    <div className="relative flex h-full">
      <div className="flex min-w-0 flex-1 flex-col has-[+aside[data-expanded]]:invisible">
        <PageHeader
          title="Installed"
          subtitle={
            addons ? (
              <>
                {addons.length} {addons.length === 1 ? "addon" : "addons"} in your library
                {updatesAvailable > 0 && ` · ${updatesAvailable} ${updatesAvailable === 1 ? "update" : "updates"} available`}
                {unusedLibs > 0 && (
                  <>
                    {" · "}
                    <button
                      type="button"
                      onClick={() => setStatus("unused-library")}
                      className="rounded-sm text-primary underline underline-offset-4 hover:text-on-surface focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
                    >
                      {unusedLibs} {unusedLibs === 1 ? "unused library" : "unused libraries"}
                    </button>
                  </>
                )}
              </>
            ) : "Your library, updates, and dependency health."
          }
          actions={
            <>
              {updatesAvailable > 0 || bulkProgress ? (
                <button
                  onClick={handleUpdateAll}
                  disabled={!!bulkProgress}
                  className="inline-flex items-center gap-2 rounded-md border border-[var(--color-primary)]/40 bg-[var(--color-primary-container)]/20 px-4 py-2 text-sm font-medium text-[var(--color-primary)] hover:border-[var(--color-primary)] hover:bg-[var(--color-primary-container)]/35 disabled:opacity-70"
                >
                  {bulkProgress ? (
                    <>
                      <Loader2 className="h-4 w-4 animate-spin" strokeWidth={1.75} />
                      Updating {Math.min(bulkProgress.done + 1, bulkProgress.total)} of{" "}
                      {bulkProgress.total}
                    </>
                  ) : (
                    <>
                      <ArrowUpCircle className="h-4 w-4" strokeWidth={1.75} />
                      Update All ({updatesAvailable})
                    </>
                  )}
                </button>
              ) : null}
              <button
                onClick={rescan}
                disabled={refreshingCatalog || loading || !!bulkProgress}
                className="inline-flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-low)] px-4 py-2 text-sm text-[var(--color-on-surface-variant)] hover:border-[var(--color-primary)] hover:text-[var(--color-primary)] disabled:opacity-50"
              >
                {refreshingCatalog || loading ? (
                  <Loader2 className="h-4 w-4 animate-spin" strokeWidth={1.75} />
                ) : (
                  <RefreshCw className="h-4 w-4" strokeWidth={1.75} />
                )}
                {refreshingCatalog ? "Checking ESOUI…" : loading ? "Scanning…" : "Re-scan"}
              </button>
            </>
          }
        />

        {catalogRefreshFailed ? (
          <p className="px-10 pt-4 text-sm text-[var(--color-on-surface-variant)]" role="status">
            Couldn’t refresh the ESOUI catalog. Update availability may be out of date.
          </p>
        ) : null}

        {addons && addons.length > 0 ? (
          <div className="flex flex-wrap items-center gap-3 px-10 pb-6 pt-8">
            <div className="relative min-w-0 flex-[1_1_12rem]">
              <SearchIcon className="pointer-events-none absolute left-4 top-1/2 h-5 w-5 -translate-y-1/2 text-[var(--color-outline)]" />
              <input
                type="text"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="Filter installed addons..."
                className="w-full rounded-md border border-[var(--color-border)] bg-[var(--color-surface-lowest)] py-3.5 pl-12 pr-4 text-base text-[var(--color-on-surface)] placeholder:text-[var(--color-outline)] focus:border-[var(--color-primary)] focus:outline-none focus:ring-1 focus:ring-[var(--color-primary)]"
              />
            </div>
            <SearchSelect<InstalledStatus>
              label="Addon status"
              value={status}
              onChange={setStatus}
              widthClass="w-48 max-w-full"
              options={[
                { key: "all", label: "All statuses", icon: ListFilter },
                { key: "up-to-date", label: "Up to date", icon: Check },
                { key: "update-available", label: "Update available", icon: ArrowUpCircle },
                { key: "orphan", label: "Orphan", icon: CircleMinus },
                { key: "unused-library", label: "Unused libraries", icon: Library },
              ]}
            />
            <SearchSelect<InstalledSort>
              label="Sort by"
              value={sort}
              onChange={setSort}
              widthClass="w-44 max-w-full"
              options={[
                { key: "name", label: "Name", icon: ArrowDownAZ },
                { key: "last-updated", label: "Last Updated", icon: Clock },
                { key: "last-released", label: "Last Released", icon: CalendarDays },
              ]}
            />
          </div>
        ) : null}

        <div className="flex-1 overflow-y-auto px-10 pb-8">
          {loading && !addons ? (
            <EmptyState
              icon={<Loader2 className="h-6 w-6 animate-spin text-[var(--color-primary)]" />}
              title="Scanning your AddOns folder..."
            />
          ) : error ? (
            <EmptyState
              icon={<AlertTriangle className="h-6 w-6 text-[var(--color-error)]" />}
              title="Failed to scan your AddOns folder."
              subtitle={error}
            />
          ) : addons && addons.length === 0 ? (
            <EmptyState
              title="No addons indexed yet."
              subtitle="ESO either isn't installed or your AddOns folder is empty. Check Settings → AddOns Folder."
            />
          ) : filteredAddons && filteredAddons.length === 0 ? (
            <EmptyState
              title={status === "unused-library" && unusedLibs === 0 ? "No unused libraries." : "No matching addons."}
              subtitle={query.trim() ? "Try a different search term or status." : undefined}
            />
          ) : (
            <div className="overflow-hidden rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-lowest)]">
              {filteredAddons?.map((a) => (
                <InstalledRow
                  key={a.dir_name}
                  addon={a}
                  uninstalling={uninstalling.has(a.dir_name)}
                  updating={a.catalog_id ? updatingCatalogIds.has(a.catalog_id) : false}
                  selected={selectedDirName === a.dir_name}
                  onUninstall={() => handleUninstall(a)}
                  onUpdate={() => handleUpdate(a)}
                  onSelect={a.catalog_id ? () => setSelection({ dirName: a.dir_name }) : undefined}
                />
              ))}
            </div>
          )}
        </div>
      </div>

      {selection && currentSelectedInstalled?.catalog_id && panelAction ? (
        <InstalledAddonDetails
          key={JSON.stringify([currentSelectedInstalled.dir_name, currentSelectedInstalled.catalog_id])}
          selection={selection}
          catalogId={currentSelectedInstalled.catalog_id}
          action={panelAction}
          installedDirName={currentSelectedInstalled.dir_name}
          installedAt={currentSelectedInstalled.installed_at}
          unusedLibrary={currentSelectedInstalled.unused_lib}
          onClose={closePanel}
          onUpdate={() => handleUpdate(currentSelectedInstalled)}
          onUninstall={() => handleUninstall(currentSelectedInstalled)}
        />
      ) : null}
    </div>
  );
}

function InstalledAddonDetails({ selection, catalogId, ...panelProps }:
  Omit<ComponentProps<typeof AddonDetailPanel>, "addon"> & { selection: InstalledSelection; catalogId: string },
) {
  const [addon, setAddon] = useState<Addon | null>(null);

  // Selecting the row again retries an unavailable catalog lookup.
  useEffect(() => {
    let active = true;
    api.getAddonById(catalogId)
      .then((catalog) => {
        if (active && catalog?.id === catalogId) setAddon(catalog);
      })
      .catch((error) => {
        if (active) console.error("failed to load catalog data:", error);
      });
    return () => {
      active = false;
    };
  }, [catalogId, selection]);

  return addon ? <AddonDetailPanel addon={addon} {...panelProps} /> : null;
}

function EmptyState({
  icon,
  title,
  subtitle,
}: {
  icon?: React.ReactNode;
  title: string;
  subtitle?: string;
}) {
  return (
    <div className="grid h-full place-items-center rounded-lg border border-dashed border-[var(--color-border)] text-center">
      <div className="max-w-md px-6">
        {icon ? <div className="mb-3 flex justify-center">{icon}</div> : null}
        <p className="text-lg font-medium text-[var(--color-on-surface-variant)]">{title}</p>
        {subtitle ? <p className="mt-2 text-sm text-[var(--color-outline)]">{subtitle}</p> : null}
      </div>
    </div>
  );
}
