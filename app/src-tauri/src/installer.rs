use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{ipc::Channel, AppHandle, Emitter, Manager};
use tokio::sync::oneshot;

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

#[derive(Clone, Serialize)]
pub struct InstallConflictReview {
    id: String,
    backup_path: String,
    file_count: usize,
    files: Vec<InstallConflictFile>,
}

#[derive(Clone, Serialize)]
struct InstallConflictFile {
    path: String,
    operation: &'static str,
    reason: &'static str,
}

#[derive(Default)]
pub struct InstallConflictApproval {
    pending: Mutex<Option<(String, oneshot::Sender<bool>)>>,
}

impl InstallConflictApproval {
    fn take(&self, id: &str) -> Option<oneshot::Sender<bool>> {
        let mut pending = self.pending.lock().unwrap();
        if pending.as_ref().is_some_and(|(current, _)| current == id) {
            return pending.take().map(|(_, sender)| sender);
        }
        None
    }

    pub fn resolve(&self, id: &str, approved: bool) -> Result<()> {
        self.take(id)
            .ok_or_else(|| anyhow!("this conflict review is no longer pending"))?
            .send(approved)
            .map_err(|_| anyhow!("this conflict review is no longer pending"))
    }

    async fn request(
        &self,
        channel: &Channel<InstallConflictReview>,
        review: InstallConflictReview,
    ) -> Result<bool> {
        let id = review.id.clone();
        let (sender, receiver) = oneshot::channel();
        *self.pending.lock().unwrap() = Some((id.clone(), sender));
        let result = async {
            channel.send(review).context("could not open conflict review")?;
            tokio::time::timeout(Duration::from_secs(15 * 60), receiver)
                .await
                .context("conflict review timed out")?
                .context("conflict review closed")
        }
        .await;
        drop(self.take(&id));
        result
    }
}

pub async fn install_addon(
    state: &AppState,
    app: &AppHandle,
    addon_id: String,
    conflict_review: Option<&Channel<InstallConflictReview>>,
) -> Result<Vec<String>> {
    // Hold one guard for the full dependency tree; install_one recurses without locking.
    let _mutation_guard = state.addon_fs_mutations.lock().await;
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
            conflict_review,
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
    conflict_review: Option<&Channel<InstallConflictReview>>,
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
    let staged = stage_install(&bytes, addons_dir, &addon_id)?;
    let saved_backup = if staged.conflicts.is_empty() {
        None
    } else {
        let channel = conflict_review.ok_or_else(|| anyhow!(
            "update requires approval for existing untracked or modified files; update this addon manually in Apocrypha"
        ))?;
        let conflicts = &staged.conflicts;
        let backup = backup_conflicts(addons_dir, &state.backups_dir, &addon_id, conflicts)?;
        let review = InstallConflictReview {
            id: backup
                .path()
                .file_name()
                .context("conflict backup has no directory name")?
                .to_string_lossy()
                .into_owned(),
            backup_path: backup.path().display().to_string(),
            file_count: conflicts.len(),
            files: conflicts
                .iter()
                .take(12)
                .map(|file| InstallConflictFile {
                    path: file.rel.display().to_string(),
                    operation: file.operation,
                    reason: file.action,
                })
                .collect(),
        };
        if !app
            .state::<InstallConflictApproval>()
            .request(channel, review)
            .await?
        {
            backup
                .close()
                .context("failed to remove cancelled conflict backup")?;
            return Err(anyhow!(
                "update cancelled; conflicting files were not changed"
            ));
        }
        verify_conflict_backup(addons_dir, backup.path(), conflicts)?;
        let path = backup.keep();
        let _ = app.emit(
            "addon:install:backup",
            serde_json::json!({ "id": &addon_id, "path": path.to_string_lossy() }),
        );
        Some(path)
    };
    let extracted = staged.commit(addons_dir).map_err(|error| match saved_backup {
        Some(path) => anyhow!(
            "{error:#}; original conflicting files are backed up at {}",
            path.display()
        ),
        None => error,
    })?;
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
                conflict_review,
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

const OWNERSHIP_FILE: &str = ".apocrypha-ownership.json";

#[derive(Default, Serialize, Deserialize)]
struct Ownership {
    files: BTreeMap<String, OwnedFile>,
}

#[derive(Serialize, Deserialize)]
struct OwnedFile {
    path: String,
    addon_id: String,
    fingerprint: FileFingerprint,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileFingerprint {
    sha256: [u8; 32],
    volume: u64,
    file_id: u64,
}

#[derive(Serialize)]
struct FileChange {
    rel: PathBuf,
    install: bool,
    expected_hash: Option<FileFingerprint>,
}

struct FileConflict {
    rel: PathBuf,
    fingerprint: FileFingerprint,
    operation: &'static str,
    action: &'static str,
}

fn backup_conflicts(
    addons_dir: &Path,
    backups_dir: &Path,
    addon_id: &str,
    conflicts: &[FileConflict],
) -> Result<tempfile::TempDir> {
    let parent = backups_dir.join("addon-file-conflicts");
    let addons_canonical = addons_dir.canonicalize()?;
    let mut existing = parent.as_path();
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| anyhow!("cannot resolve backup directory"))?;
    }
    if existing.canonicalize()?.starts_with(&addons_canonical) {
        return Err(anyhow!(
            "conflict backups must be stored outside the AddOns folder"
        ));
    }
    std::fs::create_dir_all(&parent)?;
    if parent.canonicalize()?.starts_with(&addons_canonical) {
        return Err(anyhow!(
            "conflict backups must be stored outside the AddOns folder"
        ));
    }
    let backup = tempfile::Builder::new()
        .prefix("conflict-")
        .tempdir_in(&parent)?;
    let files_dir = backup.path().join("files");
    std::fs::create_dir(&files_dir)?;
    let mut list = format!(
        "Addon ID or directory: {addon_id}\nOriginal AddOns folder: {}\n\nOriginal selected files are under files/ using these relative paths; this is not a full addon backup.\nTo restore, close Apocrypha and ESO, back up the current files, then copy the desired originals to the corresponding paths in AddOns. If restoring a manifest, remove the newer alternate extension so the game cannot load both. Restored files will need conflict resolution on the next update.\n\n",
        addons_dir.display()
    );
    for item in conflicts {
        let source = existing_file(addons_dir, &item.rel)?
            .ok_or_else(|| conflict(&item.rel, "file disappeared before backup"))?;
        if hash_file(&source)? != item.fingerprint {
            return Err(conflict(&item.rel, "file changed before backup"));
        }
        let destination = files_dir.join(&item.rel);
        std::fs::create_dir_all(destination.parent().unwrap())?;
        std::fs::copy(&source, &destination)?;
        if hash_file(&destination)?.sha256 != item.fingerprint.sha256
            || hash_file(&source)? != item.fingerprint
        {
            return Err(conflict(&item.rel, "file changed during backup"));
        }
        list.push_str(&format!(
            "{}: {} ({})\n",
            item.operation,
            item.rel.display(),
            item.action
        ));
    }
    std::fs::write(backup.path().join("conflicts.txt"), list)?;
    Ok(backup)
}

fn verify_conflict_backup(
    addons_dir: &Path,
    backup: &Path,
    conflicts: &[FileConflict],
) -> Result<()> {
    if existing_file(backup, Path::new("conflicts.txt"))?.is_none() {
        return Err(anyhow!("conflict backup list is missing"));
    }
    for item in conflicts {
        let original = existing_file(addons_dir, &item.rel)?
            .ok_or_else(|| conflict(&item.rel, "file disappeared during review"))?;
        let saved = existing_file(&backup.join("files"), &item.rel)?
            .ok_or_else(|| conflict(&item.rel, "backup disappeared during review"))?;
        if hash_file(&original)? != item.fingerprint
            || hash_file(&saved)?.sha256 != item.fingerprint.sha256
        {
            return Err(conflict(&item.rel, "file or backup changed during review"));
        }
    }
    Ok(())
}

