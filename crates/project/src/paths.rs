//! Paths of media relative to a project's folder.

use std::path::{Component, Path, PathBuf};

/// `path` relative to `folder` (`../clips/a.mp4`); `None` if one is relative and not the
/// other, or if they are on different drives (Windows).
pub(crate) fn relative(path: &Path, folder: &Path) -> Option<PathBuf> {
    if path.is_absolute() != folder.is_absolute() {
        return None;
    }
    let path: Vec<Component> = path.components().collect();
    let folder: Vec<Component> = folder.components().collect();
    // Same root (drive and root on Windows) or nothing in common.
    if path.first() != folder.first() {
        return None;
    }
    let common = path.iter().zip(&folder).take_while(|(a, b)| a == b).count();
    let mut relative = PathBuf::new();
    for _ in folder.iter().skip(common) {
        relative.push("..");
    }
    for part in path.iter().skip(common) {
        relative.push(part.as_os_str());
    }
    (!relative.as_os_str().is_empty()).then_some(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn relative_paths() {
        let r = |p: &str, f: &str| relative(Path::new(p), Path::new(f));
        assert_eq!(r("/show/clips/a.mp4", "/show"), Some("clips/a.mp4".into()));
        assert_eq!(
            r("/media/a.mp4", "/show/2026"),
            Some("../../media/a.mp4".into())
        );
        assert_eq!(r("/show/a.mp4", "/show"), Some("a.mp4".into()));
        assert_eq!(r("a.mp4", "/show"), None);
        assert_eq!(r("/show", "/show"), None);
    }

    #[cfg(windows)]
    #[test]
    fn relative_paths() {
        let r = |p: &str, f: &str| relative(Path::new(p), Path::new(f));
        assert_eq!(
            r(r"C:\show\clips\a.mp4", r"C:\show"),
            Some(r"clips\a.mp4".into())
        );
        assert_eq!(r(r"D:\media\a.mp4", r"C:\show"), None);
    }
}
