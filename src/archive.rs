use portable_atomic::{AtomicU64, Ordering};
use rustc_hash::FxHashSet;
use std::{
    cmp,
    collections::VecDeque,
    path::Path,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    fs,
    io::{self, AsyncRead as Read, AsyncReadExt},
    sync::Mutex,
};
use tokio_stream::*;

use crate::{
    entry::{EntryFields, EntryIo, PaxOwnerName},
    error::TarError,
    other, Entry, GnuExtSparseHeader, GnuSparseHeader, Header,
};
use crate::{header::BLOCK_SIZE, pax::pax_extensions};

/// A top-level representation of an archive file.
///
/// This archive can have an entry added to it and it can be iterated over.
#[derive(Debug)]
pub struct Archive<R: Read + Unpin> {
    inner: Arc<ArchiveInner<R>>,
}

impl<R: Read + Unpin> Clone for Archive<R> {
    fn clone(&self) -> Self {
        Archive {
            inner: self.inner.clone(),
        }
    }
}

#[derive(Debug)]
pub struct ArchiveInner<R> {
    pos: AtomicU64,
    physical_entries: AtomicU64,
    extension_bytes: AtomicU64,
    unpack_xattrs: bool,
    preserve_permissions: bool,
    preserve_mtime: bool,
    allow_external_symlinks: bool,
    overwrite: bool,
    ignore_zeros: bool,
    pax_only: bool,
    max_extension_entry_size: Option<u64>,
    max_total_extension_size: Option<u64>,
    max_physical_entries: Option<u64>,
    max_sparse_entries: Option<u64>,
    max_sparse_continuation_blocks: Option<u64>,
    obj: Mutex<R>,
}

impl<R> ArchiveInner<R> {
    fn reserve_entry(&self, size: u64, is_extension: bool) -> io::Result<()> {
        if let Some(limit) = self.max_physical_entries {
            reserve_with_limit(
                &self.physical_entries,
                1,
                limit,
                "archive physical entry limit exceeded",
            )?;
        }

        if !is_extension {
            return Ok(());
        }

        if self
            .max_extension_entry_size
            .is_some_and(|limit| limit == 0 || size > limit)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "archive extension entry size limit exceeded",
            ));
        }

        if let Some(limit) = self.max_total_extension_size {
            if limit == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "archive total extension size limit exceeded",
                ));
            }
            reserve_with_limit(
                &self.extension_bytes,
                size,
                limit,
                "archive total extension size limit exceeded",
            )?;
        }

        Ok(())
    }
}

fn reserve_with_limit(
    counter: &AtomicU64,
    amount: u64,
    limit: u64,
    message: &'static str,
) -> io::Result<()> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current
                .checked_add(amount)
                .filter(|next| *next <= limit)
        })
        .map(|_| ())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, message))
}

/// Configure the archive.
pub struct ArchiveBuilder<R: Read + Unpin> {
    obj: R,
    unpack_xattrs: bool,
    preserve_permissions: bool,
    preserve_mtime: bool,
    allow_external_symlinks: bool,
    overwrite: bool,
    ignore_zeros: bool,
    pax_only: bool,
    max_extension_entry_size: Option<u64>,
    max_total_extension_size: Option<u64>,
    max_physical_entries: Option<u64>,
    max_sparse_entries: Option<u64>,
    max_sparse_continuation_blocks: Option<u64>,
}

impl<R: Read + Unpin> ArchiveBuilder<R> {
    /// Create a new builder.
    pub fn new(obj: R) -> Self {
        ArchiveBuilder {
            unpack_xattrs: false,
            preserve_permissions: false,
            preserve_mtime: true,
            allow_external_symlinks: true,
            overwrite: true,
            ignore_zeros: false,
            pax_only: false,
            max_extension_entry_size: None,
            max_total_extension_size: None,
            max_physical_entries: None,
            max_sparse_entries: None,
            max_sparse_continuation_blocks: None,
            obj,
        }
    }

    /// Indicate whether extended file attributes (xattrs on Unix) are preserved
    /// when unpacking this archive.
    ///
    /// This flag is disabled by default and is currently only implemented on
    /// Unix using xattr support. This may eventually be implemented for
    /// Windows, however, if other archive implementations are found which do
    /// this as well.
    pub fn set_unpack_xattrs(mut self, unpack_xattrs: bool) -> Self {
        self.unpack_xattrs = unpack_xattrs;
        self
    }

    /// Indicate whether the permissions on files and directories are preserved
    /// when unpacking this entry.
    ///
    /// This flag is disabled by default and is currently only implemented on
    /// Unix.
    pub fn set_preserve_permissions(mut self, preserve: bool) -> Self {
        self.preserve_permissions = preserve;
        self
    }

    /// Indicate whether files and symlinks should be overwritten on extraction.
    pub fn set_overwrite(mut self, overwrite: bool) -> Self {
        self.overwrite = overwrite;
        self
    }

    /// Indicate whether access time information is preserved when unpacking
    /// this entry.
    ///
    /// This flag is enabled by default.
    pub fn set_preserve_mtime(mut self, preserve: bool) -> Self {
        self.preserve_mtime = preserve;
        self
    }

    /// Ignore zeroed headers, which would otherwise indicate to the archive that it has no more
    /// entries.
    ///
    /// This can be used in case multiple tar archives have been concatenated together.
    pub fn set_ignore_zeros(mut self, ignore_zeros: bool) -> Self {
        self.ignore_zeros = ignore_zeros;
        self
    }

