use crate::other;
use std::{
    fmt, io,
    path::{Component, Path, PathBuf},
};

/// A validated UTF-8 archive member name.
///
/// Archive member names are relative, contain no parent-directory components
/// or Unicode control characters, and are normalized by removing interior `.`
/// components. A sole `.` name and a trailing `/` are preserved.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Name(ValidatedUtf8);

/// A validated UTF-8 symbolic-link target stored in an archive.
///
/// Link targets may be absolute or contain `.` and `..` components, because
/// these values describe symbolic-link contents rather than archive members.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LinkTarget(ValidatedUtf8);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ValidatedUtf8(String);

impl Name {
    /// Returns this archive member name as UTF-8 text.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Returns this archive member name as a path.
    pub fn as_path(&self) -> &Path {
        self.0.as_path()
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl LinkTarget {
    /// Returns this symbolic-link target as UTF-8 text.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Returns this symbolic-link target as a path.
    pub fn as_path(&self) -> &Path {
        self.0.as_path()
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl TryFrom<&Path> for Name {
    type Error = io::Error;

    fn try_from(path: &Path) -> io::Result<Self> {
        let value = ValidatedUtf8::from_path(path, "archive names")?;
        Ok(Self(ValidatedUtf8(canonicalize_name(value.as_path())?)))
    }
}

impl TryFrom<&Path> for LinkTarget {
    type Error = io::Error;

    fn try_from(path: &Path) -> io::Result<Self> {
        let value = ValidatedUtf8::from_path(path, "symbolic-link targets")?;
        Ok(Self(ValidatedUtf8(canonicalize_link_target(
            value.as_str(),
        )?)))
    }
}

macro_rules! impl_owned_value_behavior {
    ($value:ty) => {
        impl TryFrom<&str> for $value {
            type Error = io::Error;

            fn try_from(value: &str) -> io::Result<Self> {
                Self::try_from(Path::new(value))
            }
        }

        impl TryFrom<String> for $value {
            type Error = io::Error;

            fn try_from(value: String) -> io::Result<Self> {
                Self::try_from(value.as_str())
            }
        }

        impl TryFrom<&String> for $value {
            type Error = io::Error;

            fn try_from(value: &String) -> io::Result<Self> {
                Self::try_from(value.as_str())
            }
        }

        impl TryFrom<PathBuf> for $value {
            type Error = io::Error;

            fn try_from(value: PathBuf) -> io::Result<Self> {
                Self::try_from(value.as_path())
            }
        }

        impl TryFrom<&PathBuf> for $value {
            type Error = io::Error;

            fn try_from(value: &PathBuf) -> io::Result<Self> {
                Self::try_from(value.as_path())
            }
        }

        impl From<&$value> for $value {
            fn from(value: &$value) -> Self {
                value.clone()
            }
        }

        impl AsRef<str> for $value {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl AsRef<Path> for $value {
            fn as_ref(&self) -> &Path {
                self.as_path()
            }
        }

        impl fmt::Display for $value {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.as_str().fmt(formatter)
            }
        }
    };
}

impl_owned_value_behavior!(Name);
impl_owned_value_behavior!(LinkTarget);

pub(crate) fn into_name<N>(name: N) -> io::Result<Name>
where
    N: TryInto<Name>,
    N::Error: fmt::Display,
{
    name.try_into().map_err(|err| other(&err.to_string()))
}

pub(crate) fn into_link_target<T>(target: T) -> io::Result<LinkTarget>
where
    T: TryInto<LinkTarget>,
    T::Error: fmt::Display,
{
    target.try_into().map_err(|err| other(&err.to_string()))
}

impl ValidatedUtf8 {
    fn from_path(path: &Path, kind: &str) -> io::Result<Self> {
        let value = path
            .to_str()
            .ok_or_else(|| other(&format!("{kind} must be valid UTF-8")))?;
        if value.chars().any(char::is_control) {
            return Err(other(&format!(
                "{kind} must not contain Unicode control characters"
            )));
        }
        Ok(Self(value.to_owned()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn as_path(&self) -> &Path {
        Path::new(self.as_str())
    }

    fn as_bytes(&self) -> &[u8] {
        self.as_str().as_bytes()
    }
}

fn canonicalize_name(path: &Path) -> io::Result<String> {
    let count = path.components().count();
    let mut result = String::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                return Err(other("paths in archives must be relative"));
            }
            Component::ParentDir => {
                return Err(other("paths in archives must not have `..`"));
            }
            Component::CurDir if count != 1 => continue,
            Component::CurDir => push_component(&mut result, "."),
            Component::Normal(value) => {
                push_component(
                    &mut result,
                    value.to_str().expect("already validated UTF-8"),
                );
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

fn canonicalize_link_target(value: &str) -> io::Result<String> {
    if value.is_empty() {
        return Err(other(
            "symbolic-link targets must have at least one component",
        ));
    }

    #[cfg(any(windows, target_arch = "wasm32"))]
    {
        Ok(value.replace('\\', "/"))
    }

    #[cfg(unix)]
    {
        Ok(value.to_owned())
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
    use std::path::Path;

    #[test]
    fn name_validation_and_normalization() {
        assert_eq!(Name::try_from("filer").unwrap().as_str(), "filer");
        assert_eq!(
            Name::try_from("dossier/\u{e9}").unwrap().as_str(),
            "dossier/\u{e9}"
        );
        assert_eq!(Name::try_from("./a/./b/").unwrap().as_str(), "a/b/");
        assert_eq!(Name::try_from(".").unwrap().as_str(), ".");
        assert!(Name::try_from("").is_err());
        assert!(Name::try_from("/absolute").is_err());
        assert!(Name::try_from("../outside").is_err());
        assert!(Name::try_from("line\nbreak").is_err());
        assert!(Name::try_from("control-\u{85}").is_err());
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
        assert!(LinkTarget::try_from("").is_err());
        assert!(LinkTarget::try_from("line\nbreak").is_err());
        assert!(LinkTarget::try_from("control-\u{85}").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unix_names_require_utf8_and_preserve_backslashes() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        assert!(Name::try_from(Path::new(OsStr::from_bytes(b"bad-\xff"))).is_err());
        assert!(LinkTarget::try_from(Path::new(OsStr::from_bytes(b"bad-\xff"))).is_err());
        assert_eq!(Name::try_from(r"foo\bar").unwrap().as_str(), r"foo\bar");
    }

    #[cfg(windows)]
    #[test]
    fn windows_names_normalize_separators() {
        assert_eq!(Name::try_from(r"foo\bar").unwrap().as_str(), "foo/bar");
        assert_eq!(LinkTarget::try_from(r"..\bar").unwrap().as_str(), "../bar");
    }
}
