use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::Serialize;
use std::collections::HashSet;
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};
use tauri::{AppHandle, Emitter};

use crate::db;
use crate::scanner;
use crate::state::AppState;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Dependency {
    name: String,
    min_version: Option<u32>,
    required_by: String,
}

#[derive(Serialize, Clone)]
struct InstallProgress<'a> {
    id: &'a str,
    phase: &'a str,
}

pub async fn install_addon(
    state: &AppState,
    app: &AppHandle,
    addon_id: String,
) -> Result<Vec<String>> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut installed_dirs: Vec<String> = Vec::new();
    let mut dependencies = Vec::new();
    let addons_dir = state.addons_dir.lock().unwrap().clone();
    let result = async {
        install_one(
            state,
            app,
            addon_id,
            &addons_dir,
            &mut visited,
            &mut installed_dirs,
            &mut dependencies,
        )
        .await?;
        validate_dependencies(&addons_dir, &dependencies)?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    result.map_err(|error| anyhow!("Installation may be incomplete: {error:#}"))?;
    Ok(installed_dirs)
}

async fn install_one(
    state: &AppState,
    app: &AppHandle,
    addon_id: String,
    addons_dir: &Path,
    visited: &mut HashSet<String>,
    installed_dirs: &mut Vec<String>,
    dependencies: &mut Vec<Dependency>,
) -> Result<()> {
    if !visited.insert(addon_id.clone()) {
        return Ok(());
    }

    let _ = app.emit(
        "addon:install:progress",
        InstallProgress {
            id: &addon_id,
            phase: "fetching",
        },
    );

    let url = state
        .mmoui
        .fetch_addon_download_url(&addon_id)
        .await?
        .ok_or_else(|| anyhow!("no download URL for addon {}", addon_id))?;

    let _ = app.emit(
        "addon:install:progress",
        InstallProgress {
            id: &addon_id,
            phase: "downloading",
        },
    );

    let bytes = state.mmoui.download_bytes(&url).await?;

    let _ = app.emit(
        "addon:install:progress",
        InstallProgress {
            id: &addon_id,
            phase: "extracting",
        },
    );

    std::fs::create_dir_all(addons_dir)?;
    let extracted = {
        let _lock = state.addon_files_lock.lock().unwrap();
        extract_zip(&bytes, addons_dir)?
    };
    for d in &extracted {
        if !installed_dirs.contains(d) {
            installed_dirs.push(d.clone());
        }
    }

    // Resolve dependencies (DependsOn / PCDependsOn / Dependencies — required, not optional)
    let _ = app.emit(
        "addon:install:progress",
        InstallProgress {
            id: &addon_id,
            phase: "dependencies",
        },
    );

    // Record the catalog timestamp we just landed on. The next scan compares
    // this marker against the addon's current catalog.last_updated — useful
    // when an author bumps the ESOUI submission but forgets to bump the
    // `## Version:` line inside the manifest, which would otherwise leave the
    // row stuck on "Update available" forever.
    //
    // Also persist `installed_dir:{dir}` → catalog_id for every root dir we
    // just dropped (including auxiliary folders like `gamedata/` / `EsoUI/`
    // that translation addons ship). Two catalog rows can declare the same
    // directory (e.g. EsoBR 2256 and the obsolete ESOBR Reforged 4541 both
    // ship `EsoBR_Reforged/`) — without this mapping the UI would guess which
    // catalog row owns the dir via `find_by_directory`, non-deterministically.
    {
        let conn = state.db.lock().unwrap();
        if let Ok(Some(addon)) = db::find_by_id(&conn, &addon_id) {
            let key = format!("installed:{}", addon.id);
            let _ = db::set_metadata(&conn, &key, &addon.last_updated.to_string());
            let at_key = format!("installed_at:{}", addon.id);
            let _ = db::set_metadata(&conn, &at_key, &Utc::now().to_rfc3339());

            for dir in &extracted {
                let dir_key = format!("installed_dir:{}", dir);
                let _ = db::set_metadata(&conn, &dir_key, &addon.id);
            }
        }
    }

    let deps = collect_dependencies(addons_dir, &extracted)?;
    for dep in deps {
        dependencies.push(dep.clone());
        if find_dependency_dir(addons_dir, &dep.name)?.is_some() {
            continue;
        }
        let dep_id = {
            let conn = state.db.lock().unwrap();
            db::find_by_directory(&conn, &dep.name)?.map(|a| a.id)
        };
        if let Some(id) = dep_id {
            Box::pin(install_one(
                state,
                app,
                id,
                addons_dir,
                visited,
                installed_dirs,
                dependencies,
            ))
            .await?;
        } else if let Some(min_version) = dep.min_version {
            return Err(anyhow!(
                "{} requires {} >= {}, but it is not installed or in the catalog",
                dep.required_by,
                dep.name,
                min_version
            ));
        } else {
            eprintln!("dependency '{}' not in catalog — skipping", dep.name);
        }
    }

    Ok(())
}

