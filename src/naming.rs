//! Copy-name patterns and the "canonical" name of a duplicate group.
//!
//! Operating systems and browsers decorate copies in predictable ways:
//! `report (1).pdf`, `report copy.pdf`, `report - Copy (2).pdf`,
//! `Copy of report.pdf`, `report_2.pdf`. Stripping those markers from every
//! member of a group yields a base name; the most common base plus the
//! most common extension is the group's canonical name. It is used to
//! recognise copies, to prefer the file that already carries the clean
//! name as the keeper, and to offer a rename when no surviving copy does.
//!
//! Names never decide what is a duplicate. Size and SHA-256 do that; names
//! only decide which copy to keep and what to call it.

use crate::duplicates::DuplicateGroup;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Extensions that are really two parts.
const COMPOUND_EXTENSIONS: [&str; 6] = [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".user.js", ".d.ts"];

/// Split `report (1).tar.gz` into (`report (1)`, `.tar.gz`). Dotfiles like
/// `.bashrc` have no extension.
pub fn split_name(name: &str) -> (&str, &str) {
    let lower = name.to_ascii_lowercase();
    for ext in COMPOUND_EXTENSIONS {
        if lower.ends_with(ext) && lower.len() > ext.len() {
            let cut = name.len() - ext.len();
            return (&name[..cut], &name[cut..]);
        }
    }
    match name.rfind('.') {
        Some(pos) if pos > 0 && pos + 1 < name.len() => (&name[..pos], &name[pos..]),
        _ => (name, ""),
    }
}

/// Strip copy markers from a stem. Returns the base and whether anything
/// was stripped. Original casing of the base is preserved.
///
/// Recognised, repeatedly and in any order at the end of the stem:
/// ` (n)`, `(n)`, ` copy`, ` - copy`, `-copy`, `_copy`, ` copy n`,
/// ` duplicate`, `_duplicate`, ` n` / `-n` / `_n` (a single digit 2..9);
/// and at the start: `copy of `, `kopie von `.
pub fn strip_copy_markers(stem: &str) -> (String, bool) {
    let mut s = stem.trim().to_string();
    let mut stripped = false;

    loop {
        let lower = s.to_ascii_lowercase();
        let before = s.len();

        // Prefixes
        for prefix in ["copy of ", "kopie von ", "copie de "] {
            if lower.starts_with(prefix) && lower.len() > prefix.len() {
                s = s[prefix.len()..].to_string();
                stripped = true;
            }
        }

        let lower = s.to_ascii_lowercase();
        let trimmed = lower.trim_end();
        let mut cut: Option<usize> = None;

        // "(n)" with optional space before it
        if trimmed.ends_with(')') {
            if let Some(open) = trimmed.rfind('(') {
                let inner = &trimmed[open + 1..trimmed.len() - 1];
                if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
                    cut = Some(open);
                }
            }
        }
        // "... copy", "... - copy", "... copy 2", "... duplicate"
        if cut.is_none() {
            for marker in [" - copy", " copy", "-copy", "_copy", " duplicate", "_duplicate", "-duplicate"] {
                if let Some(pos) = trimmed.rfind(marker) {
                    let tail = &trimmed[pos + marker.len()..];
                    let tail_ok = tail.is_empty()
                        || (tail.trim().chars().all(|c| c.is_ascii_digit()) && tail.trim().len() <= 3 && tail.starts_with(' '));
                    if tail_ok && pos > 0 {
                        cut = Some(pos);
                        break;
                    }
                }
            }
        }
        // "... 2", "...-2", "..._2": a single digit 2..9 after a separator
        if cut.is_none() {
            let chars: Vec<char> = trimmed.chars().collect();
            if chars.len() >= 3 {
                let last = chars[chars.len() - 1];
                let sep = chars[chars.len() - 2];
                if ('2'..='9').contains(&last) && matches!(sep, ' ' | '-' | '_') {
                    let byte_cut = trimmed.char_indices().nth(chars.len() - 2).map(|(i, _)| i);
                    cut = byte_cut;
                }
            }
        }

        if let Some(c) = cut {
            s = s[..c].trim_end_matches([' ', '-', '_']).to_string();
            stripped = true;
        }
        if s.len() == before || s.is_empty() {
            break;
        }
    }

    if s.is_empty() {
        return (stem.to_string(), false);
    }
    (s, stripped)
}