fn path_key(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err(anyhow!("invalid ownership path: {}", path.display()));
        };
        let name = name
            .to_str()
            .ok_or_else(|| anyhow!("invalid ownership path"))?;
        let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        if name.chars().any(char::is_control)
            || name.contains(['\\', ':'])
            || name.ends_with(['.', ' '])
            || matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (base.len() == 4
                && (base.starts_with("COM") || base.starts_with("LPT"))
                && matches!(base.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(anyhow!("invalid ownership path: {}", path.display()));
        }
        parts.push(name);
    }
    if parts.is_empty()
        || parts.iter().any(|name| {
            let name = name.to_ascii_lowercase();
            name == OWNERSHIP_FILE
                || name == ".apocrypha-manifest"
                || name.starts_with(".apocrypha-install-")
        })
    {
        return Err(anyhow!("invalid ownership path: {}", path.display()));
    }
    Ok(parts.join("/").to_lowercase())
}

fn portable_path(path: &Path) -> Result<String> {
    path_key(path)?;
    Ok(path
        .components()
        .map(|component| component.as_os_str().to_str().unwrap())
        .collect::<Vec<_>>()
        .join("/"))
}

fn hash_file(path: &Path) -> Result<FileFingerprint> {
    let mut input = std::fs::File::open(path)?;
    #[cfg(windows)]
    let (volume, file_id) = {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        // The handle remains open for the duration of this call.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(input.as_raw_handle() as _, &mut info) } == 0 {
            return Err(std::io::Error::last_os_error())
                .context(format!("identifying {}", path.display()));
        }
        let id = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
        if id == 0 {
            return Err(anyhow!(
                "filesystem did not provide an identity for {}",
                path.display()
            ));
        }
        (u64::from(info.dwVolumeSerialNumber), id)
    };
    #[cfg(unix)]
    let (volume, file_id) = {
        use std::os::unix::fs::MetadataExt;
        let metadata = input.metadata()?;
        (metadata.dev(), metadata.ino())
    };
    let mut hasher = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(FileFingerprint {
        sha256: hasher.finalize().into(),
        volume,
        file_id,
    })
}

