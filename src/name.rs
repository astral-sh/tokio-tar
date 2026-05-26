use crate::other;
use std::{
    fmt, io,
    path::{Component, Path, PathBuf},
};

/// A validated UTF-8 archive member name.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Name(String);

/// A validated UTF-8 symbolic-link target stored in an archive.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LinkTarget(String);

pub(crate) fn archive_name(path: &Path) -> io::Result<String> {
    validate_utf8(path, "archive names")?;
    let mut result = String::new();
    let sole_component = path.components().count() == 1;
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                return Err(other("paths in archives must be relative"));
            }
            Component::ParentDir => {
                return Err(other("paths in archives must not have `..`"));
            }
            Component::CurDir if !sole_component => continue,
            Component::CurDir => push_component(&mut result, "."),
            Component::Normal(value) => {
                push_component(&mut result, value.to_str().expect("validated UTF-8"));
            }
        }
    }
    if result.is_empty() {
        return Err(other("paths in archives must have at least one component"));
    }
    if ends_with_slash(path) && !result.ends_with('/') {
        result.push('/');
    }
    Ok(result)
}

pub(crate) fn link_target(path: &Path) -> io::Result<String> {
    let value = validate_utf8(path, "symbolic-link targets")?;
    if value.is_empty() {
        return Err(other(
            "symbolic-link targets must have at least one component",
        ));
    }
    #[cfg(any(windows, target_arch = "wasm32"))]
    return Ok(value.replace('\\', "/"));
    #[cfg(unix)]
    Ok(value.to_owned())
}

macro_rules! validated_value {
    ($ty:ident, $validate:ident) => {
        impl $ty {
            /// Returns this validated value as UTF-8 text.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Returns this validated value as a path.
            pub fn as_path(&self) -> &Path {
                Path::new(self.as_str())
            }
        }

        impl TryFrom<&Path> for $ty {
            type Error = io::Error;
            fn try_from(path: &Path) -> io::Result<Self> {
                $validate(path).map(Self)
            }
        }

        impl TryFrom<&str> for $ty {
            type Error = io::Error;
            fn try_from(value: &str) -> io::Result<Self> {
                Self::try_from(Path::new(value))
            }
        }

        impl TryFrom<String> for $ty {
            type Error = io::Error;
            fn try_from(value: String) -> io::Result<Self> {
                Self::try_from(value.as_str())
            }
        }

        impl TryFrom<&String> for $ty {
            type Error = io::Error;
            fn try_from(value: &String) -> io::Result<Self> {
                Self::try_from(value.as_str())
            }
        }

        impl TryFrom<PathBuf> for $ty {
            type Error = io::Error;
            fn try_from(value: PathBuf) -> io::Result<Self> {
                Self::try_from(value.as_path())
            }
        }

        impl TryFrom<&PathBuf> for $ty {
            type Error = io::Error;
            fn try_from(value: &PathBuf) -> io::Result<Self> {
                Self::try_from(value.as_path())
            }
        }

        impl From<&$ty> for $ty {
            fn from(value: &$ty) -> Self {
                value.clone()
            }
        }

        impl AsRef<str> for $ty {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl AsRef<Path> for $ty {
            fn as_ref(&self) -> &Path {
                self.as_path()
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.as_str().fmt(formatter)
            }
        }
    };
}

validated_value!(Name, archive_name);
validated_value!(LinkTarget, link_target);

fn validate_utf8<'a>(path: &'a Path, kind: &str) -> io::Result<&'a str> {
    let value = path
        .to_str()
        .ok_or_else(|| other(&format!("{kind} must be valid UTF-8")))?;
    if value.chars().any(char::is_control) {
        Err(other(&format!(
            "{kind} must not contain Unicode control characters"
        )))
    } else {
        Ok(value)
    }
}

fn push_component(result: &mut String, component: &str) {
    if !result.is_empty() {
        result.push('/');
    }
    result.push_str(component);
}

#[cfg(any(windows, target_arch = "wasm32"))]
fn ends_with_slash(path: &Path) -> bool {
    path.to_string_lossy()
        .as_bytes()
        .last()
        .is_some_and(|byte| *byte == b'/' || *byte == b'\\')
}

#[cfg(unix)]
fn ends_with_slash(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().ends_with(b"/")
}

#[cfg(test)]
mod tests {
    use super::{LinkTarget, Name};

    #[test]
    fn name_validation_and_normalization() {
        assert_eq!(Name::try_from("filer").unwrap().as_str(), "filer");
        assert_eq!(
            Name::try_from("dossier/\u{e9}").unwrap().as_str(),
            "dossier/\u{e9}"
        );
        assert_eq!(Name::try_from("./a/./b/").unwrap().as_str(), "a/b/");
        assert_eq!(Name::try_from(".").unwrap().as_str(), ".");
        for invalid in [
            "",
            "/absolute",
            "../outside",
            "line\nbreak",
            "control-\u{85}",
        ] {
            assert!(Name::try_from(invalid).is_err());
        }
    }

    #[test]
    fn link_target_validation_and_meaning() {
        assert_eq!(
            LinkTarget::try_from("../\u{e9}/file").unwrap().as_str(),
            "../\u{e9}/file"
        );
        assert_eq!(
            LinkTarget::try_from("/root/./file").unwrap().as_str(),
            "/root/./file"
        );
        for invalid in ["", "line\nbreak", "control-\u{85}"] {
            assert!(LinkTarget::try_from(invalid).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_names_require_utf8_and_preserve_backslashes() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};
        let invalid = Path::new(OsStr::from_bytes(b"bad-\xff"));
        assert!(Name::try_from(invalid).is_err());
        assert!(LinkTarget::try_from(invalid).is_err());
        assert_eq!(Name::try_from(r"foo\bar").unwrap().as_str(), r"foo\bar");
    }

    #[cfg(windows)]
    #[test]
    fn windows_names_normalize_separators() {
        assert_eq!(Name::try_from(r"foo\bar").unwrap().as_str(), "foo/bar");
        assert_eq!(LinkTarget::try_from(r"..\bar").unwrap().as_str(), "../bar");
    }
}
