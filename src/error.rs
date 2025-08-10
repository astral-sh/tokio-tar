use std::borrow::Cow;
use std::error;
use std::fmt;
use std::io;

/// Error that occurred while reading or writing tar file.
///
/// The source may be an IO error from the operating system or a custom error with additional
/// context.
#[derive(Debug)]
pub struct TarError {
    desc: Cow<'static, str>,
    io: io::Error,
}

impl TarError {
    pub(crate) fn new(desc: impl Into<Cow<'static, str>>, err: io::Error) -> TarError {
        TarError {
            desc: desc.into(),
            io: err,
        }
    }
}

impl error::Error for TarError {
    fn description(&self) -> &str {
        &self.desc
    }

    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        Some(&self.io)
    }
}

impl fmt::Display for TarError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.desc.fmt(f)
    }
}

impl From<TarError> for io::Error {
    fn from(t: TarError) -> io::Error {
        io::Error::new(t.io.kind(), t)
    }
}