    /// Indicate whether to only accept strict POSIX pax-compatible input archives.
    ///
    /// This mode accepts UStar members that need no pax extended records and
    /// local pax records that describe the following UStar member. It rejects
    /// global pax extensions, non-pax typeflags, and non-octal header numeric
    /// fields. Extended values must be carried by local pax records instead.
    /// Raw entry iteration is unavailable in this mode because it cannot
    /// validate local pax metadata without consuming the entry payload.
    ///
    /// This flag is disabled by default.
    pub fn set_pax_only(mut self, pax_only: bool) -> Self {
        self.pax_only = pax_only;
        self
    }

    /// Limit the declared payload size of each metadata extension entry.
    ///
    /// This applies to GNU long name and long link entries and to local and
    /// global PAX extension entries. The limit is checked before the extension
    /// payload is buffered. A value of zero rejects every such entry. Extension
    /// entry sizes are unlimited by default.
    pub fn set_max_extension_entry_size(mut self, max: u64) -> Self {
        self.max_extension_entry_size = Some(max);
        self
    }

    /// Limit the total declared payload size of metadata extension entries.
    ///
    /// This cumulative limit covers GNU long name and long link entries and
    /// local and global PAX extension entries. It is shared by all entry streams
    /// created from this archive. A value of zero rejects every such entry.
    /// Total extension entry size is unlimited by default.
    pub fn set_max_total_extension_size(mut self, max: u64) -> Self {
        self.max_total_extension_size = Some(max);
        self
    }

    /// Limit the number of physical entry headers in the archive.
    ///
    /// Extension entries count toward this limit. GNU sparse continuation
    /// blocks are part of one physical entry and do not count separately. The
    /// limit is shared by all entry streams created from this archive. A value
    /// of zero rejects every entry. Physical entry count is unlimited by
    /// default.
    pub fn set_max_physical_entries(mut self, max: u64) -> Self {
        self.max_physical_entries = Some(max);
        self
    }

    /// Limit the number of non-empty map entries in each GNU sparse entry.
    ///
    /// Map entries in the main GNU header and every sparse continuation block
    /// count toward the same per-entry limit. The limit is checked before the
    /// corresponding sparse I/O map grows. A value of zero rejects every GNU
    /// sparse entry with a non-empty map. Sparse map entry count is unlimited
    /// by default. This limit applies to logical entry streams created by
    /// [`Archive::entries`] and to unpacking. It is not applied by
    /// [`Archive::entries_raw`], which does not interpret GNU sparse maps.
    pub fn set_max_sparse_entries(mut self, max: u64) -> Self {
        self.max_sparse_entries = Some(max);
        self
    }

    /// Limit the number of continuation blocks in each GNU sparse entry.
    ///
    /// Every 512-byte sparse continuation block counts, including blocks with
    /// no map entries. The limit is checked before each block is read. A value
    /// of zero rejects every GNU sparse entry that declares a continuation
    /// block. Sparse continuation block count is unlimited by default. This
    /// limit applies to logical entry streams created by [`Archive::entries`]
    /// and to unpacking. It is not applied by [`Archive::entries_raw`], which
    /// returns the stored sparse payload without parsing continuation blocks.
    pub fn set_max_sparse_continuation_blocks(mut self, max: u64) -> Self {
        self.max_sparse_continuation_blocks = Some(max);
        self
    }

    /// Indicate whether to deny symlinks that point outside the destination
    /// directory when unpacking this entry. (Writing to locations outside the
    /// destination directory is _always_ forbidden.)
    ///
    /// This flag is enabled by default.
    pub fn set_allow_external_symlinks(mut self, allow_external_symlinks: bool) -> Self {
        self.allow_external_symlinks = allow_external_symlinks;
        self
    }

    /// Construct the archive, ready to accept inputs.
    pub fn build(self) -> Archive<R> {
        let Self {
            unpack_xattrs,
            preserve_permissions,
            preserve_mtime,
            allow_external_symlinks,
            overwrite,
            ignore_zeros,
            pax_only,
            max_extension_entry_size,
            max_total_extension_size,
            max_physical_entries,
            max_sparse_entries,
            max_sparse_continuation_blocks,
            obj,
        } = self;

        Archive {
            inner: Arc::new(ArchiveInner {
                unpack_xattrs,
                preserve_permissions,
                preserve_mtime,
                allow_external_symlinks,
                overwrite,
                ignore_zeros,
                pax_only,
                max_extension_entry_size,
                max_total_extension_size,
                max_physical_entries,
                max_sparse_entries,
                max_sparse_continuation_blocks,
                obj: Mutex::new(obj),
                pos: 0.into(),
                physical_entries: 0.into(),
                extension_bytes: 0.into(),
            }),
        }
    }
}

impl<R: Read + Unpin> Archive<R> {
    /// Create a new archive with the underlying object as the reader.
    pub fn new(obj: R) -> Archive<R> {
        Archive {
            inner: Arc::new(ArchiveInner {
                unpack_xattrs: false,
                preserve_permissions: false,
                preserve_mtime: true,
                allow_external_symlinks: true,
                overwrite: true,
                ignore_zeros: false,
                pax_only: false,
                max_extension_entry_size: None,
                max_total_extension_size: None,
                max_physical_entries: None,
                max_sparse_entries: None,
                max_sparse_continuation_blocks: None,
                obj: Mutex::new(obj),
                pos: 0.into(),
                physical_entries: 0.into(),
                extension_bytes: 0.into(),
            }),
        }
    }

    /// Unwrap this archive, returning the underlying object.
    pub fn into_inner(self) -> Result<R, Self> {
        let Self { inner } = self;

        match Arc::try_unwrap(inner) {
            Ok(inner) => Ok(inner.obj.into_inner()),
            Err(inner) => Err(Self { inner }),
        }
    }

