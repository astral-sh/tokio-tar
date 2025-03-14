use std::path::{Component, Path, PathBuf};

/// Normalize an absolute path.
pub(crate) fn normalize_absolute(p: &Path) -> Option<PathBuf> {
    debug_assert!(p.is_absolute(), "Target must be an absolute path");

    let mut resolved = PathBuf::new();
    for component in p.components() {
        match component {
            Component::Prefix(prefix) => {
                // Windows-specific: preserve drive letter and prefix.
                resolved.push(prefix.as_os_str());
            }
            Component::RootDir => {
                // Append root directory after prefix.
                resolved.push(component.as_os_str());
            }
            Component::CurDir => {
                // Ignore `.`
            }
            Component::ParentDir => {
                if !resolved.pop() {
                    return None;
                }
            }
            Component::Normal(segment) => {
                resolved.push(segment);
            }
        }
    }
    Some(resolved)
}

/// Normalize a path relative to a destination directory.
pub(crate) fn normalize_relative(dst: &Path, p: &Path) -> Option<PathBuf> {
    debug_assert!(!p.is_absolute(), "Target must be a relative path");
    debug_assert!(dst.is_absolute(), "Destination must be an absolute path");

    let mut resolved = dst.to_path_buf();
    for component in p.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => {
                // E.g., `/usr` on Windows.
                return None;
            }
            Component::CurDir => {
                // Ignore `.`
            }
            Component::ParentDir => {
                if !resolved.pop() {
                    return None;
                }
            }
            Component::Normal(segment) => {
                resolved.push(segment);
            }
        }
    }
    Some(resolved)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    #[test]
    #[cfg(unix)]
    fn test_normalize_absolute() {
        // Basic absolute path.
        assert_eq!(
            crate::fs::normalize_absolute(Path::new("/a/b/c")),
            Some(PathBuf::from("/a/b/c"))
        );

        // Path with `..` (should remove `b`).
        assert_eq!(
            crate::fs::normalize_absolute(Path::new("/a/b/../c")),
            Some(PathBuf::from("/a/c"))
        );

        // Path with `.`, should be ignored.
        assert_eq!(
            crate::fs::normalize_absolute(Path::new("/a/./b")),
            Some(PathBuf::from("/a/b"))
        );

        // Excessive `..` that escapes root.
        assert_eq!(crate::fs::normalize_absolute(Path::new("/../b")), None);
    }

    #[test]
    #[cfg(windows)]
    fn test_normalize_absolute() {
        // Basic absolute path.
        assert_eq!(
            crate::fs::normalize_absolute(Path::new(r"C:\a\b\c")),
            Some(PathBuf::from(r"C:\a\b\c"))
        );

        // Path with `..` (should remove `b`).
        assert_eq!(
            crate::fs::normalize_absolute(Path::new(r"C:\a\b\..\c")),
            Some(PathBuf::from(r"C:\a\c"))
        );

        // Path with `.`, should be ignored.
        assert_eq!(
            crate::fs::normalize_absolute(Path::new(r"C:\a\.\b")),
            Some(PathBuf::from(r"C:\a\b"))
        );

        // Excessive `..` that escapes root.
        assert_eq!(crate::fs::normalize_absolute(Path::new(r"C:\..\b")), None);
    }

    #[test]
    #[cfg(unix)]
    fn test_normalize_relative() {
        let dst = Path::new("/home/user/dst");

        // Basic relative path.
        assert_eq!(
            crate::fs::normalize_relative(dst, Path::new("a/b/c")),
            Some(PathBuf::from("/home/user/dst/a/b/c"))
        );

        // Path with `..`, should remove `b`.
        assert_eq!(
            crate::fs::normalize_relative(dst, Path::new("a/b/../c")),
            Some(PathBuf::from("/home/user/dst/a/c"))
        );

        // Path with `.` should be ignored.
        assert_eq!(
            crate::fs::normalize_relative(dst, Path::new("./a/b")),
            Some(PathBuf::from("/home/user/dst/a/b"))
        );

        // Path escaping `dst`, should return None.
        assert_eq!(
            crate::fs::normalize_relative(dst, Path::new("../../../../outside")),
            None
        );

        // Excessive `..`, escaping `dst`.
        assert_eq!(
            crate::fs::normalize_relative(dst, Path::new("../../../../")),
            None
        );
    }
}