// Inspect each component without following destination symlinks.
fn existing_file(addons_dir: &Path, rel: &Path) -> Result<Option<PathBuf>> {
    let mut path = addons_dir.to_path_buf();
    let mut components = rel.components().peekable();
    while let Some(component) = components.next() {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if components.peek().is_some() && meta.file_type().is_dir() => {}
            Ok(meta) if components.peek().is_none() && meta.file_type().is_file() => {
                return Ok(Some(path))
            }
            Ok(_) => {
                return Err(anyhow!(
                    "destination is not a regular file or directory: {}",
                    path.display()
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    Err(anyhow!("empty destination path"))
}

fn load_ownership(addons_dir: &Path) -> Result<(Ownership, Option<FileFingerprint>)> {
    let Some(path) = existing_file(addons_dir, Path::new(OWNERSHIP_FILE))? else {
        return Ok((Ownership::default(), None));
    };
    let contents = std::fs::read(&path)?;
    let hash = hash_file(&path)?;
    if hash.sha256 != <[u8; 32]>::from(Sha256::digest(&contents)) {
        return Err(anyhow!("addon ownership registry changed while reading"));
    }
    let ownership: Ownership = serde_json::from_slice(&contents)
        .context("invalid addon ownership registry; restore it from backup before installing")?;
    for (key, entry) in &ownership.files {
        if path_key(Path::new(&entry.path))? != *key || entry.addon_id.is_empty() {
            return Err(anyhow!("invalid addon ownership registry entry: {key}"));
        }
    }
    Ok((ownership, Some(hash)))
}

fn conflict(path: &Path, reason: &str) -> anyhow::Error {
    anyhow!(
        "cannot change {}: {reason}; back up and move the conflicting file outside AddOns after verifying its owner, then retry",
        path.display()
    )
}

#[cfg(test)]
fn extract_zip(bytes: &[u8], addons_dir: &Path, addon_id: &str) -> Result<Vec<String>> {
    extract_zip_with_resolution(bytes, addons_dir, addon_id, &mut |conflicts| {
        Err(conflict(&conflicts[0].rel, conflicts[0].action))
    })
}

fn ensure_no_unfinished_install(addons_dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(addons_dir)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with(".apocrypha-install-")
        {
            return Err(anyhow!(
                "unfinished installation at {}; recover its backup and move the directory outside AddOns before retrying",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
fn extract_zip_with_resolution(
    bytes: &[u8],
    addons_dir: &Path,
    addon_id: &str,
    resolve: &mut impl FnMut(&[FileConflict]) -> Result<()>,
) -> Result<Vec<String>> {
    let staged = stage_install(bytes, addons_dir, addon_id)?;
    if !staged.conflicts.is_empty() {
        resolve(&staged.conflicts)?;
    }
    staged.commit(addons_dir)
}

struct StagedInstall {
    work: tempfile::TempDir,
    changes: Vec<FileChange>,
    conflicts: Vec<FileConflict>,
    roots: Vec<String>,
}

impl StagedInstall {
    fn commit(self, addons_dir: &Path) -> Result<Vec<String>> {
        let staged = self.work.path().join("staged");
        let backup = self.work.path().join("backup");
        commit_file_changes(self.work, addons_dir, &staged, &backup, &self.changes)?;
        Ok(self.roots)
    }
}

fn stage_install(bytes: &[u8], addons_dir: &Path, addon_id: &str) -> Result<StagedInstall> {
    ensure_no_unfinished_install(addons_dir)?;
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
                part == OWNERSHIP_FILE
                    || part == ".apocrypha-manifest"
                    || part.starts_with(".apocrypha-install-")
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
            if name.chars().any(char::is_control)
                || name.ends_with(['.', ' '])
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
    let (mut ownership, registry_hash) = load_ownership(addons_dir)?;
    let mut incoming = BTreeMap::new();
    for rel in &files {
        let key = path_key(rel)?;
        if incoming.insert(key, rel).is_some() {
            return Err(anyhow!(
                "archive contains conflicting file names: {}",
                rel.display()
            ));
        }
    }

    let mut changes = Vec::new();
    let mut conflicts = Vec::new();
    // Both extensions can be loaded by the game.
    for root in &root_dirs {
        let txt = PathBuf::from(root).join(format!("{root}.txt"));
        let addon = PathBuf::from(root).join(format!("{root}.addon"));
        let has_txt = incoming.contains_key(&path_key(&txt)?);
        let has_addon = incoming.contains_key(&path_key(&addon)?);
        if has_txt && has_addon {
            return Err(anyhow!("archive contains both manifests for {root}"));
        }
        let alternate = if has_txt {
            Some(addon)
        } else if has_addon {
            Some(txt)
        } else {
            None
        };
        if let Some(alternate) = alternate {
            if let Some(path) = existing_file(addons_dir, &alternate)? {
                match ownership.files.get(&path_key(&alternate)?) {
                    Some(entry) if entry.addon_id != addon_id => {
                        return Err(conflict(
                            &alternate,
                            "alternate manifest belongs to another addon",
                        ));
                    }
                    Some(entry) if cfg!(windows) || Path::new(&entry.path) == alternate => {}
                    _ => {
                        let fingerprint = hash_file(&path)?;
                        conflicts.push(FileConflict {
                            rel: alternate.clone(),
                            fingerprint: fingerprint.clone(),
                            operation: "Remove",
                            action: "alternate manifest has unknown ownership",
                        });
                        changes.push(FileChange {
                            rel: alternate,
                            install: false,
                            expected_hash: Some(fingerprint),
                        });
                    }
                }
            }
        }
    }

    for (key, entry) in &ownership.files {
        if entry.addon_id != addon_id || incoming.contains_key(key) {
            continue;
        }
        let rel = PathBuf::from(&entry.path);
        if let Some(path) = existing_file(addons_dir, &rel)? {
            let current = hash_file(&path)?;
            if current != entry.fingerprint {
                conflicts.push(FileConflict {
                    rel: rel.clone(),
                    fingerprint: current.clone(),
                    operation: "Remove",
                    action: "previously installed file was modified",
                });
            }
            changes.push(FileChange {
                rel,
                install: false,
                expected_hash: Some(current),
            });
        }
    }
    for (key, rel) in &incoming {
        let previous = ownership.files.get(key);
        let current = existing_file(addons_dir, rel)?
            .map(|path| hash_file(&path))
            .transpose()?;
        if let Some(entry) = previous {
            if entry.addon_id != addon_id && current.is_some() {
                return Err(conflict(rel, "file belongs to another addon"));
            }
            if !cfg!(windows)
                && entry.path != portable_path(rel)?
                && existing_file(addons_dir, Path::new(&entry.path))?.is_some()
            {
                return Err(conflict(
                    rel,
                    "file name differs only by case from an owned file",
                ));
            }
        }
        if let Some(hash) = &current {
            match previous {
                None => conflicts.push(FileConflict {
                    rel: (*rel).clone(),
                    fingerprint: hash.clone(),
                    operation: "Replace",
                    action: "file ownership is unknown",
                }),
                Some(entry) if &entry.fingerprint != hash => {
                    conflicts.push(FileConflict {
                        rel: (*rel).clone(),
                        fingerprint: hash.clone(),
                        operation: "Replace",
                        action: "previously installed file was modified",
                    });
                }
                _ => {}
            }
        }
        changes.push(FileChange {
            rel: (*rel).clone(),
            install: true,
            expected_hash: current,
        });
    }
    ownership
        .files
        .retain(|_, entry| entry.addon_id != addon_id);
    for (key, rel) in incoming {
        ownership.files.insert(
            key,
            OwnedFile {
                path: portable_path(rel)?,
                addon_id: addon_id.to_string(),
                fingerprint: hash_file(&staged.join(rel))?,
            },
        );
    }
    std::fs::write(staged.join(OWNERSHIP_FILE), serde_json::to_vec(&ownership)?)?;
    changes.push(FileChange {
        rel: PathBuf::from(OWNERSHIP_FILE),
        install: true,
        expected_hash: registry_hash,
    });
    let mut roots: Vec<_> = root_dirs.into_iter().collect();
    roots.sort();
    Ok(StagedInstall {
        work,
        changes,
        conflicts,
        roots,
    })
}

fn commit_file_changes(
    work: tempfile::TempDir,
    addons_dir: &Path,
    staged: &Path,
    backup: &Path,
    changes: &[FileChange],
) -> Result<()> {
    std::fs::write(
        work.path().join("journal.json"),
        serde_json::to_vec(changes)?,
    )?;
    if let Err((error, rollback_failed)) =
        replace_files(addons_dir, staged, backup, changes, |from, to| {
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
        .context("file changes succeeded but temporary backup cleanup failed")?;
    Ok(())
}

// Files are moved within the same filesystem; each old file remains in backup
// until all replacements succeed. Never follow links in the destination.
fn replace_files<F>(
    addons_dir: &Path,
    staged: &Path,
    backup: &Path,
    files: &[FileChange],
    mut rename: F,
) -> std::result::Result<(), (anyhow::Error, bool)>
where
    F: FnMut(&Path, &Path) -> std::io::Result<()>,
{
    let mut changed: Vec<(PathBuf, bool, bool)> = Vec::new();
    let mut created_dirs = Vec::new();
    let result = (|| -> Result<()> {
        for change in files {
            let rel = &change.rel;
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
            let actual_hash = if existed {
                Some(hash_file(&target)?)
            } else {
                None
            };
            if actual_hash != change.expected_hash {
                return Err(conflict(rel, "file changed during installation"));
            }
            if existed {
                let saved = backup.join(rel);
                std::fs::create_dir_all(saved.parent().unwrap())?;
                rename(&target, &saved)
                    .with_context(|| format!("backing up {}", target.display()))?;
            }
            changed.push((rel.clone(), existed, false));
            if change.install {
                rename(&staged.join(rel), &target)
                    .with_context(|| format!("installing {}", target.display()))?;
                changed.last_mut().unwrap().2 = true;
            }
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

pub struct UninstallOutcome {
    pub preserved_files: bool,
    pub backup_path: Option<PathBuf>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct LegacyUninstallFile {
    path: String,
    fingerprint: String,
    manifest: bool,
}

#[derive(Serialize)]
pub struct UninstallPreview {
    legacy: bool,
    addons_dir: String,
    files: Vec<LegacyUninstallFile>,
    protected_count: usize,
    backup_dir: String,
}

fn validate_uninstall_dir(addons_dir: &Path, dir_name: &str) -> Result<PathBuf> {
    if dir_name.is_empty()
        || dir_name.contains('/')
        || dir_name.contains('\\')
        || dir_name.contains("..")
    {
        return Err(anyhow!("invalid addon directory name"));
    }
    path_key(Path::new(dir_name))?;
    let target = addons_dir.join(dir_name);
    match std::fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_dir() => Ok(target),
        Ok(_) => Err(anyhow!("target is not a real addon directory")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(anyhow!("addon directory not found"))
        }
        Err(e) => Err(e.into()),
    }
}

fn manifest_path(dir_name: &str, rel: &Path) -> bool {
    ["txt", "addon"].iter().any(|extension| {
        let expected = PathBuf::from(dir_name).join(format!("{dir_name}.{extension}"));
        if cfg!(windows) {
            rel.to_string_lossy()
                .eq_ignore_ascii_case(&expected.to_string_lossy())
        } else {
            rel == expected
        }
    })
}

pub async fn preview_uninstall(state: &AppState, dir_name: &str) -> Result<UninstallPreview> {
    let _guard = state.addon_fs_mutations.lock().await;
    let addons_dir = state.addons_dir.lock().unwrap().clone();
    let target = validate_uninstall_dir(&addons_dir, dir_name)?;
    ensure_no_unfinished_install(&addons_dir)?;
    let (ownership, _) = load_ownership(&addons_dir)?;
    let mut tracked_manifest = false;
    let mut untracked_manifest = false;
    for extension in ["txt", "addon"] {
        let rel = PathBuf::from(dir_name).join(format!("{dir_name}.{extension}"));
        if existing_file(&addons_dir, &rel)?.is_some() {
            if ownership.files.contains_key(&path_key(&rel)?) {
                tracked_manifest = true;
            } else {
                untracked_manifest = true;
            }
        }
    }
    if tracked_manifest && untracked_manifest {
        return Err(anyhow!("cannot safely uninstall {dir_name}: tracked and untracked manifests coexist; review them manually"));
    }
    let mut result = UninstallPreview {
        legacy: !tracked_manifest,
        addons_dir: addons_dir.canonicalize()?.to_string_lossy().into_owned(),
        files: Vec::new(),
        protected_count: 0,
        backup_dir: state
            .backups_dir
            .join("addon-file-conflicts")
            .to_string_lossy()
            .into_owned(),
    };
    if !result.legacy {
        return Ok(result);
    }
    let mut pending = vec![target];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            if ty.is_dir() {
                pending.push(entry.path());
            } else if ty.is_file() {
                let path = entry.path();
                let rel = path.strip_prefix(&addons_dir)?;
                let key = match path_key(rel) {
                    Ok(key) => key,
                    Err(_) => {
                        result.protected_count += 1;
                        continue;
                    }
                };
                if ownership.files.contains_key(&key) {
                    result.protected_count += 1;
                    continue;
                }
                result.files.push(LegacyUninstallFile {
                    path: portable_path(rel)?,
                    fingerprint: serde_json::to_string(&hash_file(&path)?)?,
                    manifest: manifest_path(dir_name, rel),
                });
                if result.files.len() > 100_000 {
                    return Err(anyhow!("too many untracked files to review"));
                }
            } else {
                result.protected_count += 1;
            }
        }
    }
    result.files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(result)
}

pub async fn uninstall_addon(state: &AppState, dir_name: &str) -> Result<UninstallOutcome> {
    let _mutation_guard = state.addon_fs_mutations.lock().await;
    let addons_dir = state.addons_dir.lock().unwrap().clone();
    let (id, roots, outcome) = uninstall(&addons_dir, dir_name)?;
    let cleanup = (|| -> Result<()> {
        use rusqlite::OptionalExtension;
        let mut conn = state.db.lock().unwrap();
        let tx = conn.transaction()?;
        db::set_metadata(&tx, &format!("installed:{id}"), "")?;
        db::set_metadata(&tx, &format!("installed_at:{id}"), "")?;
        for root in roots {
            let key = format!("installed_dir:{root}");
            let pinned: Option<String> = tx
                .query_row("SELECT value FROM metadata WHERE key = ?1", [&key], |row| {
                    row.get(0)
                })
                .optional()?;
            if pinned.as_deref() == Some(&id) {
                db::set_metadata(&tx, &key, "")?;
            }
        }
        tx.commit()?;
        Ok(())
    })();
    cleanup.context("addon files were removed but metadata cleanup failed")?;
    Ok(outcome)
}

pub async fn uninstall_legacy(
    state: &AppState,
    dir_name: &str,
    preview_root: &str,
    approved: Vec<LegacyUninstallFile>,
) -> Result<UninstallOutcome> {
    let _guard = state.addon_fs_mutations.lock().await;
    let addons_dir = state.addons_dir.lock().unwrap().clone();
    let target = validate_uninstall_dir(&addons_dir, dir_name)?;
    if addons_dir.canonicalize()? != Path::new(preview_root) {
        return Err(anyhow!("AddOns folder changed since review; review again"));
    }
    ensure_no_unfinished_install(&addons_dir)?;
    if approved.is_empty() {
        return Err(anyhow!("no files selected for removal"));
    }
    let (ownership, registry_hash) = load_ownership(&addons_dir)?;
    let mut keys = HashSet::new();
    let mut conflicts = Vec::new();
    let mut changes = Vec::new();
    let mut dirs = HashSet::new();
    for file in approved {
        let rel = PathBuf::from(&file.path);
        let key = path_key(&rel)?;
        let root = rel
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
            .ok_or_else(|| anyhow!("invalid selected path"))?;
        if !(if cfg!(windows) {
            root.eq_ignore_ascii_case(dir_name)
        } else {
            root == dir_name
        }) || rel.components().count() < 2
            || !keys.insert(key.clone())
        {
            return Err(anyhow!(
                "invalid or duplicate selected path: {}",
                rel.display()
            ));
        }
        if ownership.files.contains_key(&key) {
            return Err(conflict(
                &rel,
                "file has tracked ownership and cannot be removed as legacy",
            ));
        }
        let expected: FileFingerprint =
            serde_json::from_str(&file.fingerprint).context("invalid file review fingerprint")?;
        let path = existing_file(&addons_dir, &rel)?
            .ok_or_else(|| conflict(&rel, "file disappeared since review"))?;
        if hash_file(&path)? != expected {
            return Err(conflict(&rel, "file changed since review"));
        }
        add_parent_dirs(&rel, &mut dirs);
        conflicts.push(FileConflict {
            rel: rel.clone(),
            fingerprint: expected.clone(),
            operation: "Remove",
            action: "untracked file selected for uninstall",
        });
        changes.push(FileChange {
            rel,
            install: false,
            expected_hash: Some(expected),
        });
    }
    let mut selected_manifest = false;
    for extension in ["txt", "addon"] {
        let rel = PathBuf::from(dir_name).join(format!("{dir_name}.{extension}"));
        if existing_file(&addons_dir, &rel)?.is_some() {
            let key = path_key(&rel)?;
            if ownership.files.contains_key(&key) {
                return Err(anyhow!(
                    "a tracked manifest cannot be removed through legacy uninstall"
                ));
            }
            if !keys.contains(&key) {
                return Err(anyhow!(
                    "select every existing addon manifest to complete uninstall"
                ));
            }
            selected_manifest = true;
        }
    }
    let backup = backup_conflicts(&addons_dir, &state.backups_dir, dir_name, &conflicts)?;
    verify_conflict_backup(&addons_dir, backup.path(), &conflicts)?;
    if load_ownership(&addons_dir)?.1 != registry_hash {
        return Err(anyhow!(
            "ownership registry changed since review; review again"
        ));
    }
    let backup_path = backup.keep();
    let result = (|| -> Result<()> {
        let work = tempfile::Builder::new()
            .prefix(".apocrypha-install-")
            .tempdir_in(&addons_dir)?;
        let staged = work.path().join("staged");
        let rollback = work.path().join("backup");
        std::fs::create_dir(&staged)?;
        std::fs::create_dir(&rollback)?;
        commit_file_changes(work, &addons_dir, &staged, &rollback, &changes)?;
        cleanup_empty_dirs(&addons_dir, dirs)?;
        if selected_manifest {
            let conn = state.db.lock().unwrap();
            db::set_metadata(&conn, &format!("installed_dir:{dir_name}"), "")
                .context("files were removed but directory marker cleanup failed")?;
        }
        Ok(())
    })();
    result.with_context(|| {
        format!(
            "original selected files are backed up at {}",
            backup_path.display()
        )
    })?;
    Ok(UninstallOutcome {
        preserved_files: target.exists(),
        backup_path: Some(backup_path),
    })
}

fn uninstall(
    addons_dir: &Path,
    dir_name: &str,
) -> Result<(String, HashSet<String>, UninstallOutcome)> {
    let target = validate_uninstall_dir(addons_dir, dir_name)?;
    ensure_no_unfinished_install(addons_dir)?;
    let (mut ownership, registry_hash) = load_ownership(addons_dir)?;
    let mut owner: Option<String> = None;
    for extension in ["txt", "addon"] {
        let rel = PathBuf::from(dir_name).join(format!("{dir_name}.{extension}"));
        let Some(path) = existing_file(addons_dir, &rel)? else {
            continue;
        };
        let entry = ownership.files.get(&path_key(&rel)?)
            .filter(|entry| cfg!(windows) || Path::new(&entry.path) == rel)
            .ok_or_else(|| anyhow!("cannot safely uninstall {dir_name}: manifest ownership is unknown; legacy files are left untouched"))?;
        if hash_file(&path)? != entry.fingerprint {
            return Err(anyhow!(
                "cannot safely uninstall {dir_name}: manifest was modified or replaced"
            ));
        }
        if owner.as_ref().is_some_and(|id| id != &entry.addon_id) {
            return Err(anyhow!(
                "cannot safely uninstall {dir_name}: manifests belong to different addons"
            ));
        }
        owner = Some(entry.addon_id.clone());
    }
    let owner = owner.ok_or_else(|| anyhow!("cannot safely uninstall {dir_name}: no tracked manifest; legacy files are left untouched"))?;
    let mut changes = Vec::new();
    let mut dirs = HashSet::new();
    let mut roots = HashSet::new();
    for entry in ownership
        .files
        .values()
        .filter(|entry| entry.addon_id == owner)
    {
        let rel = PathBuf::from(&entry.path);
        if let Some(Component::Normal(root)) = rel.components().next() {
            roots.insert(root.to_string_lossy().into_owned());
        }
        if let Some(path) = existing_file(addons_dir, &rel)? {
            let fingerprint = hash_file(&path)?;
            if fingerprint != entry.fingerprint {
                return Err(conflict(
                    &rel,
                    "owned file was modified; uninstall was not started",
                ));
            }
            add_parent_dirs(&rel, &mut dirs);
            changes.push(FileChange {
                rel,
                install: false,
                expected_hash: Some(fingerprint),
            });
        }
    }
    ownership.files.retain(|_, entry| entry.addon_id != owner);
    let work = tempfile::Builder::new()
        .prefix(".apocrypha-install-")
        .tempdir_in(addons_dir)?;
    let staged = work.path().join("staged");
    let backup = work.path().join("backup");
    std::fs::create_dir(&staged)?;
    std::fs::create_dir(&backup)?;
    std::fs::write(staged.join(OWNERSHIP_FILE), serde_json::to_vec(&ownership)?)?;
    changes.push(FileChange {
        rel: OWNERSHIP_FILE.into(),
        install: true,
        expected_hash: registry_hash,
    });
    commit_file_changes(work, addons_dir, &staged, &backup, &changes)?;
    cleanup_empty_dirs(addons_dir, dirs)?;
    Ok((
        owner,
        roots,
        UninstallOutcome {
            preserved_files: target.exists(),
            backup_path: None,
        },
    ))
}

fn add_parent_dirs(rel: &Path, dirs: &mut HashSet<PathBuf>) {
    let mut parent = rel.parent();
    while let Some(dir) = parent {
        if dir.as_os_str().is_empty() {
            break;
        }
        dirs.insert(dir.to_path_buf());
        parent = dir.parent();
    }
}

fn cleanup_empty_dirs(addons_dir: &Path, dirs: HashSet<PathBuf>) -> Result<()> {
    let mut dirs: Vec<_> = dirs.into_iter().collect();
    dirs.sort_by_key(|dir| std::cmp::Reverse(dir.components().count()));
    for dir in dirs {
        let path = addons_dir.join(dir);
        match std::fs::remove_dir(&path) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::NotFound
                ) => {}
            Err(error) => {
                return Err(anyhow!(
                    "addon files were removed, but empty directory cleanup failed at {}: {error}",
                    path.display()
                ))
            }
        }
    }
    Ok(())
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
pub async fn migrate_live_orphans(state: &AppState) {
    let _mutation_guard = state.addon_fs_mutations.lock().await;
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

    #[tokio::test]
    async fn conflict_review_accepts_only_the_current_decision() -> Result<()> {
        let approval = std::sync::Arc::new(InstallConflictApproval::default());
        for (id, approved) in [("first", false), ("second", true)] {
            let respond = approval.clone();
            let channel = Channel::new(move |_| {
                assert!(respond.resolve("stale", true).is_err());
                respond.resolve(id, approved).unwrap();
                assert!(respond.resolve(id, !approved).is_err());
                Ok(())
            });
            let review = InstallConflictReview {
                id: id.to_string(),
                backup_path: String::new(),
                file_count: 0,
                files: Vec::new(),
            };
            assert_eq!(approval.request(&channel, review).await?, approved);
        }
        Ok(())
    }
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use zip::write::SimpleFileOptions;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_uninstalls_serialize_through_the_mutation_path() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons_dir = temp.path().join("AddOns");
        let addon_dir = addons_dir.join("Example");
        std::fs::create_dir_all(&addon_dir)?;
        extract_zip(
            &archive(&[("Example/Example.txt", b"manifest")])?,
            &addons_dir,
            "1",
        )?;
        let conn = db::open(&temp.path().join("catalog.db"))?;
        let state = std::sync::Arc::new(AppState {
            db: std::sync::Mutex::new(conn),
            addons_dir: std::sync::Mutex::new(addons_dir),
            addon_fs_mutations: tokio::sync::Mutex::new(()),
            backups_dir: temp.path().join("backups"),
            mmoui: crate::api::MmouiClient::new(),
            syncing: AtomicBool::new(false),
            close_to_tray: AtomicBool::new(false),
            auto_update_interval_minutes: AtomicU64::new(0),
        });

        let start = std::sync::Arc::new(tokio::sync::Barrier::new(3));
        let first_state = state.clone();
        let first_start = start.clone();
        let first = tokio::spawn(async move {
            first_start.wait().await;
            uninstall_addon(&first_state, "Example").await
        });
        let second_state = state.clone();
        let second_start = start.clone();
        let second = tokio::spawn(async move {
            second_start.wait().await;
            uninstall_addon(&second_state, "Example").await
        });
        start.wait().await;

        let first = first.await?;
        let second = second.await?;
        assert_ne!(first.is_ok(), second.is_ok());
        let failure = first.err().or_else(|| second.err()).unwrap();
        assert!(failure.to_string().contains("addon directory not found"));
        assert!(!addon_dir.exists());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ownership_replacement_and_uninstall_share_the_mutation_guard() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons_dir = temp.path().join("AddOns");
        std::fs::create_dir(&addons_dir)?;
        let state = std::sync::Arc::new(AppState {
            db: std::sync::Mutex::new(db::open(&temp.path().join("catalog.db"))?),
            addons_dir: std::sync::Mutex::new(addons_dir.clone()),
            addon_fs_mutations: tokio::sync::Mutex::new(()),
            backups_dir: temp.path().join("backups"),
            mmoui: crate::api::MmouiClient::new(),
            syncing: AtomicBool::new(false),
            close_to_tray: AtomicBool::new(false),
            auto_update_interval_minutes: AtomicU64::new(0),
        });
        let guard = state.addon_fs_mutations.lock().await;
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let worker_state = state.clone();
        let mut uninstall = tokio::spawn(async move {
            started_tx.send(()).unwrap();
            uninstall_addon(&worker_state, "Example").await
        });
        started_rx.await?;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut uninstall)
                .await
                .is_err()
        );
        extract_zip(
            &archive(&[("Example/Example.txt", b"first")])?,
            &addons_dir,
            "first",
        )?;
        drop(guard);
        uninstall.await??;
        assert!(!addons_dir.join("Example").exists());
        let _guard = state.addon_fs_mutations.lock().await;
        extract_zip(
            &archive(&[("Example/Example.txt", b"second")])?,
            &addons_dir,
            "second",
        )?;
        assert!(extract_zip(
            &archive(&[("Example/Example.txt", b"overwrite")])?,
            &addons_dir,
            "first"
        )
        .is_err());
        assert_eq!(
            std::fs::read(addons_dir.join("Example/Example.txt"))?,
            b"second"
        );
        Ok(())
    }

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

    fn archive(files: &[(&str, &[u8])]) -> Result<Vec<u8>> {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in files {
            archive.start_file(*name, SimpleFileOptions::default())?;
            archive.write_all(data)?;
        }
        Ok(archive.finish()?.into_inner())
    }

    fn legacy_state(temp: &tempfile::TempDir) -> Result<AppState> {
        let addons = temp.path().join("AddOns");
        std::fs::create_dir_all(addons.join("Example"))?;
        Ok(AppState {
            db: std::sync::Mutex::new(db::open(&temp.path().join("catalog.db"))?),
            addons_dir: std::sync::Mutex::new(addons),
            addon_fs_mutations: tokio::sync::Mutex::new(()),
            backups_dir: temp.path().join("backups"),
            mmoui: crate::api::MmouiClient::new(),
            syncing: AtomicBool::new(false),
            close_to_tray: AtomicBool::new(false),
            auto_update_interval_minutes: AtomicU64::new(0),
        })
    }

    #[tokio::test]
    async fn legacy_uninstall_backs_up_selected_files_and_preserves_everything_else() -> Result<()>
    {
        let temp = tempfile::tempdir()?;
        let state = legacy_state(&temp)?;
        let addons = temp.path().join("AddOns");
        std::fs::write(addons.join("Example/Example.txt"), b"legacy manifest")?;
        std::fs::write(addons.join("Example/mine.lua"), b"mine")?;
        std::fs::write(addons.join("Example/unknown.lua"), b"unknown shared")?;
        std::fs::create_dir(addons.join("shared"))?;
        std::fs::write(addons.join("shared/outside"), b"outside")?;
        extract_zip(
            &archive(&[("Example/tracked.lua", b"other addon")])?,
            &addons,
            "other",
        )?;
        {
            let conn = state.db.lock().unwrap();
            db::set_metadata(&conn, "installed_dir:Example", "old")?;
        }
        let preview = preview_uninstall(&state, "Example").await?;
        assert!(preview.legacy);
        assert_eq!(preview.files.len(), 3);
        assert_eq!(preview.protected_count, 1);
        assert!(addons.join("Example/Example.txt").exists());
        let approved: Vec<_> = preview
            .files
            .into_iter()
            .filter(|file| file.manifest || file.path.ends_with("/mine.lua"))
            .collect();
        let approved = serde_json::from_str(&serde_json::to_string(&approved)?)?;
        let result = uninstall_legacy(&state, "Example", &preview.addons_dir, approved).await?;
        assert!(result.preserved_files);
        let backup = result.backup_path.unwrap();
        assert_eq!(
            std::fs::read(backup.join("files/Example/Example.txt"))?,
            b"legacy manifest"
        );
        assert_eq!(
            std::fs::read(backup.join("files/Example/mine.lua"))?,
            b"mine"
        );
        assert!(!addons.join("Example/Example.txt").exists());
        assert!(!addons.join("Example/mine.lua").exists());
        assert_eq!(
            std::fs::read(addons.join("Example/unknown.lua"))?,
            b"unknown shared"
        );
        assert_eq!(
            std::fs::read(addons.join("Example/tracked.lua"))?,
            b"other addon"
        );
        assert_eq!(std::fs::read(addons.join("shared/outside"))?, b"outside");
        let (ownership, _) = load_ownership(&addons)?;
        assert!(ownership
            .files
            .values()
            .any(|entry| entry.addon_id == "other"));
        std::fs::copy(
            backup.join("files/Example/Example.txt"),
            addons.join("Example/Example.txt"),
        )?;
        assert_eq!(
            std::fs::read(addons.join("Example/Example.txt"))?,
            b"legacy manifest"
        );
        assert_eq!(
            std::fs::read(addons.join("Example/tracked.lua"))?,
            b"other addon"
        );
        let conn = state.db.lock().unwrap();
        assert_eq!(
            db::get_metadata(&conn, "installed_dir:Example").as_deref(),
            Some("")
        );
        Ok(())
    }

    #[tokio::test]
    async fn legacy_uninstall_revalidates_approval_and_never_removes_tracked_files() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let state = legacy_state(&temp)?;
        let addons = temp.path().join("AddOns");
        std::fs::write(addons.join("Example/Example.txt"), b"manifest")?;
        std::fs::write(addons.join("Example/user.lua"), b"original")?;
        extract_zip(
            &archive(&[("Example/tracked.lua", b"other")])?,
            &addons,
            "other",
        )?;
        let preview = preview_uninstall(&state, "Example").await?;
        assert!(
            uninstall_legacy(&state, "Example", &preview.addons_dir, Vec::new())
                .await
                .is_err()
        );
        let mut forged = preview.files.clone();
        forged.push(LegacyUninstallFile {
            path: "Example/tracked.lua".into(),
            fingerprint: serde_json::to_string(&hash_file(&addons.join("Example/tracked.lua"))?)?,
            manifest: false,
        });
        assert!(
            uninstall_legacy(&state, "Example", &preview.addons_dir, forged)
                .await
                .is_err()
        );
        let mut outside = preview.files.clone();
        outside.push(LegacyUninstallFile {
            path: "../outside".into(),
            fingerprint: preview.files[0].fingerprint.clone(),
            manifest: false,
        });
        assert!(
            uninstall_legacy(&state, "Example", &preview.addons_dir, outside)
                .await
                .is_err()
        );
        std::fs::write(addons.join("Example/user.lua"), b"edited")?;
        assert!(
            uninstall_legacy(&state, "Example", &preview.addons_dir, preview.files)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(addons.join("Example/Example.txt"))?,
            b"manifest"
        );
        assert_eq!(std::fs::read(addons.join("Example/user.lua"))?, b"edited");
        assert_eq!(std::fs::read(addons.join("Example/tracked.lua"))?, b"other");
        Ok(())
    }

    #[tokio::test]
    async fn legacy_uninstall_requires_all_current_manifests_and_working_backup() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut state = legacy_state(&temp)?;
        let addons = temp.path().join("AddOns");
        std::fs::write(addons.join("Example/Example.txt"), b"manifest")?;
        std::fs::write(addons.join("Example/file"), b"data")?;
        let preview = preview_uninstall(&state, "Example").await?;
        let without_manifest = preview
            .files
            .iter()
            .filter(|file| !file.manifest)
            .cloned()
            .collect();
        assert!(
            uninstall_legacy(&state, "Example", &preview.addons_dir, without_manifest)
                .await
                .is_err()
        );
        std::fs::write(addons.join("Example/Example.addon"), b"new manifest")?;
        assert!(
            uninstall_legacy(&state, "Example", &preview.addons_dir, preview.files)
                .await
                .is_err()
        );
        std::fs::remove_file(addons.join("Example/Example.addon"))?;
        let preview = preview_uninstall(&state, "Example").await?;
        state.backups_dir = addons.join("unsafe-backups");
        assert!(
            uninstall_legacy(&state, "Example", &preview.addons_dir, preview.files)
                .await
                .is_err()
        );
        assert!(!addons.join("unsafe-backups").exists());
        assert_eq!(
            std::fs::read(addons.join("Example/Example.txt"))?,
            b"manifest"
        );
        assert_eq!(std::fs::read(addons.join("Example/file"))?, b"data");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn legacy_review_waits_for_filesystem_mutation_guard() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let state = std::sync::Arc::new(legacy_state(&temp)?);
        std::fs::write(temp.path().join("AddOns/Example/Example.txt"), b"manifest")?;
        let guard = state.addon_fs_mutations.lock().await;
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let worker_state = state.clone();
        let mut review = tokio::spawn(async move {
            started_tx.send(()).unwrap();
            preview_uninstall(&worker_state, "Example").await
        });
        started_rx.await?;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut review)
                .await
                .is_err()
        );
        drop(guard);
        let preview = review.await??;
        assert!(preview.legacy);
        assert_eq!(preview.files.len(), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_review_never_follows_or_removes_links() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let state = legacy_state(&temp)?;
        let addons = temp.path().join("AddOns");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside)?;
        std::fs::write(outside.join("file"), b"keep")?;
        std::fs::write(addons.join("Example/Example.txt"), b"manifest")?;
        std::os::unix::fs::symlink(&outside, addons.join("Example/link"))?;
        let preview = preview_uninstall(&state, "Example").await?;
        assert_eq!(preview.protected_count, 1);
        assert_eq!(preview.files.len(), 1);
        uninstall_legacy(&state, "Example", &preview.addons_dir, preview.files).await?;
        assert_eq!(std::fs::read(outside.join("file"))?, b"keep");
        assert!(std::fs::symlink_metadata(addons.join("Example/link"))?
            .file_type()
            .is_symlink());
        Ok(())
    }

    #[test]
    fn legacy_update_requires_consent_and_preserves_backup() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path().join("AddOns");
        let backups = temp.path().join("backups");
        std::fs::create_dir_all(addons.join("Example"))?;
        std::fs::create_dir_all(addons.join("shared"))?;
        std::fs::write(addons.join("Example/Example.txt"), b"legacy manifest")?;
        std::fs::write(addons.join("shared/conflict"), b"legacy file")?;
        std::fs::write(addons.join("shared/unrelated"), b"other addon")?;
        let bytes = archive(&[
            ("Example/Example.addon", b"new manifest"),
            ("shared/conflict", b"new file"),
        ])?;
        let mut declined = |conflicts: &[FileConflict]| {
            assert_eq!(conflicts.len(), 2);
            Err(anyhow!("declined"))
        };
        assert!(extract_zip_with_resolution(&bytes, &addons, "1", &mut declined).is_err());
        assert_eq!(
            std::fs::read(addons.join("Example/Example.txt"))?,
            b"legacy manifest"
        );
        assert!(!addons.join("Example/Example.addon").exists());
        let mut saved = None;
        extract_zip_with_resolution(&bytes, &addons, "1", &mut |conflicts| {
            let backup = backup_conflicts(&addons, &backups, "1", conflicts)?;
            verify_conflict_backup(&addons, backup.path(), conflicts)?;
            saved = Some(backup.keep());
            Ok(())
        })?;
        let saved = saved.unwrap();
        assert_eq!(
            std::fs::read(saved.join("files/Example/Example.txt"))?,
            b"legacy manifest"
        );
        assert_eq!(
            std::fs::read(saved.join("files/shared/conflict"))?,
            b"legacy file"
        );
        let list = std::fs::read_to_string(saved.join("conflicts.txt"))?;
        assert!(list.contains("Remove: Example"));
        assert!(list.contains("Replace: shared"));
        assert!(!addons.join("Example/Example.txt").exists());
        assert_eq!(
            std::fs::read(addons.join("Example/Example.addon"))?,
            b"new manifest"
        );
        assert_eq!(std::fs::read(addons.join("shared/conflict"))?, b"new file");
        assert_eq!(
            std::fs::read(addons.join("shared/unrelated"))?,
            b"other addon"
        );
        uninstall(&addons, "Example")?;
        assert!(!addons.join("Example/Example.addon").exists());
        assert!(!addons.join("shared/conflict").exists());
        assert_eq!(
            std::fs::read(addons.join("shared/unrelated"))?,
            b"other addon"
        );
        extract_zip(&bytes, &addons, "1")?;
        assert_eq!(
            std::fs::read(addons.join("Example/Example.addon"))?,
            b"new manifest"
        );
        assert_eq!(
            std::fs::read(saved.join("files/Example/Example.txt"))?,
            b"legacy manifest"
        );
        Ok(())
    }

    #[test]
    fn conflict_backup_and_rollback_preserve_originals_if_files_change() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path().join("AddOns");
        let backups = temp.path().join("backups");
        std::fs::create_dir_all(addons.join("Example"))?;
        std::fs::create_dir_all(addons.join("shared"))?;
        std::fs::write(addons.join("Example/Example.txt"), b"legacy")?;
        std::fs::write(addons.join("shared/file"), b"original")?;
        let bytes = archive(&[
            ("Example/Example.addon", b"new manifest"),
            ("shared/file", b"new"),
        ])?;
        let mut saved = None;
        let result = extract_zip_with_resolution(&bytes, &addons, "1", &mut |conflicts| {
            saved = Some(backup_conflicts(&addons, &backups, "1", conflicts)?.keep());
            std::fs::write(addons.join("shared/file"), b"external edit")?;
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(
            std::fs::read(addons.join("Example/Example.txt"))?,
            b"legacy"
        );
        assert!(!addons.join("Example/Example.addon").exists());
        assert_eq!(std::fs::read(addons.join("shared/file"))?, b"external edit");
        assert_eq!(
            std::fs::read(saved.unwrap().join("files/shared/file"))?,
            b"original"
        );
        Ok(())
    }

    #[test]
    fn known_other_owner_cannot_be_approved() -> Result<()> {
        let temp = tempfile::tempdir()?;
        extract_zip(
            &archive(&[("shared/file", b"other")])?,
            temp.path(),
            "other",
        )?;
        let mut called = false;
        let result = extract_zip_with_resolution(
            &archive(&[("shared/file", b"replace")])?,
            temp.path(),
            "new",
            &mut |_| {
                called = true;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!called);
        assert_eq!(std::fs::read(temp.path().join("shared/file"))?, b"other");
        Ok(())
    }

    #[test]
    fn altered_backup_prevents_approval() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path().join("AddOns");
        std::fs::create_dir_all(addons.join("Example"))?;
        std::fs::write(addons.join("Example/file"), b"original")?;
        let result = extract_zip_with_resolution(
            &archive(&[("Example/file", b"new")])?,
            &addons,
            "1",
            &mut |conflicts| {
                let backup =
                    backup_conflicts(&addons, &temp.path().join("backups"), "1", conflicts)?;
                std::fs::write(backup.path().join("files/Example/file"), b"tampered")?;
                verify_conflict_backup(&addons, backup.path(), conflicts)
            },
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(addons.join("Example/file"))?, b"original");
        Ok(())
    }

    #[test]
    fn uninstall_removes_only_owned_files_and_allows_reinstall() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path();
        let first = archive(&[
            ("Example/Example.txt", b"manifest"),
            ("Example/owned", b"mine"),
            ("shared/owned", b"mine"),
        ])?;
        extract_zip(&first, addons, "1")?;
        extract_zip(
            &archive(&[("Example/other", b"other"), ("shared/other", b"other")])?,
            addons,
            "2",
        )?;
        std::fs::write(addons.join("Example/manual"), b"manual")?;
        let (owner, _, result) = uninstall(addons, "Example")?;
        assert_eq!(owner, "1");
        assert!(result.preserved_files);
        assert!(!addons.join("Example/Example.txt").exists());
        assert!(!addons.join("shared/owned").exists());
        assert_eq!(std::fs::read(addons.join("Example/other"))?, b"other");
        assert_eq!(std::fs::read(addons.join("shared/other"))?, b"other");
        assert_eq!(std::fs::read(addons.join("Example/manual"))?, b"manual");
        let (registry, _) = load_ownership(addons)?;
        assert!(registry.files.values().all(|entry| entry.addon_id == "2"));
        extract_zip(&first, addons, "1")?;
        assert_eq!(
            std::fs::read(addons.join("Example/Example.txt"))?,
            b"manifest"
        );
        assert_eq!(std::fs::read(addons.join("Example/manual"))?, b"manual");
        Ok(())
    }

    #[test]
    fn uninstall_refuses_legacy_or_modified_files_without_changes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path();
        std::fs::create_dir(addons.join("Legacy"))?;
        std::fs::write(addons.join("Legacy/Legacy.txt"), b"legacy")?;
        assert!(uninstall(addons, "Legacy").is_err());
        assert_eq!(std::fs::read(addons.join("Legacy/Legacy.txt"))?, b"legacy");
        extract_zip(
            &archive(&[
                ("Example/Example.txt", b"manifest"),
                ("Example/modified", b"original"),
            ])?,
            addons,
            "1",
        )?;
        std::fs::write(addons.join("Example/modified"), b"edited")?;
        assert!(uninstall(addons, "Example").is_err());
        assert_eq!(
            std::fs::read(addons.join("Example/Example.txt"))?,
            b"manifest"
        );
        assert_eq!(std::fs::read(addons.join("Example/modified"))?, b"edited");
        Ok(())
    }

    #[tokio::test]
    async fn uninstall_clears_own_markers_but_preserves_other_pins() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path().join("AddOns");
        std::fs::create_dir(&addons)?;
        extract_zip(
            &archive(&[
                ("Example/Example.txt", b"manifest"),
                ("shared/one", b"file"),
            ])?,
            &addons,
            "1",
        )?;
        let conn = db::open(&temp.path().join("catalog.db"))?;
        db::set_metadata(&conn, "installed:1", "123")?;
        db::set_metadata(&conn, "installed_at:1", "date")?;
        db::set_metadata(&conn, "installed_dir:Example", "1")?;
        db::set_metadata(&conn, "installed_dir:shared", "2")?;
        let state = AppState {
            db: std::sync::Mutex::new(conn),
            addons_dir: std::sync::Mutex::new(addons),
            addon_fs_mutations: tokio::sync::Mutex::new(()),
            backups_dir: temp.path().join("backups"),
            mmoui: crate::api::MmouiClient::new(),
            syncing: AtomicBool::new(false),
            close_to_tray: AtomicBool::new(false),
            auto_update_interval_minutes: AtomicU64::new(0),
        };
        uninstall_addon(&state, "Example").await?;
        let conn = state.db.lock().unwrap();
        assert_eq!(db::get_metadata(&conn, "installed:1").as_deref(), Some(""));
        assert_eq!(
            db::get_metadata(&conn, "installed_at:1").as_deref(),
            Some("")
        );
        assert_eq!(
            db::get_metadata(&conn, "installed_dir:Example").as_deref(),
            Some("")
        );
        assert_eq!(
            db::get_metadata(&conn, "installed_dir:shared").as_deref(),
            Some("2")
        );
        Ok(())
    }

    #[test]
    fn owned_updates_remove_obsolete_manifest_and_preserve_shared_files() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path().join("AddOns");
        std::fs::create_dir(&addons)?;
        extract_zip(
            &archive(&[
                ("Example/Example.txt", b"old manifest"),
                ("gamedata/current.txt", b"old"),
                ("Example/obsolete.lua", b"obsolete"),
            ])?,
            &addons,
            "1",
        )?;
        extract_zip(
            &archive(&[
                ("Example/shared.txt", b"other addon"),
                ("gamedata/other.txt", b"other addon"),
            ])?,
            &addons,
            "2",
        )?;
        extract_zip(
            &archive(&[
                ("Example/Example.addon", b"new"),
                ("gamedata/current.txt", b"updated"),
            ])?,
            &addons,
            "1",
        )?;
        assert!(!addons.join("Example/Example.txt").exists());
        assert!(!addons.join("Example/obsolete.lua").exists());
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
        extract_zip(
            &archive(&[("Example/Example.txt", b"newer")])?,
            &addons,
            "1",
        )?;
        assert_eq!(
            scanner::locate_manifest(&addons.join("Example"), "Example"),
            Some(addons.join("Example/Example.txt"))
        );
        assert!(!addons.join("Example/Example.addon").exists());
        Ok(())
    }

    #[test]
    fn refuses_other_addon_and_legacy_conflicts_before_any_changes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path();
        extract_zip(&archive(&[("shared/file", b"owner")])?, addons, "owner")?;
        std::fs::write(addons.join("shared/manual"), "manual")?;
        for (name, reason) in [
            ("shared/file", "another addon"),
            ("shared/manual", "ownership is unknown"),
        ] {
            let error = extract_zip(
                &archive(&[("shared/new", b"new"), (name, b"overwrite")])?,
                addons,
                "other",
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains(reason), "{error}");
            assert!(!addons.join("shared/new").exists());
        }
        assert_eq!(std::fs::read(addons.join("shared/file"))?, b"owner");
        assert_eq!(std::fs::read(addons.join("shared/manual"))?, b"manual");
        Ok(())
    }

    #[test]
    fn refuses_unknown_or_other_owner_alternate_manifest() -> Result<()> {
        for owned_by_other in [false, true] {
            let temp = tempfile::tempdir()?;
            let addons = temp.path();
            if owned_by_other {
                extract_zip(
                    &archive(&[("Example/Example.txt", b"original")])?,
                    addons,
                    "other",
                )?;
            } else {
                std::fs::create_dir(addons.join("Example"))?;
                std::fs::write(addons.join("Example/Example.txt"), b"original")?;
            }
            let err = extract_zip(
                &archive(&[("Example/Example.addon", b"new")])?,
                addons,
                "new",
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("alternate manifest"), "{err}");
            assert_eq!(
                std::fs::read(addons.join("Example/Example.txt"))?,
                b"original"
            );
            assert!(!addons.join("Example/Example.addon").exists());
        }
        Ok(())
    }

    #[test]
    fn refuses_archives_with_both_manifest_extensions() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let bytes = archive(&[
            ("Example/Example.txt", b"txt"),
            ("Example/Example.addon", b"addon"),
        ])?;
        let error = extract_zip(&bytes, temp.path(), "1")
            .unwrap_err()
            .to_string();
        assert!(error.contains("both manifests"), "{error}");
        assert!(!temp.path().join("Example").exists());
        Ok(())
    }

    #[test]
    fn refuses_modified_owned_file_and_corrupt_registry() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path();
        extract_zip(&archive(&[("Example/file", b"original")])?, addons, "1")?;
        std::fs::write(addons.join("Example/file"), b"edited")?;
        let err = extract_zip(&archive(&[("Example/file", b"new")])?, addons, "1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("modified"), "{err}");
        assert_eq!(std::fs::read(addons.join("Example/file"))?, b"edited");
        std::fs::write(addons.join(OWNERSHIP_FILE), b"invalid")?;
        assert!(extract_zip(&archive(&[("Example/other", b"new")])?, addons, "1").is_err());
        assert!(!addons.join("Example/other").exists());
        Ok(())
    }

    #[test]
    fn refuses_recreated_file_even_when_content_matches() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path();
        extract_zip(&archive(&[("Example/file", b"original")])?, addons, "1")?;
        std::fs::rename(addons.join("Example/file"), addons.join("original-backup"))?;
        std::fs::write(addons.join("Example/file"), b"original")?;
        let err = extract_zip(&archive(&[("Example/file", b"new")])?, addons, "1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("modified"), "{err}");
        assert_eq!(std::fs::read(addons.join("Example/file"))?, b"original");
        Ok(())
    }

    #[test]
    fn refuses_modified_obsolete_manifest_and_unfinished_recovery() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path();
        extract_zip(
            &archive(&[("Example/Example.txt", b"original")])?,
            addons,
            "1",
        )?;
        std::fs::write(addons.join("Example/Example.txt"), b"modified")?;
        let bytes = archive(&[("Example/Example.addon", b"new")])?;
        assert!(extract_zip(&bytes, addons, "1")
            .unwrap_err()
            .to_string()
            .contains("modified"));
        assert_eq!(
            std::fs::read(addons.join("Example/Example.txt"))?,
            b"modified"
        );
        assert!(!addons.join("Example/Example.addon").exists());
        std::fs::create_dir(addons.join(".apocrypha-install-recovery"))?;
        assert!(extract_zip(&bytes, addons, "1")
            .unwrap_err()
            .to_string()
            .contains("unfinished installation"));
        Ok(())
    }

    #[test]
    fn file_claim_can_transfer_only_after_old_file_is_moved() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path().join("AddOns");
        std::fs::create_dir(&addons)?;
        extract_zip(&archive(&[("shared/file", b"owner")])?, &addons, "owner")?;
        let bytes = archive(&[("shared/file", b"other")])?;
        assert!(extract_zip(&bytes, &addons, "other").is_err());
        std::fs::rename(addons.join("shared/file"), temp.path().join("owner-backup"))?;
        extract_zip(&bytes, &addons, "other")?;
        assert_eq!(std::fs::read(temp.path().join("owner-backup"))?, b"owner");
        assert_eq!(std::fs::read(addons.join("shared/file"))?, b"other");
        assert!(extract_zip(
            &archive(&[("shared/file", b"overwrite")])?,
            &addons,
            "owner"
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn restores_removed_manifest_when_replacement_fails() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let addons = temp.path().join("addons");
        let stage = temp.path().join("stage");
        let backup = temp.path().join("backup");
        for dir in [&addons, &stage, &backup] {
            std::fs::create_dir(dir)?;
        }
        std::fs::write(addons.join("old.txt"), "old")?;
        std::fs::write(stage.join("new.addon"), "new")?;
        let changes = [
            FileChange {
                rel: "old.txt".into(),
                install: false,
                expected_hash: Some(hash_file(&addons.join("old.txt"))?),
            },
            FileChange {
                rel: "new.addon".into(),
                install: true,
                expected_hash: None,
            },
        ];
        let mut calls = 0;
        let (_, rollback_failed) = replace_files(&addons, &stage, &backup, &changes, |from, to| {
            calls += 1;
            if calls == 2 {
                Err(std::io::Error::other("injected failure"))
            } else {
                std::fs::rename(from, to)
            }
        })
        .unwrap_err();
        assert!(!rollback_failed);
        assert_eq!(std::fs::read(addons.join("old.txt"))?, b"old");
        assert!(!addons.join("new.addon").exists());
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
            assert!(extract_zip(&archive.finish()?.into_inner(), addons, "1").is_err());
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
        assert!(extract_zip(&bytes, addons, "1").is_err());
        assert_eq!(std::fs::read(addons.join("Example/file.txt"))?, b"old");
        Ok(())
    }

    #[test]
    fn destination_conflict_leaves_files_untouched() -> Result<()> {
        let temp = tempfile::tempdir()?;
        std::fs::create_dir(temp.path().join("Example"))?;
        std::fs::write(temp.path().join("Example/a"), "old")?;
        std::fs::create_dir(temp.path().join("Example/b"))?;
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for name in ["Example/a", "Example/b"] {
            archive.start_file(name, SimpleFileOptions::default())?;
            archive.write_all(b"new")?;
        }
        assert!(extract_zip(&archive.finish()?.into_inner(), temp.path(), "1").is_err());
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
        assert!(extract_zip(&archive.finish()?.into_inner(), &addons, "1").is_err());
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
            &[
                FileChange {
                    rel: "a".into(),
                    install: true,
                    expected_hash: Some(hash_file(&addons.join("a"))?),
                },
                FileChange {
                    rel: "b".into(),
                    install: true,
                    expected_hash: Some(hash_file(&addons.join("b"))?),
                },
            ],
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
        let (error, rollback_failed) = replace_files(
            &addons,
            &stage,
            &backup,
            &[FileChange {
                rel: "a".into(),
                install: true,
                expected_hash: Some(hash_file(&addons.join("a"))?),
            }],
            |from, to| {
                calls += 1;
                if calls >= 2 {
                    Err(std::io::Error::other("injected failure"))
                } else {
                    std::fs::rename(from, to)
                }
            },
        )
        .unwrap_err();
        assert!(rollback_failed);
        assert!(error.to_string().contains("rollback errors"));
        assert_eq!(std::fs::read(backup.join("a"))?, b"old");
        Ok(())
    }
}