    /// Construct an stream over the entries in this archive.
    ///
    /// Note that care must be taken to consider each entry within an archive in
    /// sequence. If entries are processed out of sequence (from what the
    /// stream returns), then the contents read for each entry may be
    /// corrupted.
    pub fn entries(&mut self) -> io::Result<Entries<R>> {
        if self.inner.pos.load(Ordering::SeqCst) != 0 {
            return Err(other(
                "cannot call entries unless archive is at \
                 position 0",
            ));
        }

        Ok(Entries {
            archive: self.clone(),
            pending: None,
            sparse: None,
            failed: false,
            current: (0, None, 0, None),
            gnu_longlink: (false, None),
            gnu_longname: (false, None),
            pax_extensions: (false, None),
        })
    }

    /// Construct an stream over the raw entries in this archive.
    ///
    /// Note that care must be taken to consider each entry within an archive in
    /// sequence. If entries are processed out of sequence (from what the
    /// stream returns), then the contents read for each entry may be
    /// corrupted.
    ///
    /// **IMPORTANT**: Most users want [`Self::entries`], not this API.
    /// This API returns every *physical* entry in the archive, rather
    /// than their composed "logical" entries. It should only be used
    /// for low-level diagnostic parsing.
    pub fn entries_raw(&mut self) -> io::Result<RawEntries<R>> {
        if self.inner.pax_only {
            return Err(other("raw entries are not supported by pax-only mode"));
        }
        if self.inner.pos.load(Ordering::SeqCst) != 0 {
            return Err(other(
                "cannot call entries_raw unless archive is at \
                 position 0",
            ));
        }

        Ok(RawEntries {
            archive: self.clone(),
            current: (0, None, 0),
        })
    }

    /// Unpacks the contents tarball into the specified `dst`.
    ///
    /// This function will iterate over the entire contents of this tarball,
    /// extracting each file in turn to the location specified by the entry's
    /// path name.
    ///
    /// This operation is relatively sensitive in that it will not write files
    /// outside of the path specified by `dst`. Files in the archive which have
    /// a '..' in their path are skipped during the unpacking process.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> { tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// #
    /// use tokio::fs::File;
    /// use tokio_tar::Archive;
    ///
    /// let mut ar = Archive::new(File::open("foo.tar").await?);
    /// ar.unpack("foo").await?;
    /// #
    /// # Ok(()) }) }
    /// ```
    pub async fn unpack<P: AsRef<Path>>(&mut self, dst: P) -> io::Result<()> {
        let mut entries = self.entries()?;
        let mut pinned = Pin::new(&mut entries);
        let dst = dst.as_ref();

        if fs::symlink_metadata(dst).await.is_err() {
            fs::create_dir_all(&dst)
                .await
                .map_err(|e| TarError::new(format!("failed to create `{}`", dst.display()), e))?;
        }

        // Canonicalizing the dst directory will prepend the path with '\\?\'
        // on windows which will allow windows APIs to treat the path as an
        // extended-length path with a 32,767 character limit. Otherwise all
        // unpacked paths over 260 characters will fail on creation with a
        // NotFound exception.
        let dst = fs::canonicalize(dst).await?;

        // Memoize filesystem calls to canonicalize paths.
        let mut targets = FxHashSet::default();

        // Delay any directory entries until the end (they will be created if needed by
        // descendants), to ensure that directory permissions do not interfere with descendant
        // extraction.
        let mut directories = Vec::new();
        while let Some(entry) = pinned.next().await {
            let mut file = entry.map_err(|e| TarError::new("failed to iterate over archive", e))?;
            if file.header().entry_type() == crate::EntryType::Directory {
                directories.push(file);
            } else {
                file.unpack_in_raw(&dst, &mut targets).await?;
            }
        }

        // Apply the directories.
        //
        // Note: the order of application is important to permissions. That is, we must traverse
        // the filesystem graph in topological ordering or else we risk not being able to create
        // child directories within those of more restrictive permissions. See [0] for details.
        //
        // [0]: <https://github.com/alexcrichton/tar-rs/issues/242>

        // Validate paths and pair with entries, then sort
        let mut dirs_with_paths: Vec<(Entry<Archive<R>>, Vec<u8>)> = directories
            .into_iter()
            .map(|dir| {
                let path = dir
                    .path_bytes()
                    .map_err(|e| TarError::new("failed to read directory path from archive", e))?
                    .into_owned();
                Ok((dir, path))
            })
            .collect::<io::Result<_>>()?;

        // Sort by path (reverse order for topological sorting)
        dirs_with_paths.sort_by(|a, b| b.1.cmp(&a.1));

        // Unpack directories in sorted order
        for (mut dir, _path) in dirs_with_paths {
            dir.unpack_in_raw(&dst, &mut targets).await?;
        }

        Ok(())
    }
}

/// Stream of `Entry`s.
pub struct Entries<R: Read + Unpin> {
    archive: Archive<R>,
    current: (u64, Option<Header>, usize, Option<Vec<u8>>),
    /// The [`Entry`] that is currently being processed.
    pending: Option<Entry<Archive<R>>>,
    /// Persistent parser state for a GNU sparse entry.
    sparse: Option<SparseState<R>>,
    /// Whether a sparse parser error made the stream unsafe to resume.
    failed: bool,
    /// GNU long name extension.
    ///
    /// The first element is a flag indicating whether the long name entry has been fully read.
    /// The second element is the buffer containing the long name, or `None` if the long name entry
    /// has not been encountered yet.
    gnu_longname: (bool, Option<Vec<u8>>),
    /// GNU long link extension.
    ///
    /// The first element is a flag indicating whether the long link entry has been fully read.
    /// The second element is the buffer containing the long link, or `None` if the long link entry
    /// has not been encountered yet.
    gnu_longlink: (bool, Option<Vec<u8>>),
    /// PAX extensions.
    ///
    /// The first element is a flag indicating whether the extension entry has been fully read.
    /// The second element is the buffer containing the extension, or `None` if the extension entry
    /// has not been encountered yet.
    pax_extensions: (bool, Option<Vec<u8>>),
}

