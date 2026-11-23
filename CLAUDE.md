# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Workspace layout

The Apocrypha source lives in `app/`. Tauri 2 connects the Rust backend to the
React 19 and TypeScript frontend. The frontend uses Vite+, Tailwind 4,
shadcn/ui patterns, and Lucide icons. The backend uses `rusqlite` for the
catalog, `reqwest` and `tokio` for network and background work, and `sysinfo`
to detect when ESO is running.

## Building from source

Use Node.js 22.18 or later in the 22.x line, 24.11 or later in 24.x, or 26 or
later, and stable Rust. From the repository root, change to `app/` before
running the npm commands.

```powershell
npm install
npm run tauri dev
npm run build  # Frontend build and typecheck
```

The first Windows build compiles about 300 Rust crates. Allow roughly 3–4
minutes. The Vite dev server uses port 1420 (`vite.config.ts`). Vite+ provides
the `vp` commands behind the `dev`, `build`, and `preview` scripts.

For a Rust-only check, run `cargo check --message-format=short` from
`app/src-tauri/`. For a local unsigned production build, run
`npm run tauri build -- --config src-tauri/tauri.ci.conf.json` from `app/`.
Bundles go under `app/src-tauri/target/release/bundle/` (MSI and NSIS on
Windows, AppImage and deb on Linux). See [Tauri Updater](#tauri-updater-auto-update-of-the-app-itself) for
updater-enabled builds.

## Architecture

Two halves communicating via Tauri IPC commands and broadcast events.

### Rust backend (`app/src-tauri/src/`)

- **`lib.rs`** — Tauri builder, command registration, `setup()` hook that auto-syncs the catalog on every boot.
- **`state.rs`** — `AppState` (single instance via `.manage()`): SQLite connection, MMOUI HTTP client, addons folder path, and async locks for filesystem mutations and catalog sync. Startup, Settings, timed updates, and Installed Re-scan share the catalog sync lifecycle events.
- **`api.rs`** — MMOUI v3 client. Discovers feed URLs via `globalconfig.json`. Fetches the catalog and downloads archives on demand using `fetch_addon_download_url` and `download_bytes`.
- **`db.rs`** — SQLite catalog at `%APPDATA%/Apocrypha/catalog.db`. WAL mode. Idempotent `ALTER TABLE` migrations on `addons` for new columns (currently `thumbnail_url`, `images`). `find_by_directory` is the bridge from local installed dirs back to catalog entries.
- **`scanner.rs`** — Walks `<Documents>/Elder Scrolls Online/live/AddOns/`, parses `.txt` / `.addon` manifests (`## Title`, `## Version`, `## DependsOn`, etc.). Strips ESO color codes (`|cRRGGBB...|r`) and handles lossy UTF-8.
- **`installer.rs`** — Staged install, update, and uninstall. See [installer invariants](#installer-invariants).
- **`commands.rs`** — Tauri command wrappers. Async commands emit events for the frontend: `catalog:sync:{start,done,error}`, `addon:install:{start,progress,done,error}`, `addon:uninstall:{done,error}`. Also hosts `enumerate_installed`, which resolves each on-disk addon to a catalog row (see below).
- **`models.rs`** — Shared serde types (`Addon`, `Category`, `InstalledAddon`, `CatalogMeta`, `SearchResult`). Keep mirrored in `src/lib/types.ts`.

### Installer invariants

- Hold the filesystem mutation lock across the entire install and dependency tree, including ownership updates. Resolve dependencies through `find_by_directory` with a visited `HashSet`. Enforce minimums using the installed manifest's `AddOnVersion`. Dependency failures can leave earlier installs in place.
- Manual and background updates use the same staged install pipeline. It handles manifest extension changes such as `.txt` to `.addon`.
- Track ownership, filesystem identity, and SHA-256 per file in `AddOns/.apocrypha-ownership.json`. Reject conflicts with another known owner. Manual installs may replace untracked or modified files only after explicit approval and a persistent backup. Background updates stop on these conflicts.
- Uninstall verifies ownership and file identity and content before removal. Legacy installs have unknown ownership, even in a shared root. Legacy uninstall allows explicit selection and backs up selected untracked files. It preserves tracked and unselected files and files outside the addon folder.
- A leftover `.apocrypha-install-*` directory blocks further mutations until recovery. Keep the backup and `journal.json` available for the [recovery steps](README.md#install-conflicts-and-recovery).

### Installed-row resolution (`commands.rs::enumerate_installed`)

Multiple catalog rows can declare the same directory (translation packs, patches, abandoned originals shadowed by a maintained fork). To pick the row that actually represents what's on disk:

1. **Anchor on the local manifest version.** If any catalog row sharing the directory has a version string matching `## Version:` from the on-disk manifest, use it. The manifest beats both a stale pin and the recency heuristic.
2. **Honor the explicit pin.** `installed_dir:{dir} → catalog_id` is written at install time; respect it when the manifest can't anchor.
3. **Fork-successor fallback.** No pin, no manifest match: pick the row with the most recent `last_updated` (covers manual / legacy installs in contested dirs).

When manifest resolution picks a row that disagrees with the existing pin, the corrected pin is persisted in the same pass so subsequent install / uninstall / update commands target the right catalog id.

### Update detection (same function)

For an unchanged, tracked manifest, compare the current listing's exact `UIVersion` and `last_updated` with the values recorded for the installed package in `.apocrypha-ownership.json`. A listing version change at the same timestamp or a re-publish with the same version both offer updates.

On upgrade, the catalog database saves the cached listing version for legacy install markers that match the cached timestamp. This baseline also requires an unchanged, tracked manifest. Without a verified package record, update detection falls back to comparing the local manifest version and using legacy timestamps where available.

### React frontend (`app/src/`)

- **`App.tsx`** — Root layout. Listens to `catalog:sync:*`, renders `SyncBanner`, increments `catalogTick` on `done` to force-remount pages with fresh data.
- **`pages/{SearchPage,InstalledPage,SettingsPage}.tsx`** — Each fetches its own data and subscribes to relevant events for cross-page sync (e.g. SearchPage refreshes `installedDirs` when `addon:install:done` fires; InstalledPage reloads on the same event).
- **`components/`** — `Sidebar`, `PageHeader`, `AddonRow`, `InstalledRow`, `CategoryIcon`, `CategorySelect`, `SyncBanner`.
- **`lib/api.ts`** — typed `invoke()` wrappers. One function per Tauri command.
- **`lib/types.ts`** — TS mirrors of Rust models. **Update both sides** when adding fields.
- **`lib/categoryMeta.ts`** — Hardcoded `{ icon, color, title, description }` per MMOUI category ID (~39 entries). When MMOUI introduces a new category, add it here (otherwise it falls back to a generic icon).
- **`lib/format.ts`** — `formatCount`, `formatRelativeTime`.
- **`index.css`** — Tailwind 4 + design tokens as `@theme` CSS variables. Imports Newsreader + Geist from Google Fonts.

## Catalog data flow

1. App launches → `setup()` hook spawns async task → `run_sync()` emits `catalog:sync:start`.
2. `MmouiClient` fetches `filelist.json` + `categorylist.json` (whole catalog at once — the API does **not** support pagination or server-side search).
3. Bulk upsert into SQLite via transactions.
4. Emits `catalog:sync:done` with `CatalogMeta`.
5. `App.tsx` bumps `catalogTick` → pages remount and re-fetch from SQLite.

Startup still syncs on every boot. Installed Re-scan uses `metadata.last_synced` (RFC3339) to reuse a catalog up to five minutes old. If the catalog is stale, Re-scan starts or waits for a refresh with a ten-second limit on network work or waiting. The local scan still runs if the refresh fails.

## Design system

`app/src/index.css` is canonical. The `@theme` block defines every design
token Tailwind picks up. Key facts:

- Dark teal-on-near-black ("Techno-Occult Productivity")
- **Newsreader** (serif) for the wordmark, **Geist** (sans) for body — both via Google Fonts
- Primary `#67D9CA`, surface `#101413`, border `#1A2422`

Design rule: Newsreader is reserved for the "APOCRYPHA" wordmark in the sidebar. Everything else (page titles like "Search", "Installed", "Settings", and all body text) uses Geist.

## Important gotchas

- **PowerShell + npm**: On a default Windows install, the bare `npm` command resolves to `npm.ps1` (Node 24+) which is blocked by the `Restricted` ExecutionPolicy. Fix: `Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser`. If `npm --version` returns silently in PowerShell, that's the cause.
- **OneDrive Documents redirect**: On Windows with OneDrive enabled, the "Documents" folder is redirected under `~/OneDrive/...` and may be localized depending on Windows display language. `dirs::document_dir()` respects this via the Windows Known Folder API, so the scanner finds the right path automatically. If it doesn't, the override is in Settings → AddOns Folder.
- **AddOns folder override** is applied under the filesystem mutation lock and persisted by `set_addons_dir`.
- **PowerShell wraps cargo stderr**: `cargo` writes progress to stderr, and PowerShell tags it as `NativeCommandError` even on success. Look at the "Finished" line and exit code, not the stderr wrapping.
- **PATH refresh**: After `winget install`-ing new tools, the current shell session has a stale PATH. Refresh inline: `$env:Path = [Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User')`. Most PowerShell commands in this workspace prefix this.
- **Manifest and catalog versions differ regularly**. The author's `## Version:` inside `Foo.txt`/`Foo.addon` is independent of MMOUI's `UIVersion`. Fallback comparisons normalize separators and prefixes while preserving the digits in each component.
- **Bundle size**: production build is ~265 KB JS, ~20 KB CSS, ~10 MB Rust binary. Stay frugal with new lucide icons (~1 KB each, but they add up if imported broadly).

## Commits

Project convention:
- Commit messages in **English**, natural tone (no Conventional Commits ceremony)
- **No Co-Authored-By trailers** in commit messages

## Publishing from a fork

The release workflow uses `GITHUB_REPOSITORY` for the app update feed and the
first link in Settings → About. Local builds use the GitHub `origin` remote,
with the original project as the default for source archives. Set
`GITHUB_REPOSITORY` explicitly when building from a checkout without a
GitHub `origin`.

Set the fork's GitHub Actions repository variable `TAURI_SIGNING_PUBLIC_KEY`
to the contents of its Tauri `.key.pub` file. Set the matching private key
and its password as `TAURI_SIGNING_PRIVATE_KEY` and
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` secrets. The original repository can
use the public key already in `tauri.conf.json`. For a local unsigned
installer, run `npm run tauri build -- --config src-tauri/tauri.ci.conf.json`.

## Branching & releases

- **`main`** — release branch. Tagged commits cut from here become public releases.
- **`dev`** — active development. Day-to-day commits go here.
- Feature branches off `dev` when something needs isolation. Otherwise just push to `dev`.
- `.github/workflows/build.yml` — runs on every push to `dev` or manually. Sanity-builds MSI/AppImage and uploads them as workflow artifacts (for testing, not as a release).
- `.github/workflows/release.yml` — fires on a `v*` tag push (or via workflow_dispatch). Uses `tauri-apps/tauri-action` to build, sign with the keypair stored in repo secrets, and create a **draft** GitHub Release with the MSI, NSIS, AppImage, `latest.json`, and signature files attached. Edit the draft on GitHub and publish when ready.

### Release flow

```powershell
# Develop on dev
git checkout dev
# ...commit work...

# When ready to ship:
git checkout main
git merge dev --no-ff -m "Merge dev for vX.Y.Z"

# Bump version in three places (must match):
#   app/package.json
#   app/src-tauri/Cargo.toml
#   app/src-tauri/tauri.conf.json

git commit -am "Bump version to X.Y.Z"
git push origin main

git tag vX.Y.Z
git push origin vX.Y.Z   # triggers release.yml
```

After the workflow finishes, a draft release with all assets is waiting on
GitHub. Open it, polish the notes, hit Publish.

## Distribution & release (not wired yet)

These pieces have been left as TODO because they need external credentials
or a design pass. Wire them up before shipping a public, polished build.

### App icon

`app/src-tauri/icons/source.svg` is the design source. The PNG/ICO/ICNS
variants currently in `icons/` are the Tauri default wave icons; replacing
them needs:

1. Export `source.svg` to a 1024×1024 PNG (Inkscape, Figma, etc).
2. From `app/`: `npx @tauri-apps/cli icon path/to/source.png` — writes all
   the platform variants in place.

Document for `icons/README.md` describes this.

### GitHub Actions CI

`.github/workflows/build.yml` builds MSI on `windows-latest` and
AppImage on `ubuntu-latest` on pushes to `dev` or manual dispatch, with the
artifacts uploaded. No secrets are required for the basic unsigned build.

### Code signing (Windows MSI)

Without signing, the MSI triggers Windows SmartScreen on first launch. To
fix it cleanly:

- **Azure Trusted Signing** is the cheapest legit option (~$10/month) and
  works with GitHub Actions via Microsoft's official action
  (`azure/trusted-signing-action`).
- Add secrets to the GitHub repo:
  `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`,
  `TRUSTED_SIGNING_ACCOUNT`, `TRUSTED_SIGNING_CERTIFICATE_PROFILE`.
- In `app/src-tauri/tauri.conf.json` under `bundle.windows`, add the
  signing config (the Tauri docs have the exact JSON).
- Add a post-build step to the workflow that calls the Trusted Signing
  action against the `.msi` output before uploading.

EV cert from Sectigo/DigiCert is the alternative (~$300/year) — same
endgame, just dearer and requires a hardware token or HSM.

### Tauri Updater (auto-update of the app itself)

Use `npm run tauri` so `app/scripts/tauri.mjs` adds the distribution's
updater configuration to the Tauri build. Vite uses the same distribution
configuration for Settings → About. The release workflow signs updater
artifacts and attaches `latest.json` to the draft release. See
[Publishing from a fork](#publishing-from-a-fork) for signing setup.

Code signing the MSI itself (the SmartScreen warning) is a separate
concern — that's the Azure Trusted Signing path above and is unrelated
to Tauri Updater.
