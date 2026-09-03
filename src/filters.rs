//! Group filters shared by the TUI and the web UI, so both classify sizes
//! and file types identically.

use crate::duplicates::DuplicateGroup;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SizeBucket {
    #[default]
    All,
    /// under 10 KiB
    Tiny,
    /// 10 KiB to 1 MiB
    Small,
    /// 1 MiB to 100 MiB
    Medium,
    /// 100 MiB and up
    Large,
}

impl SizeBucket {
    pub const ALL: [SizeBucket; 5] = [
        SizeBucket::All,
        SizeBucket::Tiny,
        SizeBucket::Small,
        SizeBucket::Medium,
        SizeBucket::Large,
    ];

    pub fn parse(s: &str) -> Option<SizeBucket> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "all" => Some(SizeBucket::All),
            "tiny" => Some(SizeBucket::Tiny),
            "small" => Some(SizeBucket::Small),
            "medium" => Some(SizeBucket::Medium),
            "large" => Some(SizeBucket::Large),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            SizeBucket::All => "all sizes",
            SizeBucket::Tiny => "tiny (under 10 KiB)",
            SizeBucket::Small => "small (10 KiB to 1 MiB)",
            SizeBucket::Medium => "medium (1 MiB to 100 MiB)",
            SizeBucket::Large => "large (100 MiB and up)",
        }
    }

    pub fn next(&self) -> SizeBucket {
        let i = Self::ALL.iter().position(|b| b == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// The bucket a file size falls into (never `All`).
    pub fn of(size: u64) -> SizeBucket {
        const KB: u64 = 1024;
        const MB: u64 = 1024 * KB;
        if size < 10 * KB {
            SizeBucket::Tiny
        } else if size < MB {
            SizeBucket::Small
        } else if size < 100 * MB {
            SizeBucket::Medium
        } else {
            SizeBucket::Large
        }
    }

    pub fn matches(&self, size: u64) -> bool {
        *self == SizeBucket::All || Self::of(size) == *self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    #[default]
    All,
    Image,
    Video,
    Audio,
    Document,
    Archive,
    Code,
    Other,
}

impl FileKind {
    pub const ALL: [FileKind; 8] = [
        FileKind::All,
        FileKind::Image,
        FileKind::Video,
        FileKind::Audio,
        FileKind::Document,
        FileKind::Archive,
        FileKind::Code,
        FileKind::Other,
    ];

    pub fn parse(s: &str) -> Option<FileKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "all" => Some(FileKind::All),
            "image" | "images" => Some(FileKind::Image),
            "video" => Some(FileKind::Video),
            "audio" => Some(FileKind::Audio),
            "document" | "documents" => Some(FileKind::Document),
            "archive" | "archives" => Some(FileKind::Archive),
            "code" => Some(FileKind::Code),
            "other" => Some(FileKind::Other),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            FileKind::All => "all types",
            FileKind::Image => "images",
            FileKind::Video => "video",
            FileKind::Audio => "audio",
            FileKind::Document => "documents",
            FileKind::Archive => "archives",
            FileKind::Code => "code",
            FileKind::Other => "other",
        }
    }

    pub fn next(&self) -> FileKind {
        let i = Self::ALL.iter().position(|k| k == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// Classify a file by extension (never `All`).
    pub fn of(path: &Path) -> FileKind {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "heic" | "heif" | "tif" | "tiff" | "svg" | "ico"
            | "avif" | "raw" | "cr2" | "nef" | "arw" | "dng" | "psd" => FileKind::Image,
            "mp4" | "mov" | "avi" | "mkv" | "webm" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" | "3gp" | "ts" => {
                FileKind::Video
            }
            "mp3" | "wav" | "flac" | "ogg" | "oga" | "m4a" | "aac" | "aiff" | "aif" | "wma" | "opus" | "mid"
            | "midi" => FileKind::Audio,
            "pdf" | "doc" | "docx" | "txt" | "md" | "rtf" | "odt" | "xls" | "xlsx" | "ods" | "ppt" | "pptx"
            | "odp" | "csv" | "pages" | "numbers" | "key" | "epub" | "mobi" => FileKind::Document,
            "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "dmg" | "iso" | "jar" | "war" => {
                FileKind::Archive
            }
            "rs" | "js" | "mjs" | "cjs" | "jsx" | "tsx" | "py" | "rb" | "go" | "java" | "c" | "h" | "cpp" | "hpp"
            | "cc" | "cs" | "swift" | "kt" | "kts" | "php" | "html" | "htm" | "css" | "scss" | "less" | "json"
            | "yaml" | "yml" | "toml" | "xml" | "sh" | "bash" | "zsh" | "sql" | "lua" | "pl" | "r" | "m" | "vue"
            | "svelte" | "ipynb" | "lock" => FileKind::Code,
            _ => FileKind::Other,
        }
    }

    pub fn matches(&self, path: &Path) -> bool {
        *self == FileKind::All || Self::of(path) == *self
    }
}

