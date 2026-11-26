import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "@/lib/api";
import type { InstalledAddon } from "@/lib/types";

export function useInstalledScan(isActive: boolean, catalogRevision: number) {
  const [addons, setAddons] = useState<InstalledAddon[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [rescanPhase, setRescanPhase] = useState<"refreshing" | "scanning" | null>(null);
  const [catalogRefreshFailed, setCatalogRefreshFailed] = useState(false);
  const latestScan = useRef(0);
  const handledRevision = useRef(0);
  const wasActive = useRef(false);
  const rescanPending = useRef(false);

  const reload = useCallback(async () => {
    const scan = ++latestScan.current;
    setLoading(true);
    setError(null);
    try {
      const installed = await api.listInstalled();
      if (scan === latestScan.current) setAddons(installed);
    } catch (error) {
      if (scan === latestScan.current) setError(String(error));
    } finally {
      if (scan === latestScan.current) setLoading(false);
    }
  }, []);

  const rescan = useCallback(async () => {
    if (rescanPending.current) return;
    rescanPending.current = true;
    setRescanPhase("refreshing");
    setCatalogRefreshFailed(false);
    try {
      const revision = await api.refreshCatalogForRescan();
      // The next scan covers this completion, including a sync we joined.
      handledRevision.current = Math.max(handledRevision.current, revision);
    } catch (error) {
      console.warn("Could not refresh the ESOUI catalog:", error);
      setCatalogRefreshFailed(true);
    }
    setRescanPhase("scanning");
    await reload();
    rescanPending.current = false;
    setRescanPhase(null);
  }, [reload]);

  useEffect(() => {
    const entered = isActive && !wasActive.current;
    wasActive.current = isActive;
    // A pending re-scan also serves a page reopened during either phase.
    // Check the latest catalog revision again when it finishes.
    if (!isActive || rescanPending.current) return;
    if (!entered && catalogRevision <= handledRevision.current) return;
    handledRevision.current = Math.max(handledRevision.current, catalogRevision);
    setCatalogRefreshFailed(false);
    void reload();
  }, [isActive, catalogRevision, rescanPhase, reload]);

  return {
    addons,
    loading: loading || rescanPhase === "scanning",
    error,
    refreshingCatalog: rescanPhase === "refreshing",
    catalogRefreshFailed,
    reload,
    rescan,
  };
}