impl<R: Read + Unpin> Stream for Entries<R> {
    type Item = io::Result<Entry<Archive<R>>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if self.failed {
                return Poll::Ready(None);
            }

            if self.sparse.is_some() {
                let parsed = {
                    let this = self.as_mut().get_mut();
                    let Entries { sparse, current, .. } = this;
                    sparse.as_mut().unwrap().poll(cx, &mut current.0)
                };
                match parsed {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(())) => {
                        let state = self.as_mut().get_mut().sparse.take().unwrap();
                        return Poll::Ready(Some(Ok(state.into_entry())));
                    }
                    Poll::Ready(Err(err)) => {
                        let this = self.as_mut().get_mut();
                        this.sparse = None;
                        this.failed = true;
                        return Poll::Ready(Some(Err(err)));
                    }
                }
            }

            let archive = self.archive.clone();

            let entry = if let Some(entry) = self.pending.take() {
                entry
            } else {
                let has_pending_extension =
                    self.gnu_longname.0 || self.gnu_longlink.0 || self.pax_extensions.0;
                let (next, current_header, current_header_pos, pax_extensions) =
                    &mut self.current;
                match futures_core::ready!(poll_next_raw(
                    archive,
                    next,
                    current_header,
                    current_header_pos,
                    cx,
                    pax_extensions.as_deref(),
                    true,
                    has_pending_extension,
                )) {
                    Some(Ok(entry)) => entry,
                    Some(Err(err)) => return Poll::Ready(Some(Err(err))),
                    None => {
                        if self.archive.inner.pax_only && self.pax_extensions.0 {
                            self.pax_extensions.0 = false;
                            return Poll::Ready(Some(Err(other(
                                "local pax header is not followed by an archive entry",
                            ))));
                        }
                        if self.gnu_longname.0 || self.gnu_longlink.0 || self.pax_extensions.0 {
                            return Poll::Ready(Some(Err(other(
                                "extension entry was not followed by a member",
                            ))));
                        }
                        return Poll::Ready(None);
                    }
                }
            };

            let is_ustar_header = entry.header().as_ustar().is_some();
            let permits_gnu_extensions = entry.header().as_gnu().is_some() || is_ustar_header;
            let entry_type = entry.header().entry_type();

            if (!permits_gnu_extensions
                && (entry_type.is_gnu_longname() || entry_type.is_gnu_longlink()))
                || (!is_ustar_header && entry_type.is_pax_local_extensions())
            {
                return Poll::Ready(Some(Err(other(
                    "extension typeflag is not permitted on an unrecognized header",
                ))));
            }

            if permits_gnu_extensions && entry_type.is_gnu_longname() {
                if self.pax_extensions.0 {
                    for extension in pax_extensions(self.pax_extensions.1.as_deref().unwrap()) {
                        let extension = match extension {
                            Ok(extension) => extension,
                            Err(err) => return Poll::Ready(Some(Err(err))),
                        };
                        if extension.key_bytes() == b"path" {
                            return Poll::Ready(Some(Err(other(
                                "ambiguous path: pax path and GNU longname describe the same member",
                            ))));
                        }
                    }
                }
                if self.gnu_longname.0 {
                    return Poll::Ready(Some(Err(other(
                        "two long name entries describing \
                         the same member",
                    ))));
                }

                let mut ef = EntryFields::from(entry);
                let cursor = self.gnu_longname.1.get_or_insert_with(|| {
                    let cap = cmp::min(ef.size, 128 * 1024);
                    Vec::with_capacity(cap as usize)
                });
                if let Poll::Ready(result) = Pin::new(&mut ef).poll_read_all(cx, cursor) {
                    if let Err(err) = result {
                        return Poll::Ready(Some(Err(err)));
                    }
                } else {
                    self.pending = Some(ef.into_entry());
                    return Poll::Pending;
                }

                self.gnu_longname.0 = true;
                continue;
            }

            if permits_gnu_extensions && entry_type.is_gnu_longlink() {
                if self.pax_extensions.0 {
                    for extension in pax_extensions(self.pax_extensions.1.as_deref().unwrap()) {
                        let extension = match extension {
                            Ok(extension) => extension,
                            Err(err) => return Poll::Ready(Some(Err(err))),
                        };
                        if extension.key_bytes() == b"linkpath" {
                            return Poll::Ready(Some(Err(other(
                                "ambiguous link target: pax linkpath and GNU longlink describe the same member",
                            ))));
                        }
                    }
                }
                if self.gnu_longlink.0 {
                    return Poll::Ready(Some(Err(other(
                        "two long name entries describing \
                         the same member",
                    ))));
                }

                let mut ef = EntryFields::from(entry);
                let cursor = self.gnu_longlink.1.get_or_insert_with(|| {
                    let cap = cmp::min(ef.size, 128 * 1024);
                    Vec::with_capacity(cap as usize)
                });
                if let Poll::Ready(result) = Pin::new(&mut ef).poll_read_all(cx, cursor) {
                    if let Err(err) = result {
                        return Poll::Ready(Some(Err(err)));
                    }
                } else {
                    self.pending = Some(ef.into_entry());
                    return Poll::Pending;
                }

                self.gnu_longlink.0 = true;
                continue;
            }

            if is_ustar_header && entry.header().is_pax_local_extensions() {
                if self.pax_extensions.0 {
                    return Poll::Ready(Some(Err(other(
                        "two pax extensions entries describing \
                         the same member",
                    ))));
                }

                let mut ef = EntryFields::from(entry);
                let cursor = self.pax_extensions.1.get_or_insert_with(|| {
                    let cap = cmp::min(ef.size, 128 * 1024);
                    Vec::with_capacity(cap as usize)
                });
                if let Poll::Ready(result) = Pin::new(&mut ef).poll_read_all(cx, cursor) {
                    if let Err(err) = result {
                        return Poll::Ready(Some(Err(err)));
                    }
                } else {
                    self.pending = Some(ef.into_entry());
                    return Poll::Pending;
                }

                let mut has_linkpath = false;
                for extension in pax_extensions(self.pax_extensions.1.as_deref().unwrap()) {
                    let extension = match extension {
                        Ok(extension) => extension,
                        Err(err) => return Poll::Ready(Some(Err(err))),
                    };
                    if extension.value_bytes().is_empty() {
                        return Poll::Ready(Some(Err(other(
                            "empty values are not supported in local pax extensions",
                        ))));
                    }
                    if self.gnu_longname.0 && extension.key_bytes() == b"path" {
                        return Poll::Ready(Some(Err(other(
                            "ambiguous path: pax path and GNU longname describe the same member",
                        ))));
                    }
                    if extension.key_bytes() == b"linkpath" {
                        has_linkpath = true;
                    }
                }
                if has_linkpath && self.gnu_longlink.0 {
                    return Poll::Ready(Some(Err(other(
                        "ambiguous link target: pax linkpath and GNU longlink describe the same member",
                    ))));
                }

                self.pax_extensions.0 = true;
                self.current.3 = self.pax_extensions.1.clone();
                continue;
            }

            let mut fields = EntryFields::from(entry);
            if self.gnu_longname.0 {
                fields.long_pathname = self.gnu_longname.1.take();
                self.gnu_longname.0 = false;
            }
            if self.gnu_longlink.0 {
                fields.long_linkname = self.gnu_longlink.1.take();
                self.gnu_longlink.0 = false;
            }
            if self.pax_extensions.0 {
                fields.pax_extensions = self.pax_extensions.1.take();
                self.pax_extensions.0 = false;
                self.current.3 = None;
            }

            if fields.header.entry_type().is_gnu_sparse() {
                match SparseState::new(self.archive.clone(), fields) {
                    Ok(state) => {
                        self.sparse = Some(state);
                        continue;
                    }
                    Err(err) => {
                        self.failed = true;
                        return Poll::Ready(Some(Err(err)));
                    }
                }
            }

            return Poll::Ready(Some(Ok(fields.into_entry())));
        }
    }
}

