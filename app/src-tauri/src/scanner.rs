use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::models::InstalledAddon;

/// Resolve the default ESO AddOns folder for the current OS.
///
/// Windows: `~/Documents/Elder Scrolls Online/live/AddOns`
/// Linux (Steam Proton/Steam Deck):
///   `~/.local/share/Steam/steamapps/compatdata/306130/pfx/drive_c/users/steamuser/Documents/Elder Scrolls Online/live/AddOns`
pub fn default_addons_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        dirs::document_dir().map(|d| d.join("Elder Scrolls Online").join("live").join("AddOns"))
    }
    #[cfg(target_os = "linux")]
    {
        dirs::data_dir().map(|d| {
            d.join("Steam")
                .join("steamapps")
                .join("compatdata")
                .join("306130")
                .join("pfx")
                .join("drive_c")
                .join("users")
                .join("steamuser")
                .join("Documents")
                .join("Elder Scrolls Online")
                .join("live")
                .join("AddOns")
        })
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        dirs::document_dir().map(|d| d.join("Elder Scrolls Online").join("live").join("AddOns"))
    }
}

/// Directories the ESO client itself drops inside `AddOns/` as part of
/// translation packs (and similar). Minion treats them as belonging to the
/// addon that shipped them — one catalog entry, one row in the UI. We do
/// the same by hiding them from the installed list. They're rooted by the
/// catalog row's directory list, not by their own manifest.
fn is_aux_client_dir(name: &str) -> bool {
    matches!(name, "gamedata" | "EsoUI" | "fonts" | "lang")
}

pub fn scan(addons_dir: &Path) -> Result<Vec<InstalledAddon>> {
    if !addons_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(addons_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        if dir_name.starts_with('.') {
            continue;
        }
        if is_aux_client_dir(&dir_name) {
            continue;
        }
        let dir_path = entry.path();
        let manifest = locate_manifest(&dir_path, &dir_name);
        if manifest.is_none() && !has_remaining_entries(&dir_path) {
            continue;
        }
        let mut addon = match manifest {
            Some(p) => parse_manifest(&p, &dir_name).unwrap_or_else(|_| empty(&dir_name)),
            None => empty(&dir_name),
        };
        addon.dir_name = dir_name;
        out.push(addon);
    }
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    Ok(out)
}

// Keep unreadable directories and links visible, since they may contain user data.
pub(crate) fn has_remaining_entries(dir: &Path) -> bool {
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return true,
        };
        for entry in entries {
            let Ok(entry) = entry else { return true };
            match entry.file_type() {
                Ok(ty) if ty.is_dir() => pending.push(entry.path()),
                _ => return true,
            }
        }
    }
    false
}

pub(crate) fn locate_manifest(dir: &Path, name: &str) -> Option<PathBuf> {
    let txt = dir.join(format!("{}.txt", name));
    let addon = dir.join(format!("{}.addon", name));
    if txt.exists() {
        return Some(txt);
    }
    if addon.exists() {
        return Some(addon);
    }
    None
}

fn empty(dir_name: &str) -> InstalledAddon {
    InstalledAddon {
        dir_name: dir_name.to_string(),
        title: dir_name.to_string(),
        version: None,
        author: None,
        description: None,
        depends_on: vec![],
        optional_depends_on: vec![],
        catalog_id: None,
        catalog_version: None,
        category_id: None,
        thumbnail_url: None,
        update_available: false,
        catalog_last_updated: None,
        installed_at: None,
        unused_lib: false,
    }
}

fn parse_manifest(path: &Path, fallback_name: &str) -> Result<InstalledAddon> {
    let content = read_lossy(path)?;
    let mut title: Option<String> = None;
    let mut version: Option<String> = None;
    let mut author: Option<String> = None;
    let mut description: Option<String> = None;
    let mut depends_on: Vec<String> = Vec::new();
    let mut optional_depends_on: Vec<String> = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if !line.starts_with("##") {
            continue;
        }
        let body = line.trim_start_matches('#').trim();
        let (key, value) = match body.split_once(':') {
            Some(parts) => (parts.0.trim(), parts.1.trim()),
            None => continue,
        };
        match key {
            "Title" => title = Some(strip_color_codes(value)),
            "Version" => version = Some(value.to_string()),
            "Author" => author = Some(strip_color_codes(value)),
            "Description" => description = Some(strip_color_codes(value)),
            "DependsOn" | "PCDependsOn" | "Dependencies" => {
                depends_on.extend(parse_dep_list(value));
            }
            "OptionalDependsOn" => {
                optional_depends_on.extend(parse_dep_list(value));
            }
            _ => {}
        }
    }

    Ok(InstalledAddon {
        dir_name: fallback_name.to_string(),
        title: title.unwrap_or_else(|| fallback_name.to_string()),
        version,
        author,
        description,
        depends_on,
        optional_depends_on,
        catalog_id: None,
        catalog_version: None,
        category_id: None,
        thumbnail_url: None,
        update_available: false,
        catalog_last_updated: None,
        installed_at: None,
        unused_lib: false,
    })
}

pub(crate) fn read_lossy(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn parse_dep_list(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(|tok| {
            // Strip version constraints like "LibFoo>=1.2" or "LibBar<2.0"
            tok.split(|c: char| c == '>' || c == '<' || c == '=')
                .next()
                .unwrap_or(tok)
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn strip_color_codes(s: &str) -> String {
    // ESO color codes look like |cRRGGBB ... |r
    let mut out = String::with_capacity(s.len());
    let mut remaining = s;
    while let Some(ch) = remaining.chars().next() {
        let bytes_consumed = match remaining.as_bytes() {
            [b'|', b'c' | b'C', digits @ ..]
                if digits
                    .get(..6)
                    .is_some_and(|rgb| rgb.iter().all(u8::is_ascii_hexdigit)) =>
            {
                8
            }
            [b'|', b'r' | b'R', ..] => 2,
            _ => {
                out.push(ch);
                ch.len_utf8()
            }
        };
        remaining = &remaining[bytes_consumed..];
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_non_ascii_manifest_text() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let manifest = dir.path().join("Example.addon");
        std::fs::write(
            &manifest,
            "## Title: Café\n## Author: |cFF8800Renée|r\n## Description: 日|C11aabb本語|R 🌿\n",
        )?;

        let addon = parse_manifest(&manifest, "Example")?;
        assert_eq!(addon.title, "Café");
        assert_eq!(addon.author.as_deref(), Some("Renée"));
        assert_eq!(addon.description.as_deref(), Some("日本語 🌿"));
        Ok(())
    }

    #[test]
    fn preserves_incomplete_and_invalid_color_codes() {
        assert_eq!(strip_color_codes("|c12345é |C123"), "|c12345é |C123");
    }

    #[test]
    fn skips_empty_directory_trees_but_lists_leftover_files() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("LibRemoved/datastructures/test"))?;
        std::fs::create_dir_all(dir.path().join("LibPartial/nested"))?;
        std::fs::write(dir.path().join("LibPartial/nested/settings.lua"), b"settings")?;

        let installed = scan(dir.path())?;
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].dir_name, "LibPartial");
        Ok(())
    }
}
