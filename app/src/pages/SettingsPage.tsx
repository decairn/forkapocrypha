import { useEffect, useState } from "react";
import { ExternalLink, FolderOpen, Loader2 } from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { AutoUpdatePanel } from "@/components/AutoUpdatePanel";
import { PageHeader } from "@/components/PageHeader";
import { SnapshotsManager } from "@/components/SnapshotsManager";
import { UpdaterPanel } from "@/components/UpdaterPanel";
import { WindowBehaviorPanel } from "@/components/WindowBehaviorPanel";
import { api } from "@/lib/api";
import type { CatalogMeta } from "@/lib/types";
import { formatRelativeTime } from "@/lib/format";
import { DEFAULT_ZOOM, MAX_ZOOM, MIN_ZOOM, type ZoomAction } from "@/lib/zoom";

function SettingSection({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: React.ReactNode;
}) {
  return (
    <section className="border-b border-[var(--color-border)] py-8">
      <div className="grid gap-4 @2xl:grid-cols-[300px_minmax(0,1fr)] @2xl:gap-8">
        <div>
          <h2 className="text-sm font-medium uppercase tracking-[0.14em] text-[var(--color-on-surface)]">
            {title}
          </h2>
          <p className="mt-2 text-sm text-[var(--color-on-surface-variant)]">{description}</p>
        </div>
        <div className="space-y-3">{children}</div>
      </div>
    </section>
  );
}

const zoomButtonClass =
  "min-h-9 min-w-9 rounded-md border border-outline-variant bg-surface-low px-3 text-sm hover:border-primary focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary aria-disabled:opacity-50";