/// Stream of raw `Entry`s.
pub struct RawEntries<R: Read + Unpin> {
    archive: Archive<R>,
    current: (u64, Option<Header>, usize),
}

impl<R: Read + Unpin> Stream for RawEntries<R> {
    type Item = io::Result<Entry<Archive<R>>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let archive = self.archive.clone();
        let (next, current_header, current_header_pos) = &mut self.current;
        poll_next_raw(
            archive,
            next,
            current_header,
            current_header_pos,
            cx,
            None,
            false,
            false,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn poll_next_raw<R: Read + Unpin>(
    mut archive: Archive<R>,
    next: &mut u64,
    current_header: &mut Option<Header>,
    current_header_pos: &mut usize,
    cx: &mut Context<'_>,
    pax_extensions_data: Option<&[u8]>,
    apply_pax_header_fields: bool,
    has_pending_extension: bool,
) -> Poll<Option<io::Result<Entry<Archive<R>>>>> {
    let mut header_pos = *next;

    loop {
        // Seek to the start of the next header in the archive
        if current_header.is_none() {
            let delta = *next - archive.inner.pos.load(Ordering::SeqCst);
            match futures_core::ready!(poll_skip(&mut archive, cx, delta)) {
                Ok(_) => {}
                Err(err) => return Poll::Ready(Some(Err(err))),
            }

            *current_header = Some(Header::new_old());
            *current_header_pos = 0;
        }

        let header = current_header.as_mut().unwrap();

        // EOF is an indicator that we are at the end of the archive.
        match futures_core::ready!(poll_try_read_all(
            &mut archive,
            cx,
            header.as_mut_bytes(),
            current_header_pos,
        )) {
            Ok(true) => {}
            Ok(false) => return Poll::Ready(None),
            Err(err) => return Poll::Ready(Some(Err(err))),
        }

        // If a header is not all zeros, we have another valid header.
        // Otherwise, check if we are ignoring zeros and continue, or break as if this is the
        // end of the archive.
        if !header.as_bytes().iter().all(|i| *i == 0) {
            *next += BLOCK_SIZE;
            break;
        }

        if has_pending_extension {
            if archive.inner.pax_only {
                return Poll::Ready(None);
            }
            return Poll::Ready(Some(Err(other(
                "extension entry was not followed by a member",
            ))));
        }

        if !archive.inner.ignore_zeros {
            return Poll::Ready(None);
        }

        *next += BLOCK_SIZE;
        header_pos = *next;
    }

    let header = current_header.as_mut().unwrap();

    // Make sure the checksum is ok
    let sum = header.as_bytes()[..148]
        .iter()
        .chain(&header.as_bytes()[156..])
        .fold(0, |a, b| a + (*b as u32))
        + 8 * 32;
    let cksum = header.cksum()?;
    if sum != cksum {
        return Poll::Ready(Some(Err(other("archive header checksum mismatch"))));
    }
    if header.is_ambiguous_nul_version_ustar() {
        return Poll::Ready(Some(Err(other(
            "NUL-version USTAR header is ambiguous and not supported",
        ))));
    }

    if archive.inner.pax_only && !is_pax_header(header) {
        return Poll::Ready(Some(Err(other(
            "archive header is not allowed by pax-only mode",
        ))));
    }

    let file_pos = *next;

    let mut header = current_header.take().unwrap();

    // note when pax extensions are available, the size from the header will be ignored
    let mut size = header.raw_entry_size()?;
    let mut pax_username = None;
    let mut pax_groupname = None;

    // PAX extensions describe the next *file entry*, not intermediary extensions.
    // See: "pax Header Block," `x` typeflag:
    // Ref: <https://pubs.opengroup.org/onlinepubs/9799919799/utilities/pax.html>
    let entry_type = header.entry_type();
    let is_extension_header = entry_type.is_gnu_longname()
        || entry_type.is_gnu_longlink()
        || header.is_pax_local_extensions()
        || entry_type.is_pax_global_extensions();

    archive.inner.reserve_entry(size, is_extension_header)?;

    // the size above will be overriden by the pax data if it has a size field.
    // same for uid and gid, which will be overridden in the header itself.
    let mut has_gnu_sparse_metadata = false;
    let mut has_recognized_gnu_sparse_format = false;
    let mut sparse_major_1 = false;
    let mut sparse_minor_0 = false;
    if let Some(pax) = pax_extensions_data
        .filter(|_| !is_extension_header)
        .map(pax_extensions)
    {
        for extension in pax {
            let extension = extension?;

            // ignore keys that aren't parsable as a string at this stage.
            // that isn't relevant to the size/uid/gid processing.
            let Ok(key) = extension.key() else {
                continue;
            };

            has_gnu_sparse_metadata |= key.starts_with("GNU.sparse.");

            match key {
                "size" => {
                    let size_str = extension
                        .value()
                        .map_err(|_e| other("failed to parse pax size as string"))?;
                    size = parse_pax_decimal(size_str, "failed to parse pax size")?;
                }

                "uid" => {
                    if apply_pax_header_fields {
                        let uid_str = extension
                            .value()
                            .map_err(|_e| other("failed to parse pax uid as string"))?;
                        header.set_uid(parse_pax_decimal(uid_str, "failed to parse pax uid")?);
                    }
                }

                "gid" => {
                    if apply_pax_header_fields {
                        let gid_str = extension
                            .value()
                            .map_err(|_e| other("failed to parse pax gid as string"))?;
                        header.set_gid(parse_pax_decimal(gid_str, "failed to parse pax gid")?);
                    }
                }

                "uname" => {
                    let value = extension.value_bytes();
                    pax_username = Some(PaxOwnerName::from_bytes(value));
                    if !value.is_empty() {
                        let Ok(uname) = extension.value() else {
                            continue;
                        };
                        // Keep the existing effective-header behavior when the
                        // physical field can represent the override.
                        let _ = header.set_username(uname);
                    }
                }

                "gname" => {
                    let value = extension.value_bytes();
                    pax_groupname = Some(PaxOwnerName::from_bytes(value));
                    if !value.is_empty() {
                        let Ok(gname) = extension.value() else {
                            continue;
                        };
                        let _ = header.set_groupname(gname);
                    }
                }

                "GNU.sparse.map" | "GNU.sparse.size" => {
                    has_recognized_gnu_sparse_format = true;
                }

                "GNU.sparse.major" => {
                    sparse_major_1 = extension.value().ok() == Some("1");
                }

                "GNU.sparse.minor" => {
                    sparse_minor_0 = extension.value().ok() == Some("0");
                }

                _ => {
                    continue;
                }
            }
        }
    }

    if has_gnu_sparse_metadata
        && !(has_recognized_gnu_sparse_format || sparse_major_1 && sparse_minor_0)
    {
        return Poll::Ready(Some(Err(other(
            "orphaned GNU sparse pax metadata is not supported",
        ))));
    }

    let data = if entry_type.is_gnu_sparse() && apply_pax_header_fields {
        VecDeque::new()
    } else {
        let mut data = VecDeque::with_capacity(1);
        data.push_back(EntryIo::Data(archive.clone().take(size)));
        data
    };

    let ret = EntryFields {
        size,
        header_pos,
        file_pos,
        data,
        header,
        long_pathname: None,
        long_linkname: None,
        pax_extensions: None,
        pax_username,
        pax_groupname,
        unpack_xattrs: archive.inner.unpack_xattrs,
        preserve_permissions: archive.inner.preserve_permissions,
        preserve_mtime: archive.inner.preserve_mtime,
        overwrite: archive.inner.overwrite,
        allow_external_symlinks: archive.inner.allow_external_symlinks,
        read_state: None,
    };

    // Store where the next entry is, rounding up by 512 bytes (the size of
    // a header);
    let size = size
        .checked_add(BLOCK_SIZE - 1)
        .ok_or_else(|| other("size overflow"))?;
    *next = next
        .checked_add(size & !(BLOCK_SIZE - 1))
        .ok_or_else(|| other("size overflow"))?;

    Poll::Ready(Some(Ok(ret.into_entry())))
}

fn is_pax_header(header: &Header) -> bool {
    let Some(ustar) = header.as_ustar() else {
        return false;
    };

    is_pax_entry_type(header.entry_type()) && has_pax_numeric_fields(ustar)
}

fn is_pax_entry_type(entry_type: crate::EntryType) -> bool {
    matches!(
        entry_type,
        crate::EntryType::Regular
            | crate::EntryType::Link
            | crate::EntryType::Symlink
            | crate::EntryType::Char
            | crate::EntryType::Block
            | crate::EntryType::Directory
            | crate::EntryType::Fifo
            | crate::EntryType::XHeader
    )
}

fn has_pax_numeric_fields(header: &crate::UstarHeader) -> bool {
    [
        &header.mode[..],
        &header.uid[..],
        &header.gid[..],
        &header.size[..],
        &header.mtime[..],
        &header.dev_major[..],
        &header.dev_minor[..],
    ]
    .into_iter()
    .all(|field| {
        field
            .iter()
            .all(|byte| matches!(byte, b'\0' | b' ' | b'0'..=b'7'))
    })
}

fn parse_pax_decimal(value: &str, parse_error: &'static str) -> io::Result<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(other(parse_error));
    }

    value.parse::<u64>().map_err(|_e| other(parse_error))
}

struct SparseState<R: Read + Unpin> {
    archive: Archive<R>,
    entry: EntryFields<Archive<R>>,
    cur: u64,
    remaining: u64,
    sparse_entries: u64,
    continuation_blocks: u64,
    continuation: GnuExtSparseHeader,
    continuation_pos: usize,
    needs_continuation: bool,
    continuation_reserved: bool,
}

impl<R: Read + Unpin> SparseState<R> {
    fn new(archive: Archive<R>, mut entry: EntryFields<Archive<R>>) -> io::Result<Self> {
        let gnu = entry
            .header
            .as_gnu()
            .ok_or_else(|| other("sparse entry type listed but not GNU header"))?;
        let needs_continuation = gnu.is_extended();
        let remaining = entry.size;
        entry.data.clear();

        let mut state = Self {
            archive,
            entry,
            cur: 0,
            remaining,
            sparse_entries: 0,
            continuation_blocks: 0,
            continuation: GnuExtSparseHeader::new(),
            continuation_pos: 0,
            needs_continuation,
            continuation_reserved: false,
        };

        for index in 0..4 {
            let block = {
                let gnu = state.entry.header.as_gnu().unwrap();
                parse_sparse_block(&gnu.sparse[index])?
            };
            if let Some((offset, length)) = block {
                state.add_block(offset, length)?;
            }
        }

        Ok(state)
    }

