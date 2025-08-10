use std::io;
use std::path::PathBuf;
use thiserror::Error;

/// Specific archive format validation errors.
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum InvalidArchive {
    /// Archive header has invalid checksum.
    #[error("archive header checksum mismatch")]
    InvalidChecksum,

    /// Archive has duplicate entries describing the same member.
    #[error("two {entry_type} entries describing the same member")]
    DuplicateEntries { entry_type: String },

    /// Archive reader is not at position 0.
    #[error("cannot call {method} unless archive is at position 0")]
    ArchiveNotAtStart { method: String },

    /// Sparse file format errors.
    #[error("sparse entry type listed but not GNU header")]
    SparseNotGnu,

    #[error("more bytes listed in sparse file than u64 can hold")]
    SparseOverflow,

    #[error("previous block in sparse file was not aligned to 512-byte boundary")]
    SparseAlignment,

    #[error("out of order or overlapping sparse blocks")]
    SparseOrdering,

    #[error("sparse file consumed more data than the header listed")]
    SparseDataMismatch,

    #[error("mismatch in sparse file chunks and size in header")]
    SparseSizeMismatch,

    #[error("mismatch in sparse file chunks and entry size in header")]
    SparseEntrySizeMismatch,

    /// Path-related errors.

    #[error("path cannot be split to be inserted into archive: {}", path.display())]
    PathNotSplittable { path: PathBuf },

    #[error("path {} was not valid Unicode", path.display())]
    PathNotUnicode { path: PathBuf },

    #[error("only Unicode paths are supported on Windows: {path}")]
    WindowsPathNotUnicode { path: String },

    #[error("paths in archives must be relative")]
    PathNotRelative,

    #[error("paths in archives must not have `..`")]
    PathHasParentDir,

    #[error("path component in archive cannot contain `/`")]
    PathComponentHasSlash,

    #[error("paths in archives must have at least one component")]
    PathEmpty,

    #[error("trying to unpack outside of destination path: {}", destination.display())]
    PathOutsideDestination { destination: PathBuf },

    /// Header and field validation errors.
    #[error("provided value is too long")]
    ValueTooLong,

    #[error("provided value contains a nul byte")]
    ValueContainsNul,

    #[error("hard link entry is missing link name")]
    HardLinkNameMissing,

    #[error("numeric field did not have utf-8 text: {text}")]
    NumericFieldNotUtf8 { text: String },

    #[error("size overflow")]
    SizeOverflow,

    /// Link-related errors.
    #[error("hard link listed but no link name found")]
    HardLinkNoName,

    #[error("symlink destination for {} is empty", path.display())]
    SymlinkDestinationEmpty { path: PathBuf },

    #[error("symlink destination for {} is outside of the target directory", path.display())]
    SymlinkDestinationOutside { path: PathBuf },

    /// PAX extension errors.
    #[error("malformed pax extension")]
    MalformedPaxExtension,

    /// Socket archiving error.
    #[error("{}: socket can not be archived", path.display())]
    SocketNotArchivable { path: PathBuf },
}

/// Error that occurred while reading or writing tar file.
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum TarError {
    /// Failed to create a file or directory.
    #[error("failed to create `{}`", path.display())]
    CreateFailed {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// Failed to iterate over archive entries.
    #[error("failed to iterate over archive")]
    IterationFailed(#[source] io::Error),

    /// Failed to unpack an entry.
    #[error("failed to unpack `{}`", path.display())]
    UnpackFailed {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// Failed to set file metadata (permissions, timestamps, etc.).
    #[error("failed to set {operation} for `{}`", path.display())]
    MetadataFailed {
        path: PathBuf,
        operation: String,
        #[source]
        source: io::Error,
    },

    /// Invalid path in entry header.
    #[error("invalid path in entry header: {path}")]
    InvalidPath { path: String },

    /// Archive format is invalid or unsupported.
    #[error(transparent)]
    InvalidArchive(#[from] InvalidArchive),

    /// File has unknown or unsupported type.
    #[error("{} has unknown file type", path.display())]
    UnknownFileType { path: PathBuf },

    /// Archive type doesn't support the requested operation.
    #[error("not a {archive_type} archive, cannot {operation}")]
    UnsupportedOperation {
        archive_type: String,
        operation: String,
    },

    /// Numeric field in archive header is malformed.
    #[error("numeric field was not a number: {field}")]
    InvalidNumericField { field: String },

    /// Failed to parse header field with context.
    #[error("{message} when getting {field} for {path}")]
    HeaderFieldParseError {
        message: String,
        field: String,
        path: String,
    },

    /// Failed to read entire block from archive.
    #[error("failed to read entire block")]
    IncompleteBlockRead,

    /// Unexpected EOF while skipping data in archive.
    #[error("unexpected EOF during skip")]
    UnexpectedEofDuringSkip,

    /// Failed to read extension header.
    #[error("failed to read extension")]
    ExtensionReadFailed,

    /// Failed to write entire file during extraction.
    #[error("failed to write entire file")]
    IncompleteFileWrite,

    /// Generic I/O error.
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<TarError> for io::Error {
    fn from(t: TarError) -> io::Error {
        match t {
            TarError::Io(err) => err,
            TarError::CreateFailed { source, .. } => source,
            TarError::UnpackFailed { source, .. } => source,
            TarError::MetadataFailed { source, .. } => source,
            _ => io::Error::other(t),
        }
    }
}