export function SettingsPage({
  zoom,
  onZoomChange,
}: {
  zoom: number;
  onZoomChange: (action: ZoomAction) => void;
}) {
  const [addonsDir, setAddonsDir] = useState("");
  const [meta, setMeta] = useState<CatalogMeta | null>(null);
  const [syncing, setSyncing] = useState(false);
  const [syncError, setSyncError] = useState<string | null>(null);

  useEffect(() => {
    api
      .getAddonsDir()
      .then(setAddonsDir)
      .catch(() => {});
    api
      .catalogMeta()
      .then(setMeta)
      .catch(() => {});
  }, []);

  async function handleSync() {
    setSyncing(true);
    setSyncError(null);
    try {
      const m = await api.syncCatalog();
      setMeta(m);
    } catch (e) {
      setSyncError(String(e));
    } finally {
      setSyncing(false);
    }
  }

  async function handlePathChange(value: string) {
    setAddonsDir(value);
    try {
      await api.setAddonsDir(value);
    } catch {
      // silently ignore for MVP
    }
  }

  async function handleBrowse() {
    try {
      const picked = await openDialog({
        directory: true,
        multiple: false,
        title: "Select your ESO AddOns folder",
        defaultPath: addonsDir || undefined,
      });
      if (typeof picked === "string" && picked) {
        await handlePathChange(picked);
      }
    } catch (e) {
      console.error("folder picker failed:", e);
    }
  }

  return (
    <div className="flex h-full flex-col">
      <PageHeader title="Settings" subtitle="Configure paths, automation, and diagnostics." />

      <div className="@container flex-1 overflow-y-auto px-10 pb-12">
        <SettingSection title="Zoom" description="Use Ctrl + Plus (or Equals), Ctrl + Minus, or Ctrl + 0 to reset.">
          <div role="group" aria-label="Zoom controls" className="flex flex-wrap items-center gap-3">
            <button
              type="button"
              aria-label="Zoom out"
              aria-disabled={zoom === MIN_ZOOM}
              className={zoomButtonClass}
              onClick={() => onZoomChange("out")}
            >
              −
            </button>
            <output aria-label="Current zoom" className="min-w-12 text-center text-sm tabular-nums">
              {zoom}%
            </output>
            <button
              type="button"
              aria-label="Zoom in"
              aria-disabled={zoom === MAX_ZOOM}
              className={zoomButtonClass}
              onClick={() => onZoomChange("in")}
            >
              +
            </button>
            <button
              type="button"
              aria-disabled={zoom === DEFAULT_ZOOM}
              className={zoomButtonClass}
              onClick={() => onZoomChange("reset")}
            >
              Reset to 100%
            </button>
          </div>
        </SettingSection>

        <SettingSection
          title="AddOns Folder"
          description="Where your ESO addons are installed. Edit if you have a non-standard ESO install."
        >
          <div className="flex gap-2">
            <input
              value={addonsDir}
              onChange={(e) => handlePathChange(e.target.value)}
              className="min-w-0 flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-lowest)] px-4 py-2.5 font-mono text-sm text-[var(--color-on-surface)] focus:border-[var(--color-primary)] focus:outline-none focus:ring-1 focus:ring-[var(--color-primary)]"
            />
            <button
              onClick={handleBrowse}
              className="inline-flex shrink-0 items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-low)] px-4 py-2.5 text-sm text-[var(--color-on-surface-variant)] hover:border-[var(--color-primary)] hover:text-[var(--color-primary)]"
            >
              <FolderOpen className="h-4 w-4" strokeWidth={1.75} />
              Browse...
            </button>
          </div>
        </SettingSection>

        <SettingSection title="Catalog" description="Synchronization with the ESOUI archive.">
          <div className="flex items-center justify-between gap-4">
            <div className="text-sm text-[var(--color-on-surface)]">
              <div>
                {meta && meta.addon_count > 0
                  ? `${meta.addon_count.toLocaleString()} addons indexed`
                  : "Catalog is empty"}
              </div>
              <div className="mt-0.5 text-xs text-[var(--color-outline)]">
                Last sync: {formatRelativeTime(meta?.last_synced ?? null)}
              </div>
            </div>
            <button
              onClick={handleSync}
              disabled={syncing}
              className="inline-flex items-center gap-2 rounded-md bg-[var(--color-primary-container)] px-4 py-2 text-sm font-medium text-[var(--color-on-primary)] hover:bg-[var(--color-primary)] disabled:opacity-60"
            >
              {syncing ? (
                <>
                  <Loader2 className="h-4 w-4 animate-spin" strokeWidth={1.75} />
                  Syncing...
                </>
              ) : (
                "Sync now"
              )}
            </button>
          </div>
          {syncError ? <p className="text-xs text-[var(--color-error)]">{syncError}</p> : null}
        </SettingSection>

        <SettingSection
          title="SavedVariables Backups"
          description="Snapshots of your live/SavedVariables folder. Create one whenever you want a safety net — restore in one click if an update wipes your config."
        >
          <SnapshotsManager />
        </SettingSection>

        <SettingSection
          title="Auto-update"
          description="Keep addons fresh without thinking about it. Apocrypha runs the same install pipeline as a manual Update click, just on a timer and only while ESO is closed."
        >
          <AutoUpdatePanel />
        </SettingSection>

        <SettingSection
          title="Window behavior"
          description="How Apocrypha starts and what happens when you close the window."
        >
          <WindowBehaviorPanel />
        </SettingSection>

        <SettingSection
          title="App Updates"
          description="Apocrypha checks for updates from this distribution’s release feed."
        >
          <UpdaterPanel />
        </SettingSection>

        <SettingSection title="About" description="Version, links, and credits.">
          <div className="space-y-3 text-base text-[var(--color-on-surface-variant)]">
            <p>Apocrypha v{__APP_VERSION__}</p>
            <p className="text-sm text-[var(--color-outline)]">
              Built with Tauri 2, Rust, React, and Tailwind.
            </p>
            <div className="flex flex-wrap gap-2">
              {__ABOUT_LINKS__.map(({ url, label }) => (
                <button
                  key={url}
                  type="button"
                  onClick={() => openUrl(url).catch(() => {})}
                  className="inline-flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-low)] px-3.5 py-1.5 text-xs font-medium text-[var(--color-on-surface-variant)] hover:border-[var(--color-primary)] hover:text-[var(--color-primary)] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
                >
                  <ExternalLink aria-hidden="true" className="h-3.5 w-3.5" strokeWidth={1.75} />
                  {label}
                </button>
              ))}
            </div>
          </div>
        </SettingSection>
      </div>
    </div>
  );
}
