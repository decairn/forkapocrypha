import { useEffect, useRef, useState } from "react";
import { AlertTriangle } from "lucide-react";
import type { InstallConflictReview } from "@/lib/types";

export function InstallConflictDialog({
  review,
  onDecision,
}: {
  review: InstallConflictReview;
  onDecision: (approved: boolean) => Promise<void>;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const cancelButton = useRef<HTMLButtonElement>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const remaining = review.file_count - review.files.length;

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    cancelButton.current?.focus();
    return () => element?.close();
  }, []);

  async function decide(approved: boolean) {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await onDecision(approved);
    } catch (error) {
      setError(String(error));
      setBusy(false);
    }
  }

  return (
    <dialog
      ref={dialog}
      aria-labelledby="install-conflict-title"
      aria-describedby="install-conflict-summary"
      className="m-auto w-[calc(100%-2rem)] max-w-2xl overflow-hidden rounded-lg border border-outline-variant bg-surface text-sm text-on-surface shadow-2xl backdrop:bg-black/75"
      onCancel={(event) => {
        event.preventDefault();
        void decide(false);
      }}
    >
      <div className="flex max-h-[85dvh] flex-col">
        <header className="flex items-center gap-3 border-b border-border px-5 py-4">
          <AlertTriangle aria-hidden="true" className="size-5 shrink-0 text-error" />
          <h2 id="install-conflict-title" className="text-lg font-semibold">
            Review addon file conflicts
          </h2>
        </header>
        <div className="min-h-0 space-y-4 overflow-y-auto px-5 py-4">
          <p id="install-conflict-summary">
            This change would replace or remove {review.file_count} existing untracked or modified{" "}
            {review.file_count === 1 ? "file" : "files"}. These files may belong to another addon.
          </p>
          <ul
            aria-label="Conflicting files"
            tabIndex={0}
            className="max-h-60 divide-y divide-border overflow-y-auto rounded-lg border border-border bg-surface-lowest"
          >
            {review.files.map((file) => (
              <li key={file.path} className="flex items-baseline gap-3 px-3 py-2">
                <span className="shrink-0 text-xs font-medium text-on-surface-variant">{file.operation}</span>
                <div className="min-w-0">
                  <div className="break-all font-mono text-xs">{file.path}</div>
                  <div className="text-xs text-on-surface-variant">{file.reason}</div>
                </div>
              </li>
            ))}
          </ul>
          {remaining > 0 && (
            <p className="text-on-surface-variant">
              See <code>conflicts.txt</code> for the remaining {remaining} {remaining === 1 ? "file" : "files"}.
            </p>
          )}
          <div>
            <p className="font-medium">Backup location</p>
            <p className="mt-1 select-text break-all font-mono text-xs text-on-surface-variant">
              {review.backup_path}
            </p>
          </div>
          <p className="text-on-surface-variant">
            Review <code>conflicts.txt</code> in the backup folder before continuing. Apocrypha will
            record replacement files as belonging to this addon and may remove them when you
            uninstall it. The originals will be kept for manual recovery.
          </p>
          {error && <p role="alert" className="text-error">{error}</p>}
        </div>
        <footer className="flex shrink-0 flex-wrap justify-end gap-3 border-t border-border px-5 py-4">
          <button
            ref={cancelButton}
            type="button"
            disabled={busy}
            onClick={() => void decide(false)}
            className="rounded-lg border border-outline-variant px-4 py-2 hover:bg-surface-high disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={() => void decide(true)}
            className="rounded-lg bg-primary px-4 py-2 font-medium text-on-primary hover:bg-primary-dim disabled:opacity-50"
          >
            Continue with backup
          </button>
        </footer>
      </div>
    </dialog>
  );
}
