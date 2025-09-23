use std::path::{Component, Path, PathBuf};

/// Normalize a path, like Python's `os.path.normpath`.
///
/// Adapted from <https://github.com/rust-lang/cargo/blob/fede83ccf973457de319ba6fa0e36ead454d2e20/src/cargo/util/paths.rs#L61>.
pub(crate) fn normalize(path: &Path) -> Option<PathBuf> {
    let mut components = path.components().peekable();
    let mut ret = if let Some(c @ Component::Prefix(..)) = components.peek() {
        let buf = PathBuf::from(c.as_os_str());
        components.next();
        buf
    } else {
        PathBuf::new()
    };
    let mut has_root = false;

    for component in components {
        match component {
            Component::Prefix(..) => unreachable!(),
            Component::RootDir => {
                ret.push(component.as_os_str());
                has_root = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                // Preserve leading `..` components.
                if ret
                    .components()
                    .next_back()
                    .is_some_and(|component| component == Component::ParentDir)
                {
                    ret.push(component.as_os_str());
                } else if ret.pop() {
                    // We successfully removed a component.
                } else if has_root {
                    // An absolute path tried to go above the root.
                    return None;
                } else {
                    // If we don't have a root, we can just push the `..` component.
                    ret.push(component.as_os_str());
                }
            }
            Component::Normal(c) => {
                ret.push(c);
            }
        }
    }

    Some(ret)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::path::{Path, PathBuf};

    #[test]
    #[cfg(unix)]
    fn test_normalize() {
        // Basic relative path.
        assert_eq!(
            crate::fs::normalize(Path::new("a/b/c")),
            Some(PathBuf::from("a/b/c"))
        );

        // Path with `..`, should remove `b`.
        assert_eq!(
            crate::fs::normalize(Path::new("a/b/../c")),
            Some(PathBuf::from("a/c"))
        );

        // Path with `.` should be ignored.
        assert_eq!(
            crate::fs::normalize(Path::new("./a/b")),
            Some(PathBuf::from("a/b"))
        );

        // Path with no relative components should be unchanged.
        assert_eq!(
            crate::fs::normalize(Path::new("outside")),
            Some(PathBuf::from("outside"))
        );

        // Excessive `..` should be ignored.
        assert_eq!(
            crate::fs::normalize(Path::new("../../../../")),
            Some(PathBuf::from("../../../../"),)
        );

        // Multiple `..` should stack.
        assert_eq!(
            crate::fs::normalize(Path::new("a/b/../../c")),
            Some(PathBuf::from("c"))
        );

        // Rooted absolute path, `..` should not go above root.
        assert_eq!(crate::fs::normalize(Path::new("/a/../..")), None);

        // Root with dot and parent.
        assert_eq!(
            crate::fs::normalize(Path::new("/./a/../b")),
            Some(PathBuf::from("/b"))
        );

        // Trailing slash should be ignored.
        assert_eq!(
            crate::fs::normalize(Path::new("a/b/c/")),
            Some(PathBuf::from("a/b/c"))
        );

        // Trailing `/.` should be dropped.
        assert_eq!(
            crate::fs::normalize(Path::new("a/b/.")),
            Some(PathBuf::from("a/b"))
        );

        // Trailing `/..` should pop last component.
        assert_eq!(
            crate::fs::normalize(Path::new("a/b/..")),
            Some(PathBuf::from("a"))
        );

        // Leading `..` in a relative path should be preserved.
        assert_eq!(
            crate::fs::normalize(Path::new("../x/y")),
            Some(PathBuf::from("../x/y"))
        );

        // Mix of preserved leading `..` and collapsed internals.
        assert_eq!(
            crate::fs::normalize(Path::new("../../a/b/../c")),
            Some(PathBuf::from("../../a/c"))
        );

        // Windows drive absolute: C:\a\..\b
        #[cfg(windows)]
        assert_eq!(
            crate::fs::normalize(Path::new(r"C:\a\..\b")),
            Some(PathBuf::from(r"C:\b"))
        );

        // Windows drive-relative (no backslash): C:..\a
        // should preserve the `..`
        #[cfg(windows)]
        assert_eq!(
            crate::fs::normalize(Path::new(r"C:..\a")),
            Some(PathBuf::from(r"C:..\a"))
        );

        // Root-only should normalize to root.
        assert_eq!(
            crate::fs::normalize(Path::new("/")),
            Some(PathBuf::from("/"))
        );

        // Just `..` should normalize to `..`
        assert_eq!(
            crate::fs::normalize(Path::new("..")),
            Some(PathBuf::from(".."))
        );
    }
}
