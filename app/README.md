# Apocrypha — `app/`

This is the Tauri 2 + Rust + React source tree for the Apocrypha addon
manager. The user-facing documentation, downloads, install instructions
and screenshots live in the repo root [README.md](../README.md). The
architectural deep-dive lives in [CLAUDE.md](../CLAUDE.md).
Vite+ provides the frontend dev and build toolchain.
Use Node.js 22.18 or later in the 22.x line, 24.11 or later in 24.x,
or 26 or later; CI uses Node.js 24.

## Quick dev

```powershell
npm install
npm run tauri dev
```

First run compiles ~500 Rust crates (3–4 minutes); subsequent runs are
fast.

## Production build

```powershell
npm run tauri build
```

Outputs:

- Windows: `src-tauri/target/release/bundle/msi/*.msi` and
  `src-tauri/target/release/bundle/nsis/*.exe`
- Linux: `src-tauri/target/release/bundle/appimage/*.AppImage` and
  `src-tauri/target/release/bundle/deb/*.deb`

## Layout

```
app/
├── src/                    React frontend
│   ├── components/         Reusable UI pieces
│   ├── pages/              Search / Installed / Settings
│   ├── lib/                api wrappers, types, helpers
│   ├── App.tsx
│   ├── main.tsx
│   └── index.css           Tailwind + design tokens
├── src-tauri/              Rust backend
│   ├── src/                Modules: api, db, scanner, installer,
│   │                       savedvars, process_watcher, commands, state
│   ├── Cargo.toml
│   └── tauri.conf.json
├── package.json
└── vite.config.ts
```
