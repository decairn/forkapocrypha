use anyhow::{anyhow, Result};
use chrono::Utc;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::models::Snapshot;

/// Resolve the ESO SavedVariables folder from the configured AddOns dir.
/// Both live as siblings inside `live/`:
///   live/AddOns/
///   live/SavedVariables/
pub fn savedvars_dir_for(addons_dir: &Path) -> Option<PathBuf> {
    addons_dir.parent().map(|p| p.join("SavedVariables"))
}

const TRIGGER_DELIM: &str = "__";
const STAGING_PREFIX: &str = ".incomplete-";

fn is_staging_name(name: &str) -> bool {
    name.get(..STAGING_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(STAGING_PREFIX))
}

fn parse_id(id: &str) -> Option<(String, String, Option<String>)> {
    // "2026-05-23_14-32-15_manual" → (created_at_rfc3339, kind, trigger)
    let mut parts = id.splitn(3, '_');
    let date = parts.next()?;
    let time = parts.next()?;
    let rest = parts.next()?;
    let (kind, trigger) = match rest.split_once(TRIGGER_DELIM) {
        Some((kind, trigger)) => (kind, Some(trigger.to_string())),
        None => (rest, None),
    };
    // Reformat the timestamp to an ISO string for the frontend.
    let time = time.split('~').next()?.replace('-', ":");
    let iso = format!("{}T{}Z", date, time);
    Some((iso, kind.to_string(), trigger))
}

fn measure(path: &Path) -> std::io::Result<(u32, u64)> {
    let mut count = 0u32;
    let mut size = 0u64;
    for entry in fs::read_dir(path)? {
        let meta = entry?.metadata()?;
        if meta.is_file() {
            count += 1;
            size += meta.len();
        }
    }
    Ok((count, size))
}

fn snapshot_from_dir(path: &Path) -> Option<Snapshot> {
    let id = path.file_name()?.to_string_lossy().to_string();
    let (created_at, kind, trigger) = parse_id(&id)?;
    let (file_count, total_size_bytes) = measure(path).ok()?;
    if file_count == 0 {
        return None;
    }
    Some(Snapshot {
        id,
        created_at,
        kind,
        file_count,
        total_size_bytes,
        trigger,
    })
}

pub fn list(backups_dir: &Path) -> Result<Vec<Snapshot>> {
    if !backups_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(backups_dir)? {
        let entry = entry?;
        if is_staging_name(&entry.file_name().to_string_lossy()) || !entry.file_type()?.is_dir() {
            continue;
        }
        if let Some(snap) = snapshot_from_dir(&entry.path()) {
            out.push(snap);
        }
    }
    // newest first
    out.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(out)
}

/// Copy every file (not subdirs) from `savedvars_dir` into a new timestamped
/// folder under `backups_dir`. Returns the metadata of the created snapshot.
pub fn create(
    backups_dir: &Path,
    savedvars_dir: &Path,
    kind: &str,
    trigger: Option<&str>,
) -> Result<Snapshot> {
    if !savedvars_dir.exists() {
        return Err(anyhow!("SavedVariables folder does not exist"));
    }
    let mut kind_and_trigger = kind.to_string();
    if let Some(t) = trigger {
        kind_and_trigger.push_str(TRIGGER_DELIM);
        kind_and_trigger.push_str(t);
    }
    let now = Utc::now();
    let timestamp = now.format("%Y-%m-%d_%H-%M-%S").to_string();
    fs::create_dir_all(backups_dir)?;
    let staging = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(backups_dir)?;

    let mut file_count = 0u32;
    let mut total_size = 0u64;
    for entry in fs::read_dir(savedvars_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let dest = staging.path().join(entry.file_name());
        let bytes = fs::copy(entry.path(), &dest)?;
        file_count += 1;
        total_size += bytes;
    }

    if file_count == 0 {
        return Err(anyhow!("SavedVariables folder is empty"));
    }

    let id = publish(staging.path(), backups_dir, &timestamp, &kind_and_trigger)?;
    let _ = staging.keep();

    Ok(Snapshot {
        id,
        created_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        kind: kind.to_string(),
        file_count,
        total_size_bytes: total_size,
        trigger: trigger.map(String::from),
    })
}