    fn poll(&mut self, cx: &mut Context<'_>, next: &mut u64) -> Poll<io::Result<()>> {
        while self.needs_continuation {
            if let Err(err) = self.reserve_continuation() {
                return Poll::Ready(Err(err));
            }

            match futures_core::ready!(poll_try_read_all(
                &mut self.archive,
                cx,
                self.continuation.as_mut_bytes(),
                &mut self.continuation_pos,
            )) {
                Ok(true) => {}
                Ok(false) => return Poll::Ready(Err(other("failed to read extension"))),
                Err(err) => return Poll::Ready(Err(err)),
            }

            self.continuation_reserved = false;
            if let Err(err) = advance_sparse_positions(next, &mut self.entry.file_pos) {
                return Poll::Ready(Err(err));
            }

            for index in 0..self.continuation.sparse.len() {
                let block = parse_sparse_block(&self.continuation.sparse[index]);
                let block = match block {
                    Ok(block) => block,
                    Err(err) => return Poll::Ready(Err(err)),
                };
                if let Some((offset, length)) = block {
                    if let Err(err) = self.add_block(offset, length) {
                        return Poll::Ready(Err(err));
                    }
                }
            }
            self.needs_continuation = self.continuation.is_extended();
        }

        let real_size = self.entry.header.as_gnu().unwrap().real_size();
        let real_size = match real_size {
            Ok(real_size) => real_size,
            Err(err) => return Poll::Ready(Err(err)),
        };
        if self.cur != real_size {
            return Poll::Ready(Err(other(
                "mismatch in sparse file chunks and \
                 size in header",
            )));
        }
        self.entry.size = self.cur;
        if self.remaining > 0 {
            return Poll::Ready(Err(other(
                "mismatch in sparse file chunks and \
                 entry size in header",
            )));
        }

        Poll::Ready(Ok(()))
    }

