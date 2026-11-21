<div align="center">

# Apocrypha

**A modern addon manager for The Elder Scrolls Online.**

Fast. Lightweight. Beautifully dark.

[**Download**](https://github.com/VitorTaichou/apocrypha/releases/latest) ·
[Report a bug](https://github.com/VitorTaichou/apocrypha/issues) ·
[Discussions](https://github.com/VitorTaichou/apocrypha/discussions)

[![build](https://github.com/VitorTaichou/apocrypha/actions/workflows/build.yml/badge.svg)](https://github.com/VitorTaichou/apocrypha/actions/workflows/build.yml)
[![release](https://img.shields.io/github/v/release/VitorTaichou/apocrypha?label=release)](https://github.com/VitorTaichou/apocrypha/releases/latest)
[![platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20Linux-67D9CA)](https://github.com/VitorTaichou/apocrypha/releases/latest)

</div>

---

## Install

### Windows

1. Download **`Apocrypha_X.Y.Z_x64_en-US.msi`** from the [latest release](https://github.com/VitorTaichou/apocrypha/releases/latest).
2. Double-click to install.
3. On first launch, Windows SmartScreen shows **"Windows protected your PC"** — click **More info** → **Run anyway**.
   - The build isn't code-signed yet (signing certificates cost money; this is a hobby project). It's not a virus warning — the source is right here for anyone who wants to read it.
   - After accepting once, Windows trusts the app and stops prompting.

### Linux

1. Download **`Apocrypha_X.Y.Z_amd64.AppImage`** from the [latest release](https://github.com/VitorTaichou/apocrypha/releases/latest).
2. Make it executable and run:

   ```bash
   chmod +x Apocrypha_*.AppImage
   ./Apocrypha_*.AppImage
   ```

### Self-update

After the first install, Apocrypha can update itself: open **Settings → App Updates → Check now**. Future versions ship via the same channel, no need to come back here.

---

## What it does

- **Browse the full ESOUI catalog** (~6,000 addons) with live search, sort, category filter, and pagination
- **One-click install** with **recursive dependency resolution** — LibStub, LibAddonMenu-2.0, LibCustomMenu, LibAsync and friends are pulled in automatically without asking
- Required dependency minimums are checked against the installed manifest's `AddOnVersion`. If a dependency is too old or its version cannot be verified, installation reports an error and may be partial; update the dependency and retry.
- **Update, uninstall, and bulk Update All** — re-uses the same staged install pipeline, including manifest renames (`.txt` → `.addon`)
- **Background auto-update** of addons while ESO is closed (off by default; toggle in Settings)
- **SavedVariables snapshots** — back up your configs before a risky update, restore in one click
- **System tray + autostart** — close to tray, launch with Windows, catalog stays warm
- **Detail panel** with description, changelog, screenshot gallery, and a fullscreen lightbox
- **Self-updating app** via the built-in Tauri Updater
- **OneDrive-aware** — automatically finds your AddOns folder even when Documents is redirected

### Install conflicts and migration

Apocrypha records each installed file's owner, filesystem identity, and content
hash in `AddOns/.apocrypha-ownership.json`. Files owned by another addon are
never overwritten. Manual updates that encounter untracked or modified files
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
to review newly created files as well as restoring backups. Dependency errors can still
leave earlier addon installations in place. Do not modify addons with another
program while an install is running.

---

## Why this exists

This is a personal project — I wanted something native, lightweight, and tray-friendly for managing my own ESO addons, and figured it might be useful to other people too.

A few things Apocrypha tries to do well:

- **Native and small** — a Rust + Tauri binary around ~10 MB, cold start in well under a second.
- **Background-friendly** — lives in the system tray, can auto-update addons while ESO is closed, skips work while the game is running.
- **Safer updates** — SavedVariables snapshots before risky updates; archive staging and rollback protect existing files, including files in shared directories.
- **Open source** — the source is right here, no telemetry, no accounts.

---

## Tech stack

- **[Tauri 2](https://tauri.app/)** + **Rust** — native window, signed bundles, ~10 MB binary
- **React 19** + **TypeScript** + **[Vite+](https://viteplus.dev/)** — UI
- **Tailwind 4** + shadcn/ui patterns + **Lucide** icons — design system
- **SQLite** (via `rusqlite`) — local catalog mirror
- **reqwest** + **tokio** — async HTTP and background tasks
- **sysinfo** — process detection for the "skip while ESO runs" logic

Catalog data comes from the official [ESOUI](https://www.esoui.com/) site via the MMOUI v3 API and is mirrored to a local SQLite database that auto-syncs on every launch.

---

## Building from source

Use Node.js 22.18 or later in the 22.x line, 24.11 or later in 24.x,
or 26 or later.

```bash
git clone https://github.com/VitorTaichou/apocrypha
cd apocrypha/app
npm install
npm run tauri dev
```

The first run compiles ~500 Rust crates (Tauri, wry, tao, webview2-com, …) — expect **~3–4 minutes**. Subsequent runs are seconds.

Production build:

```bash
npm run tauri build
```

Outputs land under `app/src-tauri/target/release/bundle/` (MSI + NSIS on Windows, AppImage + deb on Linux).

For the full architecture overview — module layout, IPC contracts, design tokens, signing/release procedure, and platform gotchas — see [CLAUDE.md](CLAUDE.md).

---

## Roadmap

- [x] Catalog browse + search + sort + category filter + pagination
- [x] Install / Update / Uninstall with recursive dependency resolution
- [x] Bulk Update All
- [x] Detail panel with screenshot gallery + lightbox
- [x] SavedVariables snapshots (manual)
- [x] System tray + autostart + close-to-tray
- [x] Background auto-update of addons (ESO-aware)
- [x] Self-updating app (Tauri Updater)
- [x] Welcome screen + native folder picker
- [ ] **Code-signed Windows MSI** (kill the SmartScreen prompt)
- [ ] **winget** submission
- [ ] **Flathub** submission
- [ ] Conflict / health detector (orphan dependencies, known incompatibilities)
- [ ] Import existing libraries and SavedVars from other managers

---

## Contributing

PRs welcome. Development happens on **`dev`**; **`main`** is the release branch (tagged commits cut releases via the workflow at `.github/workflows/release.yml`). See [CLAUDE.md](CLAUDE.md) > Branching & releases for the full flow.

If you find an addon that doesn't install or update correctly, [open an issue](https://github.com/VitorTaichou/apocrypha/issues/new) with the addon name and what you saw — extra points for a screenshot or DevTools console snippet.

---

## License

To be finalized once a public 1.0 release ships. Source is open for inspection and personal use in the meantime.

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
