<div align="center">

# Apocrypha

**A modern addon manager for The Elder Scrolls Online.**

Fast. Lightweight. Beautifully dark.

[Download](https://github.com/decairn/forkapocrypha/releases/latest) · [Report a bug](https://github.com/decairn/forkapocrypha/issues)

[![build](https://github.com/decairn/forkapocrypha/actions/workflows/build.yml/badge.svg?branch=dev)](https://github.com/decairn/forkapocrypha/actions/workflows/build.yml)
[![release](https://img.shields.io/github/v/release/decairn/forkapocrypha?label=release)](https://github.com/decairn/forkapocrypha/releases/latest)
[![platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20Linux-67D9CA)](https://github.com/decairn/forkapocrypha/releases/latest)

</div>

---

## What it does

Apocrypha is a lightweight desktop addon manager for **The Elder Scrolls Online** on Windows and Linux. It runs without an account and collects no telemetry.

- **Browse the ESOUI catalog** of about 3,400 addons with search, sorting, and category filters. Open an addon's details to see its description, changelog, and fullscreen screenshots.
- **Install addons with their required libraries**, uninstall them, or update them all at once. If an installed library is too old or its version cannot be verified, update the library and retry. Some addons may already have been installed.
- **Re-scan installed addons** to check for updates.
- **Update addons in the background** while ESO is closed. This is off by default. You can enable it in Settings, keep Apocrypha in the system tray, and launch it with Windows.
- **Back up addon settings** with SavedVariables snapshots. Restore them or open the backup folder from Settings.
- **Find your AddOns folder automatically**, including when OneDrive manages Documents. You can also choose the folder in Settings.

## Install

### Windows

1. Download **`Apocrypha_X.Y.Z_x64_en-US.msi`** from the [latest release](https://github.com/decairn/forkapocrypha/releases/latest).
2. Double-click to install.
3. The installer is unsigned. If Windows SmartScreen shows **"Windows protected your PC"**, click **More info** → **Run anyway**.

### Linux

1. Download **`Apocrypha_X.Y.Z_amd64.AppImage`** from the [latest release](https://github.com/decairn/forkapocrypha/releases/latest).
2. Make it executable and run:

   ```bash
   chmod +x Apocrypha_*.AppImage
   ./Apocrypha_*.AppImage
   ```

### Updating Apocrypha

The v1.0.0 installers require manual updates. To upgrade, download and install the [latest release](https://github.com/decairn/forkapocrypha/releases/latest).

---

## Install conflicts and recovery

Apocrypha tracks which addon owns each file and protects files owned by other
addons. Manual updates that encounter untracked or modified files
show a confirmation with the affected paths and a backup location. Review the
complete `conflicts.txt` at that location before accepting: untracked files can
belong to another addon, and accepting may affect it. Cancelling leaves the
files untouched. Background updates never approve conflicts; update manually
to review them.

Existing manual installs and installs from older versions have no ownership
record. Confirming a manual update backs up and adopts only conflicting files
that the new archive replaces, plus an obsolete conflicting manifest if needed;
other untracked files remain untouched. Backups are retained under Apocrypha's
`backups/addon-file-conflicts` directory and are not SavedVariables snapshots.
To recover originals, close ESO and Apocrypha, back up current files, and copy
the desired originals from the backup's `files` directory to the corresponding
AddOns paths. If restoring a manifest, remove the newer alternate extension so
the game cannot load both. This is a backup of conflicting files, not a full
addon rollback; restored files require conflict resolution on the next update.
Copying or recreating tracked files also requires conflict resolution.

Uninstall removes only unchanged files recorded as belonging to the selected
addon, including its owned files in shared roots; other files are preserved.
For a legacy addon without a tracked manifest, uninstall shows an unselected
per-file list for that folder. Select only files you know you want removed;
they may belong to another addon. Tracked files, links, unselected files, and
files outside that folder are preserved, and all existing manifests must be
selected to disable the addon. A backup of selected files is kept in
`backups/addon-file-conflicts` with a `conflicts.txt` restoration guide; the
exact path is shown after removal. Close ESO and Apocrypha before copying
originals from its `files` directory back to the listed AddOns paths. Remaining
files may leave the folder visible in Apocrypha. Modified tracked files block
the normal uninstall until resolved.

If an interrupted install or failed rollback leaves a hidden
`.apocrypha-install-*` directory, recover its `backup` files first, then move
that directory outside AddOns before retrying. Its `journal.json` lists affected
paths, whether a new file was planned, and whether an old file existed; use it
to review newly created files as well as restoring backups. Wait until the
install finishes before modifying addons with another program.

---

## Why this exists

This is a personal project — I wanted something native, lightweight, and tray-friendly for managing my own ESO addons, and figured it might be useful to other people too.

---

## Roadmap

- [ ] **Code-signed Windows MSI**
- [ ] **winget** submission
- [ ] **Flathub** submission
- [ ] Conflict / health detector (orphan dependencies, known incompatibilities)
- [ ] Import existing libraries and SavedVars from other managers

---

## Contributing

Pull requests are welcome. See the [source build instructions](CLAUDE.md#building-from-source) and [branching and release workflow](CLAUDE.md#branching--releases).

If you find an addon that doesn't install or update correctly, [open an issue](https://github.com/decairn/forkapocrypha/issues/new) with the addon name and what you saw — extra points for a screenshot or DevTools console snippet.

---

## License

The license is pending. Source is available for inspection and personal use.

---

## Credits

- **Catalog data** — [ESOUI](https://www.esoui.com/) and the MMOUI v3 API
- **Tentacle brand artwork** — by a friend (the chest with tentacles)
- **Inspiration & prior art** — [arviceblot/eso-addons](https://github.com/arviceblot/eso-addons) and [brainsnorkel/eso-addon-manager](https://github.com/brainsnorkel/eso-addon-manager)
- **Stack** — built with [Tauri](https://tauri.app/), [React](https://react.dev/), [Tailwind](https://tailwindcss.com/), and the rest of the open-source ecosystem

---

<div align="center">

> *"Knowledge cannot be destroyed. It can only be hidden."* — Hermaeus Mora

</div>