    fn add_block(&mut self, offset: u64, length: u64) -> io::Result<()> {
        if length != 0 && (self.entry.size - self.remaining) % BLOCK_SIZE != 0 {
            return Err(other(
                "previous block in sparse file was not \
                 aligned to 512-byte boundary",
            ));
        }
        if offset < self.cur {
            return Err(other(
                "out of order or overlapping sparse \
                 blocks",
            ));
        }

        let next_cur = offset
            .checked_add(length)
            .ok_or_else(|| other("more bytes listed in sparse file than u64 can hold"))?;
        let next_remaining = self.remaining.checked_sub(length).ok_or_else(|| {
            other(
                "sparse file consumed more data than the header \
                 listed",
            )
        })?;

        if let Some(limit) = self.archive.inner.max_sparse_entries {
            let next = self.sparse_entries.checked_add(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "archive sparse entry limit exceeded",
                )
            })?;
            if next > limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "archive sparse entry limit exceeded",
                ));
            }
            self.sparse_entries = next;
        }

        if self.cur < offset {
            let block = io::repeat(0).take(offset - self.cur);
            self.entry.data.push_back(EntryIo::Pad(block));
        }
        self.cur = next_cur;
        self.remaining = next_remaining;
        self.entry
            .data
            .push_back(EntryIo::Data(self.archive.clone().take(length)));
        Ok(())
    }

    fn reserve_continuation(&mut self) -> io::Result<()> {
        if self.continuation_reserved {
            return Ok(());
        }

        if let Some(limit) = self.archive.inner.max_sparse_continuation_blocks {
            let next = self.continuation_blocks.checked_add(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "archive sparse continuation block limit exceeded",
                )
            })?;
            if next > limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "archive sparse continuation block limit exceeded",
                ));
            }
            self.continuation_blocks = next;
        }
        self.continuation_reserved = true;
        Ok(())
    }

    fn into_entry(self) -> Entry<Archive<R>> {
        self.entry.into_entry()
    }
}

