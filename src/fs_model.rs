use chrono::{DateTime, Local, Utc};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_parent: bool,
    pub size: u64,
    pub modified: Option<DateTime<Utc>>,
    pub permissions: String,
}

impl FileEntry {
    pub fn parent(parent_path: String) -> Self {
        Self {
            name: "..".to_string(),
            path: parent_path,
            is_dir: true,
            is_symlink: false,
            is_parent: true,
            size: 0,
            modified: None,
            permissions: String::new(),
        }
    }

    pub fn formatted_size(&self) -> String {
        if self.is_dir {
            "<DIR>".to_string()
        } else {
            format_bytes(self.size)
        }
    }

    pub fn formatted_date(&self) -> String {
        match self.modified {
            Some(dt) => dt
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            None => String::new(),
        }
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

pub fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        b.is_parent
            .cmp(&a.is_parent)
            .then_with(|| b.is_dir.cmp(&a.is_dir))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

pub fn list_local_dir(dir: &Path, show_hidden: bool) -> anyhow::Result<Vec<FileEntry>> {
    let mut entries = Vec::new();
    if let Some(parent) = dir.parent() {
        entries.push(FileEntry::parent(parent.to_string_lossy().to_string()));
    }

    for item in std::fs::read_dir(dir)? {
        let Ok(item) = item else { continue };
        let name = item.file_name().to_string_lossy().to_string();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let path = item.path();
        let symlink_meta = std::fs::symlink_metadata(&path).ok();
        let meta = std::fs::metadata(&path).ok();
        let is_symlink = symlink_meta
            .as_ref()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .map(DateTime::<Utc>::from);
        let permissions = meta
            .as_ref()
            .map(format_local_permissions)
            .unwrap_or_default();

        entries.push(FileEntry {
            name,
            path: path.to_string_lossy().to_string(),
            is_dir,
            is_symlink,
            is_parent: false,
            size,
            modified,
            permissions,
        });
    }

    sort_entries(&mut entries);
    Ok(entries)
}

#[cfg(unix)]
fn format_local_permissions(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    format_unix_mode(meta.permissions().mode(), meta.is_dir())
}

#[cfg(not(unix))]
fn format_local_permissions(meta: &std::fs::Metadata) -> String {
    if meta.permissions().readonly() {
        "r--".to_string()
    } else {
        "rw-".to_string()
    }
}

pub fn format_unix_mode(mode: u32, is_dir: bool) -> String {
    let prefix = if is_dir { 'd' } else { '-' };
    let bits = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    let mut s = String::with_capacity(10);
    s.push(prefix);
    for (mask, ch) in bits {
        s.push(if mode & mask != 0 { ch } else { '-' });
    }
    s
}

pub fn remote_join(base: &str, name: &str) -> String {
    if base.is_empty() || base == "/" {
        format!("/{}", name.trim_start_matches('/'))
    } else {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            name.trim_start_matches('/')
        )
    }
}

pub fn remote_parent(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "/" {
        return "/".to_string();
    }
    match trimmed.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(idx) => trimmed[..idx].to_string(),
    }
}

pub fn default_local_dir() -> PathBuf {
    std::env::current_dir()
        .or_else(|_| dirs::home_dir().ok_or_else(|| std::io::Error::other("no home")))
        .unwrap_or_else(|_| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remote_paths() {
        assert_eq!(remote_join("/", "foo.txt"), "/foo.txt");
        assert_eq!(remote_join("/var/www", "index.html"), "/var/www/index.html");
        assert_eq!(remote_join("/var/www/", "index.html"), "/var/www/index.html");
        assert_eq!(remote_parent("/var/www/html"), "/var/www");
        assert_eq!(remote_parent("/var"), "/");
        assert_eq!(remote_parent("/"), "/");
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
    }
}