/// Root dir names that are zip packaging garbage (macOS Finder leftovers) and
/// should never be extracted. About 19 catalog entries currently ship a
/// `__MACOSX/` next to their real addon.
fn is_noise_root(name: &str) -> bool {
    matches!(name, "__MACOSX" | ".DS_Store")
}

fn extract_zip(bytes: &[u8], addons_dir: &Path) -> Result<Vec<String>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    if archive.len() > 100_000 {
        return Err(anyhow!("archive contains too many entries"));
    }
    let work = tempfile::Builder::new()
        .prefix(".apocrypha-install-")
        .tempdir_in(addons_dir)?;
    let staged = work.path().join("staged");
    let backup = work.path().join("backup");
    std::fs::create_dir(&staged)?;
    std::fs::create_dir(&backup)?;
    let mut root_dirs: HashSet<String> = HashSet::new();
    let mut files = Vec::new();
    let mut total_size = 0u64;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let raw_name = file.name()?.into_owned();
        if raw_name.split('/').next().is_some_and(is_noise_root) {
            continue;
        }
        let rel = file
            .enclosed_name()
            .ok_or_else(|| anyhow!("unsafe zip entry: {raw_name}"))?
            .to_path_buf();
        if raw_name.contains(['\\', ':'])
            || raw_name.split('/').any(|part| part == ".." || part == ".")
            || rel.components().any(|c| !matches!(c, Component::Normal(_)))
            || raw_name.split('/').any(|part| {
                let part = part.to_ascii_lowercase();
                part == ".apocrypha-manifest" || part.starts_with(".apocrypha-install-")
            })
        {
            return Err(anyhow!("unsafe zip entry: {raw_name}"));
        }
        for component in rel.components() {
            let Component::Normal(name) = component else {
                unreachable!()
            };
            let name = name
                .to_str()
                .ok_or_else(|| anyhow!("invalid zip entry name"))?;
            let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
            if name.ends_with(['.', ' '])
                || matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (base.len() == 4
                    && (base.starts_with("COM") || base.starts_with("LPT"))
                    && matches!(base.as_bytes()[3], b'1'..=b'9'))
            {
                return Err(anyhow!("unsafe zip entry: {raw_name}"));
            }
        }
        if file.unix_mode().is_some_and(|mode| {
            let kind = mode & 0o170000;
            kind != 0 && kind != 0o100000 && kind != 0o040000
        }) {
            return Err(anyhow!("unsupported zip entry type: {raw_name}"));
        }
        if let Some(Component::Normal(name)) = rel.components().next() {
            root_dirs.insert(name.to_string_lossy().into_owned());
        }
        total_size = total_size
            .checked_add(file.size())
            .ok_or_else(|| anyhow!("archive too large"))?;
        if total_size > 1024 * 1024 * 1024 {
            return Err(anyhow!("archive exceeds the 1 GiB extraction limit"));
        }
        let outpath = staged.join(&rel);
        if file.is_dir() {
            std::fs::create_dir_all(&outpath)?;
        } else {
            std::fs::create_dir_all(outpath.parent().unwrap())?;
            let mut outfile = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&outpath)
                .with_context(|| format!("duplicate or invalid zip entry: {raw_name}"))?;
            let size = file.size();
            let copied = std::io::copy(&mut std::io::Read::take(&mut file, size + 1), &mut outfile)
                .with_context(|| format!("extracting {raw_name}"))?;
            if copied != size {
                return Err(anyhow!("incorrect uncompressed size for {raw_name}"));
            }
            files.push(rel);
        }
    }
    if files.is_empty() {
        return Err(anyhow!("archive contains no addon files"));
    }
    for root in &root_dirs {
        let txt = PathBuf::from(root).join(format!("{root}.txt"));
        let addon = PathBuf::from(root).join(format!("{root}.addon"));
        let contains = |path: &Path| {
            files.iter().any(|file| {
                file == path
                    || (cfg!(windows)
                        && file
                            .to_string_lossy()
                            .eq_ignore_ascii_case(&path.to_string_lossy()))
            })
        };
        let chosen = match (contains(&txt), contains(&addon)) {
            (true, _) => Some("txt"),
            (false, true) => Some("addon"),
            _ => None,
        };
        if let Some(chosen) = chosen {
            let marker = PathBuf::from(root).join(".apocrypha-manifest");
            let existing = addons_dir.join(&marker);
            match std::fs::symlink_metadata(&existing) {
                Ok(meta) if meta.file_type().is_file() => {
                    let value = std::fs::read_to_string(&existing)?;
                    if value != "txt" && value != "addon" {
                        return Err(anyhow!(
                            "unrecognized manifest marker: {}",
                            existing.display()
                        ));
                    }
                }
                Ok(_) => return Err(anyhow!("invalid manifest marker: {}", existing.display())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            std::fs::write(staged.join(&marker), chosen)?;
            files.push(marker);
        }
    }
    files.sort();
    if let Err((error, rollback_failed)) =
        replace_files(addons_dir, &staged, &backup, &files, |from, to| {
            std::fs::rename(from, to)
        })
    {
        if rollback_failed {
            let recovery = work.keep();
            return Err(anyhow!(
                "{error:#}; rollback failed; recovery files retained at {}",
                recovery.display()
            ));
        }
        return Err(error);
    }
    work.close()
        .context("installation succeeded but temporary backup cleanup failed")?;
    let mut roots: Vec<_> = root_dirs.into_iter().collect();
    roots.sort();
    Ok(roots)
}

// Files are moved within the same filesystem; each old file remains in backup
// until all replacements succeed. Never follow links in the destination.
fn replace_files<F>(
    addons_dir: &Path,
    staged: &Path,
    backup: &Path,
    files: &[PathBuf],
    mut rename: F,
) -> std::result::Result<(), (anyhow::Error, bool)>
where
    F: FnMut(&Path, &Path) -> std::io::Result<()>,
{
    let mut changed: Vec<(PathBuf, bool, bool)> = Vec::new();
    let mut created_dirs = Vec::new();
    let result = (|| -> Result<()> {
        for rel in files {
            let mut parent = addons_dir.to_path_buf();
            for component in rel.parent().unwrap().components() {
                parent.push(component);
                match std::fs::symlink_metadata(&parent) {
                    Ok(meta) if meta.file_type().is_dir() => {}
                    Ok(_) => {
                        return Err(anyhow!(
                            "destination is not a real directory: {}",
                            parent.display()
                        ))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        std::fs::create_dir(&parent)?;
                        created_dirs.push(parent.clone());
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            let target = addons_dir.join(rel);
            let existed = match std::fs::symlink_metadata(&target) {
                Ok(meta) if meta.file_type().is_file() => true,
                Ok(_) => {
                    return Err(anyhow!(
                        "destination is not a regular file: {}",
                        target.display()
                    ))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => return Err(e.into()),
            };
            if existed {
                let saved = backup.join(rel);
                std::fs::create_dir_all(saved.parent().unwrap())?;
                rename(&target, &saved)
                    .with_context(|| format!("backing up {}", target.display()))?;
            }
            changed.push((rel.clone(), existed, false));
            rename(&staged.join(rel), &target)
                .with_context(|| format!("installing {}", target.display()))?;
            changed.last_mut().unwrap().2 = true;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut failures = Vec::new();
        for (rel, existed, installed) in changed.iter().rev() {
            let target = addons_dir.join(rel);
            if *installed {
                if let Err(e) = std::fs::remove_file(&target) {
                    failures.push(format!("removing {}: {e}", target.display()));
                    continue;
                }
            }
            if *existed {
                if let Err(e) = rename(&backup.join(rel), &target) {
                    failures.push(format!("restoring {}: {e}", target.display()));
                }
            }
        }
        for dir in created_dirs.iter().rev() {
            if let Err(e) = std::fs::remove_dir(dir) {
                failures.push(format!("removing {}: {e}", dir.display()));
            }
        }
        let failed = !failures.is_empty();
        return Err((
            if failed {
                anyhow!("{error:#}; rollback errors: {}", failures.join("; "))
            } else {
                error
            },
            failed,
        ));
    }
    Ok(())
}

fn collect_dependencies(addons_dir: &Path, dirs: &[String]) -> Result<Vec<Dependency>> {
    let mut deps = HashSet::new();

    for dir in dirs {
        let Some(path) = scanner::locate_manifest(&addons_dir.join(dir), dir) else {
            continue;
        };

        let content = scanner::read_lossy(&path)?;

        for raw_line in content.lines() {
            let line = raw_line.trim();
            if !(line.starts_with("## DependsOn:")
                || line.starts_with("## PCDependsOn:")
                || line.starts_with("## Dependencies:"))
            {
                continue;
            }
            let value = line.split_once(':').map(|(_, v)| v.trim()).unwrap_or("");
            for tok in value.split_whitespace() {
                let (name, min_version) = match tok.find(['>', '<', '=']) {
                    Some(index) => {
                        let (name, constraint) = tok.split_at(index);
                        let version = constraint
                            .strip_prefix(">=")
                            .and_then(|v| v.parse::<u32>().ok())
                            .ok_or_else(|| {
                                anyhow!(
                                    "{} has an unsupported dependency constraint '{}'",
                                    dir,
                                    tok
                                )
                            })?;
                        (name, Some(version))
                    }
                    None => (tok, None),
                };
                if name.is_empty()
                    || name.contains(['/', '\\'])
                    || name == "."
                    || name == ".."
                    || name.contains(':')
                {
                    return Err(anyhow!("{} has an invalid dependency name '{}'", dir, name));
                }
                deps.insert(Dependency {
                    name: name.to_string(),
                    min_version,
                    required_by: dir.clone(),
                });
            }
        }
    }
    Ok(deps.into_iter().collect())
}

fn find_dependency_dir(addons_dir: &Path, name: &str) -> Result<Option<String>> {
    let mut matched = None;
    for entry in std::fs::read_dir(addons_dir)? {
        let entry = entry?;
        let dir = entry.file_name().to_string_lossy().into_owned();
        if !dir.eq_ignore_ascii_case(name) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() || (file_type.is_symlink() && entry.path().metadata()?.is_dir()) {
            if matched.is_some() {
                return Err(anyhow!(
                    "multiple installed directories match dependency '{}'",
                    name
                ));
            }
            matched = Some(dir);
        }
    }
    Ok(matched)
}

fn validate_dependencies(addons_dir: &Path, dependencies: &[Dependency]) -> Result<()> {
    for dependency in dependencies {
        validate_dependency_version(addons_dir, dependency)?;
    }
    Ok(())
}

fn validate_dependency_version(addons_dir: &Path, dep: &Dependency) -> Result<()> {
    let Some(min_version) = dep.min_version else {
        return Ok(());
    };
    let dir = find_dependency_dir(addons_dir, &dep.name)?.ok_or_else(|| {
        anyhow!(
            "{} requires {} >= {}, but it is not installed",
            dep.required_by,
            dep.name,
            min_version
        )
    })?;
    let manifest = scanner::locate_manifest(&addons_dir.join(&dir), &dir).ok_or_else(|| {
        anyhow!(
            "{} requires {} >= {}, but its manifest was not found",
            dep.required_by,
            dep.name,
            min_version
        )
    })?;
    let content = scanner::read_lossy(&manifest)?;
    let version = content.lines().find_map(|line| {
        let (key, value) = line.trim().strip_prefix("##")?.split_once(':')?;
        (key.trim() == "AddOnVersion").then(|| value.trim())
    });
    let installed = version.and_then(|v| v.parse::<u32>().ok()).ok_or_else(|| {
        anyhow!(
            "{} requires {} >= {}, but its manifest has no valid AddOnVersion",
            dep.required_by,
            dep.name,
            min_version
        )
    })?;
    if installed < min_version {
        return Err(anyhow!(
            "{} requires {} >= {}, but AddOnVersion {} is installed; update the dependency",
            dep.required_by,
            dep.name,
            min_version,
            installed
        ));
    }
    Ok(())
}

/// Clear the install markers we wrote during the most recent install of this
/// addon. Used by the uninstall path so a future re-install starts fresh
/// instead of inheriting the previous marker.
pub fn clear_install_marker(state: &AppState, catalog_id: &str, dir_name: &str) {
    let conn = state.db.lock().unwrap();
    let _ = db::set_metadata(&conn, &format!("installed:{}", catalog_id), "");
    let _ = db::set_metadata(&conn, &format!("installed_at:{}", catalog_id), "");
    let _ = db::set_metadata(&conn, &format!("installed_dir:{}", dir_name), "");
}

pub fn uninstall(addons_dir: &Path, dir_name: &str) -> Result<PathBuf> {
    // Defense in depth: the command layer also validates, but never trust callers.
    if dir_name.is_empty()
        || dir_name.contains('/')
        || dir_name.contains('\\')
        || dir_name.contains("..")
    {
        return Err(anyhow!("invalid addon directory name"));
    }
    let target = addons_dir.join(dir_name);
    if !target.exists() {
        return Err(anyhow!("addon directory not found"));
    }
    if !target.is_dir() {
        return Err(anyhow!("target is not a directory"));
    }
    // Canonicalize and verify the target is inside addons_dir.
    let canon_addons = addons_dir.canonicalize()?;
    let canon_target = target.canonicalize()?;
    if !canon_target.starts_with(&canon_addons) {
        return Err(anyhow!("target escapes addons folder"));
    }
    std::fs::remove_dir_all(&canon_target)?;
    Ok(canon_target)
}

/// One-shot migration for users coming from v0.1.8 / v0.1.9. Those builds
/// placed `gamedata/`, `EsoUI/`, `fonts/` siblings to `AddOns/` based on the
/// (incorrect) belief that the game client would load translations from
/// `live/`. The Minion install layout — and therefore what the client
/// actually loads — keeps everything under `live/AddOns/`.
///
/// This walks `live/<aux>/` for each known aux root, moves contents into
/// `live/AddOns/<aux>/`, and removes the orphan if it ends up empty. Files
/// already present in `AddOns/` win (don't clobber freshly installed data).
///
/// Idempotent and best-effort: any failure on a single file is logged and
/// skipped without aborting the rest. A metadata flag prevents repeated work.
pub fn migrate_live_orphans(state: &AppState) {
    const FLAG_KEY: &str = "migration:live-orphans-v1";
    {
        let conn = state.db.lock().unwrap();
        if db::get_metadata(&conn, FLAG_KEY).as_deref() == Some("done") {
            return;
        }
    }

    let addons_dir = state.addons_dir.lock().unwrap().clone();
    let live_dir = match addons_dir.parent() {
        Some(p) => p.to_path_buf(),
        None => return,
    };
    // Sanity: only act if addons_dir really looks like `<...>/AddOns`.
    let looks_right = addons_dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.eq_ignore_ascii_case("AddOns"))
        .unwrap_or(false);
    if !looks_right {
        return;
    }

    for aux in &["gamedata", "EsoUI", "fonts"] {
        let orphan = live_dir.join(aux);
        if !orphan.is_dir() {
            continue;
        }
        let target = addons_dir.join(aux);
        if let Err(e) = merge_dir_into(&orphan, &target) {
            eprintln!(
                "migration: failed to merge {:?} into {:?}: {}",
                orphan, target, e
            );
        }
        // Try to remove the orphan if it ended up empty. Non-empty leftovers
        // (e.g. content unrelated to ESO localization that someone dropped in)
        // are intentionally left alone.
        let _ = std::fs::remove_dir(&orphan);
    }

    let conn = state.db.lock().unwrap();
    let _ = db::set_metadata(&conn, FLAG_KEY, "done");
}

/// Recursively move every file from `src` into `dst`, creating subdirs as
/// needed. Files already present at the destination are NOT overwritten —
/// a fresh install in `AddOns/` is always more authoritative than the
/// orphans we're migrating.
fn merge_dir_into(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let dst_path = dst.join(&name);

        if path.is_dir() {
            merge_dir_into(&path, &dst_path)?;
            let _ = std::fs::remove_dir(&path);
        } else if !dst_path.exists() {
            if let Some(parent) = dst_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Try a rename first (cheap, same filesystem). Fall back to copy
            // + delete when rename fails (cross-volume, perms, etc.).
            if std::fs::rename(&path, &dst_path).is_err() {
                std::fs::copy(&path, &dst_path)?;
                let _ = std::fs::remove_file(&path);
            }
        } else {
            // Destination already has this file. Drop the orphan copy.
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    #[test]
    fn collects_all_required_minimum_versions() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addon = temp.path().join("Example");
        std::fs::create_dir_all(&addon)?;
        std::fs::write(
            addon.join("Example.addon"),
            "## DependsOn: LibA>=2 LibB\n## PCDependsOn: LibA>=10\n\
             ## Dependencies: LibC>=3\n## OptionalDependsOn: LibD>=100\n",
        )?;
        let deps = collect_dependencies(temp.path(), &["Example".into()])?;
        for (name, version) in [
            ("LibA", Some(2)),
            ("LibA", Some(10)),
            ("LibB", None),
            ("LibC", Some(3)),
        ] {
            assert!(deps.contains(&Dependency {
                name: name.into(),
                min_version: version,
                required_by: "Example".into(),
            }));
        }
        assert_eq!(deps.len(), 4);

        std::fs::write(addon.join("Example.addon"), "## DependsOn: LibA>=abc")?;
        assert!(collect_dependencies(temp.path(), &["Example".into()])
            .unwrap_err()
            .to_string()
            .contains("unsupported dependency constraint"));
        Ok(())
    }

    #[test]
    fn checks_installed_addon_version_not_display_version() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let lib = temp.path().join("LibExample");
        std::fs::create_dir_all(&lib)?;
        let manifest = lib.join("LibExample.txt");
        let dep = Dependency {
            name: "libexample".into(),
            min_version: Some(10),
            required_by: "Example".into(),
        };
        std::fs::write(&manifest, "## Version: 99.0\n## AddOnVersion: 9\n")?;
        let err = validate_dependency_version(temp.path(), &dep)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Example requires libexample >= 10"));
        assert!(err.contains("AddOnVersion 9"));

        std::fs::write(&manifest, "## Version: 1.0\n## AddOnVersion: 10\n")?;
        validate_dependency_version(temp.path(), &dep)?;
        std::fs::write(&manifest, "## AddOnVersion: 11\n")?;
        validate_dependency_version(temp.path(), &dep)?;

        for content in ["## Version: 99.0\n", "## AddOnVersion: invalid\n"] {
            std::fs::write(&manifest, content)?;
            assert!(validate_dependency_version(temp.path(), &dep)
                .unwrap_err()
                .to_string()
                .contains("no valid AddOnVersion"));
        }
        std::fs::remove_file(&manifest)?;
        assert!(validate_dependency_version(temp.path(), &dep)
            .unwrap_err()
            .to_string()
            .contains("manifest was not found"));
        std::fs::remove_dir(&lib)?;
        assert!(validate_dependency_version(temp.path(), &dep)
            .unwrap_err()
            .to_string()
            .contains("not installed"));
        Ok(())
    }

    #[test]
    fn validates_all_collected_requirements_after_installation() -> Result<()> {
        let temp = tempfile::tempdir()?;
        for dir in ["Example", "Other", "LibExample"] {
            std::fs::create_dir_all(temp.path().join(dir))?;
        }
        std::fs::write(
            temp.path().join("Example").join("Example.txt"),
            "## DependsOn: LibExample>=2\n## OptionalDependsOn: Missing>=99",
        )?;
        std::fs::write(
            temp.path().join("Other").join("Other.addon"),
            "## PCDependsOn: LibExample>=10",
        )?;
        let lib_manifest = temp.path().join("LibExample").join("LibExample.txt");
        std::fs::write(&lib_manifest, "## AddOnVersion: 10")?;
        let dependencies = collect_dependencies(temp.path(), &["Example".into(), "Other".into()])?;
        validate_dependencies(temp.path(), &dependencies)?;

        std::fs::write(&lib_manifest, "## AddOnVersion: 9")?;
        let error = validate_dependencies(temp.path(), &dependencies)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Other requires LibExample >= 10"));
        Ok(())
    }

    #[test]
    fn extraction_preserves_shared_files_and_selects_new_manifest() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path().join("AddOns");
        std::fs::create_dir_all(addons.join("Example"))?;
        std::fs::write(addons.join("Example/Example.txt"), "old manifest")?;
        std::fs::write(addons.join("Example/shared.txt"), "other addon")?;
        std::fs::create_dir_all(addons.join("gamedata"))?;
        std::fs::write(addons.join("gamedata/other.txt"), "other addon")?;
        std::fs::write(addons.join("gamedata/current.txt"), "old")?;

        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        archive.start_file("Example/Example.addon", options)?;
        archive.write_all(b"new")?;
        archive.start_file("gamedata/current.txt", options)?;
        archive.write_all(b"updated")?;
        let bytes = archive.finish()?.into_inner();

        assert_eq!(extract_zip(&bytes, &addons)?, vec!["Example", "gamedata"]);
        assert_eq!(
            std::fs::read(addons.join("Example/Example.txt"))?,
            b"old manifest"
        );
        assert_eq!(
            std::fs::read(addons.join("Example/shared.txt"))?,
            b"other addon"
        );
        assert_eq!(
            std::fs::read(addons.join("gamedata/other.txt"))?,
            b"other addon"
        );
        assert_eq!(
            std::fs::read(addons.join("gamedata/current.txt"))?,
            b"updated"
        );
        assert_eq!(std::fs::read(addons.join("Example/Example.addon"))?, b"new");
        assert_eq!(
            scanner::locate_manifest(&addons.join("Example"), "Example"),
            Some(addons.join("Example/Example.addon"))
        );
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        archive.start_file("Example/Example.txt", SimpleFileOptions::default())?;
        archive.write_all(b"newer")?;
        extract_zip(&archive.finish()?.into_inner(), &addons)?;
        assert_eq!(
            scanner::locate_manifest(&addons.join("Example"), "Example"),
            Some(addons.join("Example/Example.txt"))
        );
        assert_eq!(std::fs::read(addons.join("Example/Example.addon"))?, b"new");
        Ok(())
    }

    #[test]
    fn invalid_or_corrupt_archive_leaves_existing_files_untouched() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path();
        std::fs::create_dir(addons.join("Example"))?;
        std::fs::write(addons.join("Example/file.txt"), "old")?;
        for unsafe_name in [
            "../escape.txt",
            "Example/../../escape",
            "Example/a:b",
            "Example\\escape",
        ] {
            let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
            let options = SimpleFileOptions::default();
            archive.start_file("Example/file.txt", options)?;
            archive.write_all(b"new")?;
            archive.start_file(unsafe_name, options)?;
            archive.write_all(b"unsafe")?;
            assert!(extract_zip(&archive.finish()?.into_inner(), addons).is_err());
            assert_eq!(std::fs::read(addons.join("Example/file.txt"))?, b"old");
        }
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        archive.start_file(
            "Example/file.txt",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )?;
        archive.write_all(b"new")?;
        let mut bytes = archive.finish()?.into_inner();
        let offset = bytes.windows(3).position(|w| w == b"new").unwrap();
        bytes[offset] ^= 0xff;
        assert!(extract_zip(&bytes, addons).is_err());
        assert_eq!(std::fs::read(addons.join("Example/file.txt"))?, b"old");
        Ok(())
    }

    #[test]
    fn destination_conflict_rolls_back_earlier_replacement() -> Result<()> {
        let temp = tempfile::tempdir()?;
        std::fs::create_dir(temp.path().join("Example"))?;
        std::fs::write(temp.path().join("Example/a"), "old")?;
        std::fs::create_dir(temp.path().join("Example/b"))?;
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for name in ["Example/a", "Example/b"] {
            archive.start_file(name, SimpleFileOptions::default())?;
            archive.write_all(b"new")?;
        }
        assert!(extract_zip(&archive.finish()?.into_inner(), temp.path()).is_err());
        assert_eq!(std::fs::read(temp.path().join("Example/a"))?, b"old");
        assert!(temp.path().join("Example/b").is_dir());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn destination_symlink_is_never_followed() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path().join("addons");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&addons)?;
        std::fs::create_dir(&outside)?;
        std::fs::write(outside.join("file"), "old")?;
        std::os::unix::fs::symlink(&outside, addons.join("Example"))?;
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        archive.start_file("Example/file", SimpleFileOptions::default())?;
        archive.write_all(b"new")?;
        assert!(extract_zip(&archive.finish()?.into_inner(), &addons).is_err());
        assert_eq!(std::fs::read(outside.join("file"))?, b"old");
        Ok(())
    }

    #[test]
    fn replacement_failure_rolls_back_all_changed_files() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path().join("addons");
        let stage = temp.path().join("stage");
        let backup = temp.path().join("backup");
        for dir in [&addons, &stage, &backup] {
            std::fs::create_dir(dir)?;
        }
        std::fs::write(addons.join("a"), "old a")?;
        std::fs::write(addons.join("b"), "old b")?;
        std::fs::write(stage.join("a"), "new a")?;
        std::fs::write(stage.join("b"), "new b")?;
        let mut calls = 0;
        let result = replace_files(
            &addons,
            &stage,
            &backup,
            &["a".into(), "b".into()],
            |from, to| {
                calls += 1;
                if calls == 4 {
                    Err(std::io::Error::other("injected replacement failure"))
                } else {
                    std::fs::rename(from, to)
                }
            },
        );
        let (error, rollback_failed) = result.unwrap_err();
        assert!(error.to_string().contains("installing"));
        assert!(!rollback_failed);
        assert_eq!(std::fs::read(addons.join("a"))?, b"old a");
        assert_eq!(std::fs::read(addons.join("b"))?, b"old b");
        Ok(())
    }

    #[test]
    fn rollback_failure_retains_the_backup() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path().join("addons");
        let stage = temp.path().join("stage");
        let backup = temp.path().join("backup");
        for dir in [&addons, &stage, &backup] {
            std::fs::create_dir(dir)?;
        }
        std::fs::write(addons.join("a"), "old")?;
        std::fs::write(stage.join("a"), "new")?;
        let mut calls = 0;
        let (error, rollback_failed) =
            replace_files(&addons, &stage, &backup, &["a".into()], |from, to| {
                calls += 1;
                if calls >= 2 {
                    Err(std::io::Error::other("injected failure"))
                } else {
                    std::fs::rename(from, to)
                }
            })
            .unwrap_err();
        assert!(rollback_failed);
        assert!(error.to_string().contains("rollback errors"));
        assert_eq!(std::fs::read(backup.join("a"))?, b"old");
        Ok(())
    }
}