/// Does this file name carry a copy marker, given the other stems in its
/// group? Unambiguous markers always count; a bare trailing digit only
/// counts when the base is also present in the group.
pub fn has_copy_marker(name: &str, group_bases: &HashMap<String, usize>) -> bool {
    let (stem, _) = split_name(name);
    let (base, stripped) = strip_copy_markers(stem);
    if !stripped {
        return false;
    }
    let lower = stem.to_ascii_lowercase();
    let unambiguous = lower.contains("copy")
        || lower.contains("kopie")
        || lower.contains("copie")
        || lower.contains("duplicate")
        || lower.trim_end().ends_with(')');
    if unambiguous {
        return true;
    }
    // Only the "name 2" style remains: require the base to exist in the group
    // as an actual file name, not merely as another stripped base.
    group_bases.get(&base.to_ascii_lowercase()).copied().unwrap_or(0) > 0
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CanonicalName {
    /// `base + extension`, e.g. `report.pdf`.
    pub name: String,
    /// Index of a file that already has exactly this name, if any.
    pub existing: Option<usize>,
}

/// Work out the canonical name of a group from its members' names.
/// Returns `None` when no member carries a copy marker (nothing to say).
pub fn canonical_name(group: &DuplicateGroup) -> Option<CanonicalName> {
    canonical_name_of(&group.files)
}

/// Same as `canonical_name`, for a slice of files in whatever order the
/// caller uses; `existing` indexes into that slice.
pub fn canonical_name_of(files: &[crate::scanner::FileInfo]) -> Option<CanonicalName> {
    let names: Vec<String> = files
        .iter()
        .map(|f| f.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
        .collect();
    if names.len() < 2 {
        return None;
    }

    let mut any_marker = false;
    let mut base_votes: HashMap<String, (usize, String)> = HashMap::new(); // lower -> (count, display)
    let mut ext_votes: HashMap<String, (usize, String)> = HashMap::new();
    for name in &names {
        let (stem, ext) = split_name(name);
        let (base, stripped) = strip_copy_markers(stem);
        any_marker |= stripped;
        let e = base_votes.entry(base.to_ascii_lowercase()).or_insert((0, base.clone()));
        e.0 += 1;
        // Prefer the spelling of an unstripped name for display.
        if !stripped {
            e.1 = base.clone();
        }
        let x = ext_votes.entry(ext.to_ascii_lowercase()).or_insert((0, ext.to_string()));
        x.0 += 1;
    }
    if !any_marker {
        return None;
    }

    let base = base_votes
        .values()
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.len().cmp(&a.1.len())).then_with(|| b.1.cmp(&a.1)))
        .map(|(_, display)| display.clone())?;
    let ext = ext_votes
        .values()
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)))
        .map(|(_, display)| display.clone())
        .unwrap_or_default();
    let name = format!("{base}{ext}");
    let existing = names.iter().position(|n| n.eq_ignore_ascii_case(&name));
    Some(CanonicalName { name, existing })
}

/// Lower-cased bases of the file names that appear literally in the group
/// (used by `has_copy_marker` to confirm "name 2" style copies).
pub fn literal_bases(group_files: &[crate::scanner::FileInfo]) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for f in group_files {
        if let Some(name) = f.path.file_name() {
            let name = name.to_string_lossy();
            let (stem, _) = split_name(&name);
            *m.entry(stem.to_ascii_lowercase()).or_insert(0) += 1;
        }
    }
    m
}

