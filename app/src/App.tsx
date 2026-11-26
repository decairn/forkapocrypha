import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Sidebar, type PageId } from "@/components/Sidebar";
import { SyncBanner, type SyncStatus } from "@/components/SyncBanner";
import { Toaster, useToasts } from "@/components/Toaster";
import { WelcomeScreen, welcomeAlreadyShown } from "@/components/WelcomeScreen";
import { SearchPage } from "@/pages/SearchPage";
import { InstalledPage } from "@/pages/InstalledPage";
import { SettingsPage } from "@/pages/SettingsPage";
import { LegacyUninstallDialog } from "@/components/LegacyUninstallDialog";
import { InstallConflictDialog } from "@/components/InstallConflictDialog";
import { api } from "@/lib/api";
import { useAppZoom } from "@/lib/useAppZoom";
import { useInstalledScan } from "@/lib/useInstalledScan";
import type { CatalogMeta, InstallConflictReview, LegacyUninstallFile, UninstallPreview } from "@/lib/types";

function App() {
  const [page, setPage] = useState<PageId>("search");
  const { zoom, changeZoom } = useAppZoom();
  const [syncStatus, setSyncStatus] = useState<SyncStatus>({ kind: "idle" });
  const [catalogRevision, setCatalogRevision] = useState(0);
  const installedScan = useInstalledScan(page === "installed", catalogRevision);
  const [showWelcome, setShowWelcome] = useState(() => !welcomeAlreadyShown());
  const { toasts, push: pushToast, dismiss: dismissToast } = useToasts();
  const [uninstallReview, setUninstallReview] = useState<{
    dirName: string;
    preview: UninstallPreview;
    resolve: (completed: boolean) => void;
  } | null>(null);
  const uninstallPending = useRef(false);
  const [installReview, setInstallReview] = useState<InstallConflictReview | null>(null);

  const requestInstall = useCallback(async (addonId: string) => {
    let reviewId: string | undefined;
    try {
      return await api.installAddon(addonId, (review) => {
        reviewId = review.id;
        setInstallReview(review);
      });
    } finally {
      setInstallReview((current) => current?.id === reviewId ? null : current);
    }
  }, []);

  const requestUninstall = useCallback(async (dirName: string): Promise<boolean> => {
    if (uninstallPending.current) {
      pushToast({ tone: "info", title: "Finish the current uninstall first" });
      return false;
    }
    uninstallPending.current = true;
    try {
      let preview: UninstallPreview;
      try {
        preview = await api.previewUninstall(dirName);
      } catch (error) {
        pushToast({ tone: "error", title: `Couldn't review ${dirName}`, body: String(error) });
        return false;
      }
      if (!preview.legacy) {
        try {
          await api.uninstallAddon(dirName);
          return true;
        } catch {
          return false;
        }
      }
      if (preview.files.length === 0) {
        pushToast({ tone: "info", title: `No removable files in ${dirName}` });
        return false;
      }
      return await new Promise<boolean>((resolve) => setUninstallReview({ dirName, preview, resolve }));
    } finally {
      uninstallPending.current = false;
    }
  }, [pushToast]);

  useEffect(() => {
    const subscription = listen<{ id: string; error: string; addon_title?: string }>(
      "addon:install:error",
      ({ payload }) => {
        pushToast({
          tone: "error",
          title: payload.addon_title
            ? `Auto-update failed for ${payload.addon_title}`
            : "Install failed",
          body: payload.error,
        });
      },
    );

    return () => {
      // The listener may finish registering after cleanup has started.
      void subscription.then((unlisten) => unlisten());
    };
  }, [pushToast]);

  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    let doneTimer: ReturnType<typeof setTimeout> | undefined;
    let errorTimer: ReturnType<typeof setTimeout> | undefined;

    (async () => {
      unlisteners.push(
        await listen("catalog:sync:start", () => {
          if (doneTimer) clearTimeout(doneTimer);
          if (errorTimer) clearTimeout(errorTimer);
          setSyncStatus({ kind: "syncing" });
        }),
      );
      unlisteners.push(
        await listen<CatalogMeta & { revision: number }>("catalog:sync:done", (e) => {
          setSyncStatus({ kind: "done", addonCount: e.payload.addon_count });
          setCatalogRevision((revision) => Math.max(revision, e.payload.revision));
          doneTimer = setTimeout(() => setSyncStatus({ kind: "idle" }), 4000);
        }),
      );
      unlisteners.push(
        await listen<string>("catalog:sync:error", (e) => {
          setSyncStatus({ kind: "error", message: e.payload });
          errorTimer = setTimeout(() => setSyncStatus({ kind: "idle" }), 6000);
        }),
      );

      unlisteners.push(
        await listen<{ id: string; path: string }>("addon:install:backup", (e) => {
          pushToast({
            tone: "info",
            title: "Original conflict files backed up",
            body: e.payload.path,
          });
        }),
      );
      unlisteners.push(
        await listen<{
          dir_name: string;
          preserved_files: boolean;
          backup_path: string | null;
          directory_cleanup_warning: string | null;
        }>("addon:uninstall:done", (e) => {
          if (e.payload.backup_path) {
            pushToast({
              tone: "info",
              title: "Removed files backed up",
              body: e.payload.backup_path,
            });
          }
          if (e.payload.preserved_files) {
            pushToast({
              tone: "info",
              title: `Some files remain in ${e.payload.dir_name}`,
              body: "Files that are untracked or belong to another addon were preserved.",
            });
          }
          if (e.payload.directory_cleanup_warning) {
            pushToast({
              tone: "info",
              title: "Addon files removed. A folder could not be cleaned up.",
              body: e.payload.directory_cleanup_warning,
            });
          }
        }),
      );
      unlisteners.push(
        await listen<{ dir_name: string; error: string }>("addon:uninstall:error", (e) => {
          pushToast({
            tone: "error",
            title: `Couldn't uninstall ${e.payload.dir_name}`,
            body: e.payload.error,
          });
        }),
      );
    })();

    return () => {
      unlisteners.forEach((fn) => fn());
      if (doneTimer) clearTimeout(doneTimer);
      if (errorTimer) clearTimeout(errorTimer);
    };
  }, [pushToast]);

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-[var(--color-surface)] text-[var(--color-on-surface)]">
      <Sidebar current={page} onNavigate={setPage} />
      <main className="relative flex flex-1 flex-col overflow-hidden bg-[var(--color-surface)]">
        <SyncBanner status={syncStatus} />
        <div className="flex-1 overflow-hidden">
          {page === "search" && <SearchPage key={catalogRevision} onInstall={requestInstall} onUninstall={requestUninstall} />}
          {page === "installed" && (
            <InstalledPage scan={installedScan} onInstall={requestInstall} onUninstall={requestUninstall} />
          )}
          {page === "settings" && <SettingsPage key={catalogRevision} zoom={zoom} onZoomChange={changeZoom} />}
        </div>
      </main>
      <Toaster toasts={toasts} onDismiss={dismissToast} />
      {uninstallReview && (
        <LegacyUninstallDialog
          dirName={uninstallReview.dirName}
          preview={uninstallReview.preview}
          onCancel={() => {
            uninstallReview.resolve(false);
            setUninstallReview(null);
          }}
          onConfirm={async (files: LegacyUninstallFile[]) => {
            try {
              await api.uninstallLegacy(uninstallReview.dirName, uninstallReview.preview.addons_dir, files);
              uninstallReview.resolve(true);
            } catch {
              uninstallReview.resolve(false);
            } finally {
              setUninstallReview(null);
            }
          }}
        />
      )}
      {installReview && (
        <InstallConflictDialog
          key={installReview.id}
          review={installReview}
          onDecision={async (approved) => {
            await api.resolveInstallConflict(installReview.id, approved);
            setInstallReview((current) => current?.id === installReview.id ? null : current);
          }}
        />
      )}
      {showWelcome ? <WelcomeScreen onDismiss={() => setShowWelcome(false)} /> : null}
    </div>
  );
}

export default App;