fn publish(staging: &Path, backups_dir: &Path, timestamp: &str, kind: &str) -> Result<String> {
    for sequence in 0..=u32::MAX {
        // Fixed width keeps same-second snapshots sorted in publication order.
        let id = if sequence == 0 {
            format!("{timestamp}_{kind}")
        } else {
            format!("{timestamp}~{sequence:010}_{kind}")
        };
        let target = snapshot_path(backups_dir, &id)?;
        match fs::symlink_metadata(&target) {
            Ok(_) => continue,
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        // If another writer claims the same name, its published directory is
        // nonempty, so rename cannot replace it on Windows or Unix.
        match fs::rename(staging, &target) {
            Ok(()) => return Ok(id),
            Err(_) if fs::symlink_metadata(&target).is_ok() => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Err(anyhow!("Could not allocate a snapshot id"))
}

fn snapshot_path(backups_dir: &Path, id: &str) -> Result<PathBuf> {
    if id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || id.is_empty()
        || id == "."
        || is_staging_name(id)
    {
        return Err(anyhow!("invalid snapshot id"));
    }
    Ok(backups_dir.join(id))
}

pub fn restore(backups_dir: &Path, id: &str, savedvars_dir: &Path) -> Result<u32> {
    let source = snapshot_path(backups_dir, id)?;
    if !source.exists() || !source.is_dir() {
        return Err(anyhow!("snapshot not found"));
    }
    std::fs::create_dir_all(savedvars_dir)?;

    let mut restored = 0u32;
    for entry in std::fs::read_dir(&source)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let dest = savedvars_dir.join(entry.file_name());
        std::fs::copy(entry.path(), &dest)?;
        restored += 1;
    }
    Ok(restored)
}

pub fn delete(backups_dir: &Path, id: &str) -> Result<()> {
    let target = snapshot_path(backups_dir, id)?;
    if !target.exists() {
        return Err(anyhow!("snapshot not found"));
    }
    // Canonicalize and ensure it's inside backups_dir.
    let canon_root = backups_dir.canonicalize()?;
    let canon_target = target.canonicalize()?;
    if !canon_target.starts_with(&canon_root) {
        return Err(anyhow!("snapshot escapes backups folder"));
    }
    std::fs::remove_dir_all(&canon_target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumeration_failure_does_not_publish_a_snapshot() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let backups = temp.path().join("backups");
        let source = temp.path().join("SavedVariables");
        fs::write(&source, "This must be a directory")?;

        assert!(create(&backups, &source, "manual", None).is_err());
        assert!(list(&backups)?.is_empty());
        assert_eq!(fs::read_dir(&backups)?.count(), 0);
        Ok(())
    }

    #[test]
    fn staging_is_hidden_and_legacy_snapshots_can_be_restored() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let backups = temp.path();
        let legacy_id = "2026-05-23_14-32-15_manual";
        let legacy = backups.join(legacy_id);
        let staging_id = ".incomplete-2026-05-23_14-32-16_manual";
        let staging = backups.join(staging_id);
        fs::create_dir(&legacy)?;
        fs::write(legacy.join("Example.lua"), "legacy")?;
        fs::create_dir(backups.join("2026-05-23_14-32-14_manual"))?;
        fs::create_dir(&staging)?;
        fs::write(staging.join("Example.lua"), "partial")?;

        let snapshots = list(backups)?;
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].id, legacy_id);
        let restored = backups.join("restored");
        assert!(restore(backups, staging_id, &restored).is_err());
        assert!(delete(backups, staging_id).is_err());
        assert_eq!(restore(backups, legacy_id, &restored)?, 1);
        assert_eq!(fs::read_to_string(restored.join("Example.lua"))?, "legacy");
        Ok(())
    }

    #[test]
    fn concurrent_publication_preserves_existing_snapshots() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let backups = temp.path();
        let timestamp = "2026-05-23_14-32-15";
        let kind = "auto__Example_Addon";
        let existing = backups.join(format!("{timestamp}_{kind}"));
        fs::create_dir(&existing)?;
        fs::write(existing.join("Example.lua"), "existing")?;
        let first_staged = tempfile::Builder::new()
            .prefix(STAGING_PREFIX)
            .tempdir_in(backups)?;
        let second_staged = tempfile::Builder::new()
            .prefix(STAGING_PREFIX)
            .tempdir_in(backups)?;
        fs::write(first_staged.path().join("Example.lua"), "first")?;
        fs::write(second_staged.path().join("Example.lua"), "second")?;
        let start = std::sync::Barrier::new(2);
        let publish_snapshot = |staged: &tempfile::TempDir| {
            start.wait();
            publish(staged.path(), backups, timestamp, kind)
        };

        std::thread::scope(|scope| -> Result<()> {
            let first = scope.spawn(|| publish_snapshot(&first_staged));
            let second = scope.spawn(|| publish_snapshot(&second_staged));
            let first_id = first.join().unwrap()?;
            let second_id = second.join().unwrap()?;
            assert_ne!(first_id, second_id);
            assert_eq!(
                fs::read_to_string(backups.join(first_id).join("Example.lua"))?,
                "first"
            );
            assert_eq!(
                fs::read_to_string(backups.join(second_id).join("Example.lua"))?,
                "second"
            );
            Ok(())
        })?;

        assert_eq!(
            fs::read_to_string(existing.join("Example.lua"))?,
            "existing"
        );
        let snapshots = list(backups)?;
        assert_eq!(snapshots.len(), 3);
        assert!(snapshots[0].id.contains("~0000000002_"));
        for snapshot in snapshots {
            assert_eq!(snapshot.created_at, "2026-05-23T14:32:15Z");
            assert_eq!(snapshot.kind, "auto");
            assert_eq!(snapshot.trigger.as_deref(), Some("Example_Addon"));
            assert_eq!(snapshot.file_count, 1);
        }
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn copy_failure_does_not_publish_and_can_be_retried() -> Result<()> {
        use std::os::windows::fs::OpenOptionsExt;

        let temp = tempfile::tempdir()?;
        let backups = temp.path().join("backups");
        let source = temp.path().join("SavedVariables");
        fs::create_dir(&source)?;
        fs::write(source.join("First.lua"), "first")?;
        let locked_path = source.join("Locked.lua");
        fs::write(&locked_path, "locked")?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(locked_path)?;

        assert!(create(&backups, &source, "manual", None).is_err());
        assert!(list(&backups)?.is_empty());
        assert_eq!(fs::read_dir(&backups)?.count(), 0);

        drop(lock);
        let snapshot = create(&backups, &source, "manual", None)?;
        assert_eq!(snapshot.file_count, 2);
        let snapshots = list(&backups)?;
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].id, snapshot.id);
        let restored = temp.path().join("restored");
        assert_eq!(restore(&backups, &snapshot.id, &restored)?, 2);
        assert_eq!(fs::read_to_string(restored.join("Locked.lua"))?, "locked");
        Ok(())
    }
}