/// If the keeper does not already carry the canonical name and that name is
/// free in the keeper's directory, propose renaming it.
pub fn suggested_rename(group: &DuplicateGroup, keeper: usize) -> Option<(PathBuf, String)> {
    let canonical = canonical_name(group)?;
    let keeper_file = group.files.get(keeper)?;
    let current = keeper_file.path.file_name()?.to_string_lossy().to_string();
    if current.eq_ignore_ascii_case(&canonical.name) {
        return None;
    }
    if canonical.existing.is_some() {
        // Another copy already has the clean name; the keeper choice should
        // prefer it instead of renaming.
        return None;
    }
    let target = keeper_file.path.parent().unwrap_or(Path::new("")).join(&canonical.name);
    if target.exists() {
        return None;
    }
    Some((keeper_file.path.clone(), canonical.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::FileInfo;
    use std::time::SystemTime;

    fn info(path: &str) -> FileInfo {
        FileInfo {
            path: PathBuf::from(path),
            size: 10,
            quick_hash: None,
            hash: Some("h".into()),
            modified: SystemTime::UNIX_EPOCH,
            depth: 3,
        }
    }

    #[test]
    fn splitting_names() {
        assert_eq!(split_name("report (1).pdf"), ("report (1)", ".pdf"));
        assert_eq!(split_name("archive.tar.gz"), ("archive", ".tar.gz"));
        assert_eq!(split_name(".bashrc"), (".bashrc", ""));
        assert_eq!(split_name("README"), ("README", ""));
        assert_eq!(split_name("v1.2.3"), ("v1.2", ".3"));
    }

    #[test]
    fn stripping_markers() {
        for (input, base) in [
            ("report (1)", "report"),
            ("report(2)", "report"),
            ("report copy", "report"),
            ("report - Copy", "report"),
            ("report - Copy (2)", "report"),
            ("report copy 3", "report"),
            ("Copy of report", "report"),
            ("report_copy", "report"),
            ("report duplicate", "report"),
            ("IMG_0001 2", "IMG_0001"),
            ("report copy (1) copy", "report"),
        ] {
            let (b, stripped) = strip_copy_markers(input);
            assert_eq!(b, base, "{input}");
            assert!(stripped, "{input}");
        }
        for clean in ["report", "chapter 12", "img_2024", "copyright", "(1)", "Copy"] {
            let (b, stripped) = strip_copy_markers(clean);
            assert_eq!(b, clean);
            assert!(!stripped, "{clean}");
        }
    }

    #[test]
    fn copy_marker_detection_uses_group_context() {
        let g = DuplicateGroup::new("h".into(), vec![info("/x/photo.jpg"), info("/x/photo 2.jpg")]);
        let bases = literal_bases(&g.files);
        assert!(has_copy_marker("photo 2.jpg", &bases));
        assert!(has_copy_marker("photo (1).jpg", &HashMap::new()));
        assert!(has_copy_marker("photo copy.jpg", &HashMap::new()));
        assert!(!has_copy_marker("file_2.txt", &HashMap::new()));
        assert!(!has_copy_marker("chapter 12.md", &bases));
    }

    #[test]
    fn canonical_name_prefers_the_clean_member() {
        let g = DuplicateGroup::new(
            "h".into(),
            vec![info("/a/Report (1).pdf"), info("/b/report.pdf"), info("/c/Report copy.pdf")],
        );
        let c = canonical_name(&g).unwrap();
        assert_eq!(c.name, "report.pdf");
        assert_eq!(c.existing, Some(1));
        assert!(suggested_rename(&g, 1).is_none(), "keeper already has the name");
        assert!(suggested_rename(&g, 0).is_none(), "another copy has the name; prefer it instead");
    }

    #[test]
    fn canonical_name_offers_rename_when_original_is_gone() {
        let dir = tempfile::TempDir::new().unwrap();
        let a = dir.path().join("report (1).pdf");
        let b = dir.path().join("report (2).pdf");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();
        let g = DuplicateGroup::new("h".into(), vec![info(a.to_str().unwrap()), info(b.to_str().unwrap())]);
        let c = canonical_name(&g).unwrap();
        assert_eq!(c.name, "report.pdf");
        assert_eq!(c.existing, None);
        let (path, new_name) = suggested_rename(&g, 0).unwrap();
        assert_eq!(path, a);
        assert_eq!(new_name, "report.pdf");

        // Occupied target: no suggestion.
        std::fs::write(dir.path().join("report.pdf"), b"other").unwrap();
        assert!(suggested_rename(&g, 0).is_none());
    }

    #[test]
    fn no_markers_means_no_opinion() {
        let g = DuplicateGroup::new("h".into(), vec![info("/a/one.txt"), info("/b/two.txt")]);
        assert!(canonical_name(&g).is_none());
    }
}
