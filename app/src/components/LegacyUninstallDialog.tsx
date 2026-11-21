import { useEffect, useRef, useState } from "react";
import type { LegacyUninstallFile, UninstallPreview } from "@/lib/types";

export function LegacyUninstallDialog({
  dirName,
  preview,
  onCancel,
  onConfirm,
}: {
  dirName: string;
  preview: UninstallPreview;
  onCancel: () => void;
  onConfirm: (files: LegacyUninstallFile[]) => Promise<void>;
}) {
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const dialog = useRef<HTMLDialogElement>(null);
  const missingManifest = preview.files.some((file) => file.manifest && !selected.has(file.path));
  const chosen = preview.files.filter((file) => selected.has(file.path));

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, []);

  async function confirm() {
    setBusy(true);
    try {
      await onConfirm(chosen);
    } finally {
      setBusy(false);
    }
  }

  return (
    <dialog
      ref={dialog}
      aria-labelledby="legacy-uninstall-title"
      className="m-auto w-[calc(100%-3rem)] max-w-2xl bg-transparent p-0 backdrop:bg-black/75"
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onCancel();
      }}
    >
      <div className="flex max-h-[85vh] w-full flex-col gap-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] p-5 text-sm text-[var(--color-on-surface)]">
        <h2 id="legacy-uninstall-title" className="text-lg font-semibold">
          Review files in {dirName}
        </h2>
        <p>
          These files have no verified owner and may belong to another addon. Select only files
          you want to remove; unselected files and tracked files are preserved. Files outside this
          folder are not removed, and remaining files may keep the folder listed in Apocrypha.
        </p>
        <p className="text-[var(--color-on-surface-variant)]">
          Selected files are backed up under {preview.backup_dir} before removal. To restore them,
          close ESO and Apocrypha and copy the originals from the backup to their listed paths.
          {preview.protected_count > 0 && ` ${preview.protected_count} tracked or unsafe item(s) are protected and not listed.`}
        </p>
        <div className="flex gap-4">
          <button type="button" className="text-[var(--color-primary)]" disabled={busy}
            onClick={() => setSelected(new Set(preview.files.map((file) => file.path)))}>
            Select all listed
          </button>
          <button type="button" className="text-[var(--color-primary)]" disabled={busy}
            onClick={() => setSelected(new Set())}>
            Clear selection
          </button>
        </div>
        <div className="min-h-0 overflow-y-auto rounded border border-[var(--color-border)] p-2">
          {preview.files.length === 0 && <p>No untracked regular files can be removed.</p>}
          {preview.files.map((file) => (
            <label key={file.path} className="flex items-start gap-2 break-all p-1">
              <input
                type="checkbox"
                checked={selected.has(file.path)}
                disabled={busy}
                onChange={(event) => {
                  const checked = event.target.checked;
                  setSelected((previous) => {
                    const next = new Set(previous);
                    if (checked) next.add(file.path);
                    else next.delete(file.path);
                    return next;
                  });
                }}
              />
              <span>{file.path}{file.manifest ? " (manifest)" : ""}</span>
            </label>
          ))}
        </div>
        {missingManifest && <p>Select every listed manifest to disable this addon.</p>}
        <div className="flex justify-end gap-3">
          <button type="button" autoFocus disabled={busy} onClick={onCancel} className="rounded border border-[var(--color-border)] px-4 py-2">
            Cancel
          </button>
          <button type="button" disabled={busy || chosen.length === 0 || missingManifest} onClick={confirm}
            className="rounded bg-[var(--color-primary)] px-4 py-2 text-[var(--color-surface)] disabled:opacity-40">
            Back up and remove {chosen.length} file{chosen.length === 1 ? "" : "s"}
          </button>
        </div>
      </div>
    </dialog>
  );
}