fn parse_sparse_block(block: &GnuSparseHeader) -> io::Result<Option<(u64, u64)>> {
    if block.is_empty() {
        return Ok(None);
    }
    Ok(Some((block.offset()?, block.length()?)))
}

fn advance_sparse_positions(next: &mut u64, file_pos: &mut u64) -> io::Result<()> {
    let advanced_next = next
        .checked_add(BLOCK_SIZE)
        .ok_or_else(|| other("position overflow"))?;
    let advanced_file_pos = file_pos
        .checked_add(BLOCK_SIZE)
        .ok_or_else(|| other("position overflow"))?;
    *next = advanced_next;
    *file_pos = advanced_file_pos;
    Ok(())
}

impl<R: Read + Unpin> Read for Archive<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        into: &mut io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut r = if let Ok(v) = self.inner.obj.try_lock() {
            v
        } else {
            return Poll::Pending;
        };

        let res = futures_core::ready!(Pin::new(&mut *r).poll_read(cx, into));
        match res {
            Ok(()) => {
                self.inner
                    .pos
                    .fetch_add(into.filled().len() as u64, Ordering::SeqCst);
                Poll::Ready(Ok(()))
            }
            Err(err) => Poll::Ready(Err(err)),
        }
    }
}

/// Try to fill the buffer from the reader.
///
/// If the reader reaches its end before filling the buffer at all, returns `false`.
/// Otherwise returns `true`.
fn poll_try_read_all<R: Read + Unpin>(
    mut source: R,
    cx: &mut Context<'_>,
    buf: &mut [u8],
    pos: &mut usize,
) -> Poll<io::Result<bool>> {
    while *pos < buf.len() {
        let mut read_buf = io::ReadBuf::new(&mut buf[*pos..]);
        match futures_core::ready!(Pin::new(&mut source).poll_read(cx, &mut read_buf)) {
            Ok(()) if read_buf.filled().is_empty() => {
                if *pos == 0 {
                    return Poll::Ready(Ok(false));
                }

                return Poll::Ready(Err(other("failed to read entire block")));
            }
            Ok(()) => *pos += read_buf.filled().len(),
            Err(err) => return Poll::Ready(Err(err)),
        }
    }

    *pos = 0;
    Poll::Ready(Ok(true))
}

/// Skip n bytes on the given source.
fn poll_skip<R: Read + Unpin>(
    mut source: R,
    cx: &mut Context<'_>,
    mut amt: u64,
) -> Poll<io::Result<()>> {
    let mut buf = [0u8; 4096 * 8];
    while amt > 0 {
        let n = cmp::min(amt, buf.len() as u64);
        let mut read_buf = io::ReadBuf::new(&mut buf[..n as usize]);
        match futures_core::ready!(Pin::new(&mut source).poll_read(cx, &mut read_buf)) {
            Ok(()) if read_buf.filled().is_empty() => {
                return Poll::Ready(Err(other("unexpected EOF during skip")));
            }
            Ok(()) => {
                amt -= read_buf.filled().len() as u64;
            }
            Err(err) => return Poll::Ready(Err(err)),
        }
    }

    Poll::Ready(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_with_limit_accepts_exact_boundary() {
        let counter = AtomicU64::new(7);

        reserve_with_limit(&counter, 3, 10, "limit exceeded").unwrap();

        assert_eq!(counter.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn reserve_with_limit_does_not_mutate_counter_on_rejection() {
        let counter = AtomicU64::new(7);

        let err = reserve_with_limit(&counter, 4, 10, "limit exceeded").unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(counter.load(Ordering::Relaxed), 7);
    }

    #[test]
    fn reserve_with_limit_rejects_counter_overflow() {
        let counter = AtomicU64::new(u64::MAX);

        let err = reserve_with_limit(&counter, 1, u64::MAX, "limit exceeded").unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn advance_sparse_positions_accepts_exact_boundary() {
        let mut next = u64::MAX - BLOCK_SIZE;
        let mut file_pos = u64::MAX - BLOCK_SIZE;

        advance_sparse_positions(&mut next, &mut file_pos).unwrap();

        assert_eq!(next, u64::MAX);
        assert_eq!(file_pos, u64::MAX);
    }

    #[test]
    fn advance_sparse_positions_does_not_commit_on_next_overflow() {
        let mut next = u64::MAX - BLOCK_SIZE + 1;
        let mut file_pos = 17;

        let err = advance_sparse_positions(&mut next, &mut file_pos).unwrap_err();

        assert_eq!(err.to_string(), "position overflow");
        assert_eq!(next, u64::MAX - BLOCK_SIZE + 1);
        assert_eq!(file_pos, 17);
    }

    #[test]
    fn advance_sparse_positions_does_not_commit_on_file_position_overflow() {
        let mut next = 23;
        let mut file_pos = u64::MAX - BLOCK_SIZE + 1;

        let err = advance_sparse_positions(&mut next, &mut file_pos).unwrap_err();

        assert_eq!(err.to_string(), "position overflow");
        assert_eq!(next, 23);
        assert_eq!(file_pos, u64::MAX - BLOCK_SIZE + 1);
    }
}
