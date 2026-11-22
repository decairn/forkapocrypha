import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { ChevronDown, CircleCheck, CircleMinus, ListFilter, Loader2, Search as SearchIcon } from "lucide-react";
import { PageHeader } from "@/components/PageHeader";
import { AddonRow } from "@/components/AddonRow";
import { CategorySelect } from "@/components/CategorySelect";
import { SearchSelect } from "@/components/SearchSelect";
import { SortSelect } from "@/components/SortSelect";
import { AddonDetailPanel } from "@/components/AddonDetailPanel";
import { api } from "@/lib/api";
import { getCategoryMeta } from "@/lib/categoryMeta";
import type { Addon, Category, SortKey } from "@/lib/types";

const PAGE_SIZE = 30;
type InstallationStatus = "both" | "installed" | "not-installed";

export function SearchPage({ onInstall, onUninstall }: {
  onInstall: (addonId: string) => Promise<string[]>;
  onUninstall: (dirName: string) => Promise<boolean>;
}) {
  const [categories, setCategories] = useState<Category[]>([]);
  const [activeCategory, setActiveCategory] = useState<string>("");
  const [sortBy, setSortBy] = useState<SortKey>("download_total");
  const [installationStatus, setInstallationStatus] = useState<InstallationStatus>("both");
  const [queryInput, setQueryInput] = useState("");
  const [debouncedQuery, setDebouncedQuery] = useState("");

  const [addons, setAddons] = useState<Addon[]>([]);
  const [total, setTotal] = useState<number | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [installingIds, setInstallingIds] = useState<Set<string>>(new Set());
  const [uninstallingDirs, setUninstallingDirs] = useState<Set<string>>(new Set());
  // catalog_id → { dirName, installedAt }. Sourced from listInstalled() each
  // refresh. Keyed by catalog_id (not dir_name) so two catalog rows that
  // declare the same directory — e.g. EsoBR 2256 and the obsolete Reforged
  // 4541 both ship `EsoBR_Reforged/` — don't both light up as "installed"
  // when only one is actually on disk.
  const [installedByCatalogId, setInstalledByCatalogId] = useState<
    Map<string, { dirName: string; installedAt: string | null }>
  >();
  const [installedError, setInstalledError] = useState<string | null>(null);
  const installedRequest = useRef(0);
  const [selectedAddon, setSelectedAddon] = useState<Addon | null>(null);

  useEffect(() => {
    const t = setTimeout(() => setDebouncedQuery(queryInput), 250);
    return () => clearTimeout(t);
  }, [queryInput]);

  useEffect(() => {
    api
      .listCategories()
      .then(setCategories)
      .catch(() => setCategories([]));
  }, []);

  const refreshInstalledSet = useCallback(async () => {
    const request = ++installedRequest.current;
    try {
      const list = await api.listInstalled();
      if (request !== installedRequest.current) return;
      const next = new Map<string, { dirName: string; installedAt: string | null }>();
      for (const a of list) {
        if (a.catalog_id) {
          next.set(a.catalog_id, {
            dirName: a.dir_name,
            installedAt: a.installed_at,
          });
        }
      }
      setInstalledByCatalogId(next);
      setInstalledError(null);
    } catch (e) {
      if (request === installedRequest.current) setInstalledError(String(e));
    }
  }, []);

  useEffect(() => {
    refreshInstalledSet();
  }, [refreshInstalledSet]);

  const filterByInstallation = installationStatus !== "both";
  const installedForFilter = filterByInstallation ? installedByCatalogId : undefined;
  const installationError = filterByInstallation ? installedError : null;
  const waitingForInstalled = filterByInstallation && !installedForFilter;
  const searchParams = useMemo(
    () => ({
      query: debouncedQuery || undefined,
      categoryId: activeCategory || undefined,
      sortBy,
      limit: PAGE_SIZE,
      installationFilter: installedForFilter
        ? {
            installed: installationStatus === "installed",
            catalogIds: Array.from(installedForFilter.keys()),
          }
        : undefined,
    }),
    [debouncedQuery, activeCategory, sortBy, installationStatus, installedForFilter],
  );
  const [pagination, setPagination] = useState({ searchParams, page: 1 });
  const page = pagination.searchParams === searchParams ? pagination.page : 1;

  // Fetch the current page. Page 1 replaces; later pages append.
  useEffect(() => {
    if (waitingForInstalled || installationError) return;
    let alive = true;
    if (page === 1) {
      setLoading(true);
    } else {
      setLoadingMore(true);
    }
    setError(null);
    api
      .searchAddons({ ...searchParams, page })
      .then((r) => {
        if (!alive) return;
        setTotal(r.total);
        setAddons((prev) => (page === 1 ? r.addons : [...prev, ...r.addons]));
      })
      .catch((e) => {
        if (alive) setError(String(e));
      })
      .finally(() => {
        if (!alive) return;
        setLoading(false);
        setLoadingMore(false);
      });
    return () => {
      alive = false;
    };
  }, [page, searchParams, waitingForInstalled, installationError]);

  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    (async () => {
      unlisteners.push(
        await listen<{ id: string }>("addon:install:start", (e) => {
          setInstallingIds((prev) => {
            const next = new Set(prev);
            next.add(e.payload.id);
            return next;
          });
        }),
      );
      unlisteners.push(
        await listen<{ id: string }>("addon:install:done", (e) => {
          setInstallingIds((prev) => {
            const next = new Set(prev);
            next.delete(e.payload.id);
            return next;
          });
          refreshInstalledSet();
        }),
      );
      unlisteners.push(
        await listen<{ id: string; error: string }>("addon:install:error", (e) => {
          setInstallingIds((prev) => {
            const next = new Set(prev);
            next.delete(e.payload.id);
            return next;
          });
          console.error("install failed:", e.payload.error);
          refreshInstalledSet();
        }),
      );
      unlisteners.push(
        await listen<{ dir_name: string }>("addon:uninstall:done", (e) => {
          setUninstallingDirs((prev) => {
            const next = new Set(prev);
            next.delete(e.payload.dir_name);
            return next;
          });
          refreshInstalledSet();
        }),
      );
      unlisteners.push(
        await listen<{ dir_name: string; error: string }>("addon:uninstall:error", (e) => {
          setUninstallingDirs((prev) => {
            const next = new Set(prev);
            next.delete(e.payload.dir_name);
            return next;
          });
          console.error("uninstall failed:", e.payload.error);
          refreshInstalledSet();
        }),
      );
    })();
    return () => unlisteners.forEach((fn) => fn());
  }, [refreshInstalledSet]);

  const handleInstall = useCallback((addon: Addon) => {
    onInstall(addon.id).catch((e) => console.error("install error:", e));
  }, [onInstall]);

  const isAddonInstalled = useCallback(
    (addon: Addon) => installedByCatalogId?.has(addon.id) ?? false,
    [installedByCatalogId],
  );

  const installedDirFor = useCallback(
    (addon: Addon): string | null => installedByCatalogId?.get(addon.id)?.dirName ?? null,
    [installedByCatalogId],
  );

  const installedAtFor = useCallback(
    (addon: Addon): string | null => installedByCatalogId?.get(addon.id)?.installedAt ?? null,
    [installedByCatalogId],
  );

  const handleUninstall = useCallback(
    async (addon: Addon) => {
      const dir = installedByCatalogId?.get(addon.id)?.dirName;
      if (!dir) return;
      setUninstallingDirs((prev) => {
        const next = new Set(prev);
        next.add(dir);
        return next;
      });
      try {
        await onUninstall(dir);
      } catch (e) {
        console.error("uninstall error:", e);
      } finally {
        setUninstallingDirs((prev) => {
          const next = new Set(prev);
          next.delete(dir);
          return next;
        });
      }
    },
    [installedByCatalogId, onUninstall],
  );

  const isAddonUninstalling = useCallback(
    (addon: Addon) => {
      const dir = installedByCatalogId?.get(addon.id)?.dirName;
      return dir ? uninstallingDirs.has(dir) : false;
    },
    [installedByCatalogId, uninstallingDirs],
  );

  const empty = !loading && addons.length === 0 && total === 0;
  const catalogEmpty = empty && !debouncedQuery && !activeCategory && !filterByInstallation;
  const searchError = installationError ?? error;
  const showLoading = loading || waitingForInstalled;

  const canLoadMore = total !== null && addons.length < total && !loading && !error;

  const selectedCategory = categories.find((c) => c.id === activeCategory);
  const selectedMeta = activeCategory ? getCategoryMeta(activeCategory) : null;

  const panelAction = selectedAddon
    ? installingIds.has(selectedAddon.id)
      ? ({ kind: "installing" } as const)
      : isAddonInstalled(selectedAddon)
        ? ({ kind: "installed" } as const)
        : ({ kind: "install" } as const)
    : null;

  return (
    <div className="relative flex h-full">
      <div className="@container flex min-w-0 flex-1 flex-col has-[+aside[data-expanded]]:invisible">
        <PageHeader
          title="Search"
          subtitle={
            total !== null && !searchError && !showLoading
              ? `Showing ${addons.length.toLocaleString()} of ${total.toLocaleString()} addons.`
              : "Browse the arcane catalog of ESO addons."
          }
        />

        <div className="flex flex-wrap items-center gap-3 px-4 pt-8 @2xl:px-10 [&>*]:max-w-full">
          <div className="flex min-w-0 flex-[1_1_20rem] items-center gap-3">
            <div className="relative min-w-0 flex-1">
              <SearchIcon className="pointer-events-none absolute left-4 top-1/2 h-5 w-5 -translate-y-1/2 text-[var(--color-outline)]" />
              <input
                type="text"
                value={queryInput}
                onChange={(e) => setQueryInput(e.target.value)}
                placeholder="Search addons..."
                className="w-full rounded-md border border-[var(--color-border)] bg-[var(--color-surface-lowest)] py-3.5 pl-12 pr-4 text-base text-[var(--color-on-surface)] placeholder:text-[var(--color-outline)] focus:border-[var(--color-primary)] focus:outline-none focus:ring-1 focus:ring-[var(--color-primary)]"
              />
            </div>
            <SearchSelect<InstallationStatus>
              label="Installation status"
              value={installationStatus}
              onChange={setInstallationStatus}
              widthClass="w-44 max-w-[65%]"
              options={[
                { key: "both", label: "Both", icon: ListFilter },
                { key: "installed", label: "Installed", icon: CircleCheck },
                { key: "not-installed", label: "Not Installed", icon: CircleMinus },
              ]}
            />
          </div>

          <SortSelect value={sortBy} onChange={setSortBy} />
          <CategorySelect
            categories={categories}
            value={activeCategory}
            onChange={setActiveCategory}
          />
        </div>

        {selectedCategory && selectedMeta ? (
          <div className="mt-3 px-10">
            <div
              className="flex items-center gap-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-lowest)] px-4 py-2.5"
              style={{ borderLeftColor: selectedMeta.color, borderLeftWidth: 3 }}
            >
              <selectedMeta.icon
                className="h-4 w-4 shrink-0"
                strokeWidth={1.75}
                style={{ color: selectedMeta.color }}
              />
              <p className="text-sm text-[var(--color-on-surface-variant)]">
                <span className="font-medium text-[var(--color-on-surface)]">
                  {selectedCategory.title}
                </span>
                <span className="mx-2 text-[var(--color-outline-variant)]">·</span>
                {selectedMeta.description}
              </p>
            </div>
          </div>
        ) : null}

        <div className="flex-1 overflow-y-auto px-4 py-6 @2xl:px-10">
          {searchError ? (
            <CenteredState title="Something went wrong." subtitle={searchError} tone="error" />
          ) : showLoading ? (
            <CenteredState
              icon={<Loader2 className="h-6 w-6 animate-spin text-[var(--color-primary)]" />}
              title="Reading the archive..."
              subtitle="Loading addons from your local catalog."
            />
          ) : catalogEmpty ? (
            <CenteredState
              title="The catalog is empty."
              subtitle="Open Settings → Catalog → Sync now to download from ESOUI."
            />
          ) : empty ? (
            <CenteredState
              title="No matches found."
              subtitle="Try a different search term or change your filters."
            />
          ) : (
            <>
              <div className="@container/addon-list overflow-hidden rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-lowest)]">
                {addons.map((a) => (
                  <AddonRow
                    key={a.id}
                    addon={a}
                    installing={installingIds.has(a.id)}
                    installed={isAddonInstalled(a)}
                    uninstalling={isAddonUninstalling(a)}
                    selected={selectedAddon?.id === a.id}
                    onInstall={() => handleInstall(a)}
                    onUninstall={() => handleUninstall(a)}
                    onSelect={() => setSelectedAddon(a)}
                  />
                ))}
              </div>

              {canLoadMore ? (
                <div className="mt-4 flex justify-center">
                  <button
                    onClick={() => setPagination({ searchParams, page: page + 1 })}
                    disabled={loadingMore}
                    className="inline-flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-low)] px-5 py-2.5 text-sm font-medium text-[var(--color-on-surface-variant)] hover:border-[var(--color-primary)] hover:text-[var(--color-primary)] disabled:opacity-60"
                  >
                    {loadingMore ? (
                      <>
                        <Loader2 className="h-4 w-4 animate-spin" strokeWidth={1.75} />
                        Loading...
                      </>
                    ) : (
                      <>
                        <ChevronDown className="h-4 w-4" strokeWidth={1.75} />
                        Load more ({(total! - addons.length).toLocaleString()} remaining)
                      </>
                    )}
                  </button>
                </div>
              ) : total !== null && addons.length > PAGE_SIZE ? (
                <p className="mt-4 text-center text-xs uppercase tracking-wider text-[var(--color-outline)]">
                  End of catalog
                </p>
              ) : null}
            </>
          )}
        </div>
      </div>

      {selectedAddon && panelAction ? (
        <AddonDetailPanel
          key={selectedAddon.id}
          addon={selectedAddon}
          action={panelAction}
          maxDockedWidth="calc(100% - 10rem)"
          installedDirName={installedDirFor(selectedAddon)}
          installedAt={installedAtFor(selectedAddon)}
          onClose={() => setSelectedAddon(null)}
          onInstall={() => handleInstall(selectedAddon)}
        />
      ) : null}
    </div>
  );
}

function CenteredState({
  icon,
  title,
  subtitle,
  tone,
}: {
  icon?: React.ReactNode;
  title: string;
  subtitle?: string;
  tone?: "error";
}) {
  return (
    <div className="grid h-full place-items-center rounded-lg border border-dashed border-[var(--color-border)] text-center">
      <div className="max-w-md px-6">
        {icon ? <div className="mb-3 flex justify-center">{icon}</div> : null}
        <p
          className={
            tone === "error"
              ? "text-lg font-medium text-[var(--color-error)]"
              : "text-lg font-medium text-[var(--color-on-surface-variant)]"
          }
        >
          {title}
        </p>
        {subtitle ? <p className="mt-2 text-sm text-[var(--color-outline)]">{subtitle}</p> : null}
      </div>
    </div>
  );
}