/// The filter both UIs apply to the list of groups.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupFilter {
    /// Case-insensitive substring of any file path in the group.
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub size: SizeBucket,
    #[serde(default, rename = "type")]
    pub kind: FileKind,
}

impl GroupFilter {
    pub fn is_active(&self) -> bool {
        !self.path.trim().is_empty() || self.size != SizeBucket::All || self.kind != FileKind::All
    }

    /// A group matches when its size is in the bucket and at least one of
    /// its files matches both the path substring and the type.
    pub fn matches(&self, group: &DuplicateGroup) -> bool {
        if !self.size.matches(group.file_size()) {
            return false;
        }
        let needle = self.path.trim().to_lowercase();
        group.files.iter().any(|f| {
            let path_ok = needle.is_empty() || f.path.to_string_lossy().to_lowercase().contains(&needle);
            path_ok && self.kind.matches(&f.path)
        })
    }

    /// Indices of the groups that pass the filter, in the original order.
    pub fn apply(&self, groups: &[DuplicateGroup]) -> Vec<usize> {
        if !self.is_active() {
            return (0..groups.len()).collect();
        }
        groups
            .iter()
            .enumerate()
            .filter(|(_, g)| self.matches(g))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.path.trim().is_empty() {
            parts.push(format!("path contains \"{}\"", self.path.trim()));
        }
        if self.size != SizeBucket::All {
            parts.push(self.size.label().to_string());
        }
        if self.kind != FileKind::All {
            parts.push(self.kind.label().to_string());
        }
        if parts.is_empty() {
            "no filter".to_string()
        } else {
            parts.join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::FileInfo;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn info(path: &str, size: u64) -> FileInfo {
        FileInfo {
            path: PathBuf::from(path),
            size,
            quick_hash: None,
            hash: Some("h".into()),
            modified: SystemTime::UNIX_EPOCH,
            depth: 2,
        }
    }

    #[test]
    fn size_buckets() {
        assert_eq!(SizeBucket::of(100), SizeBucket::Tiny);
        assert_eq!(SizeBucket::of(10 * 1024), SizeBucket::Small);
        assert_eq!(SizeBucket::of(5 * 1024 * 1024), SizeBucket::Medium);
        assert_eq!(SizeBucket::of(500 * 1024 * 1024), SizeBucket::Large);
        assert_eq!(SizeBucket::parse("MEDIUM"), Some(SizeBucket::Medium));
        assert_eq!(SizeBucket::parse("huge"), None);
        assert_eq!(SizeBucket::Large.next(), SizeBucket::All);
    }

    #[test]
    fn file_kinds() {
        assert_eq!(FileKind::of(Path::new("/x/photo.JPG")), FileKind::Image);
        assert_eq!(FileKind::of(Path::new("/x/clip.mov")), FileKind::Video);
        assert_eq!(FileKind::of(Path::new("/x/song.flac")), FileKind::Audio);
        assert_eq!(FileKind::of(Path::new("/x/notes.txt")), FileKind::Document);
        assert_eq!(FileKind::of(Path::new("/x/site.tar.gz")), FileKind::Archive);
        assert_eq!(FileKind::of(Path::new("/x/main.rs")), FileKind::Code);
        assert_eq!(FileKind::of(Path::new("/x/blob")), FileKind::Other);
        assert_eq!(FileKind::parse("Images"), Some(FileKind::Image));
    }

    #[test]
    fn group_filter_matches_any_file() {
        let g = DuplicateGroup::new("h".into(), vec![info("/a/photo.jpg", 2048), info("/b/photo.jpg", 2048)]);
        assert!(GroupFilter::default().matches(&g));
        assert!(GroupFilter { path: "/A/".into(), ..Default::default() }.matches(&g));
        assert!(!GroupFilter { path: "/c/".into(), ..Default::default() }.matches(&g));
        assert!(GroupFilter { size: SizeBucket::Tiny, ..Default::default() }.matches(&g));
        assert!(!GroupFilter { size: SizeBucket::Large, ..Default::default() }.matches(&g));
        assert!(GroupFilter { kind: FileKind::Image, ..Default::default() }.matches(&g));
        assert!(!GroupFilter { kind: FileKind::Audio, ..Default::default() }.matches(&g));
        assert_eq!(GroupFilter::default().apply(&[g.clone(), g.clone()]), vec![0, 1]);
    }
}
