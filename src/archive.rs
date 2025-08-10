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

use crate::header::BLOCK_SIZE;
use crate::{
    entry::{EntryFields, EntryIo},
    error::{InvalidArchive, TarError},
    Entry, GnuExtSparseHeader, GnuSparseHeader, Header,
};

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
    unpack_xattrs: bool,
    preserve_permissions: bool,
    preserve_mtime: bool,
    allow_external_symlinks: bool,
    overwrite: bool,
    ignore_zeros: bool,
    obj: Mutex<R>,
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
                obj: Mutex::new(obj),
                pos: 0.into(),
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
                obj: Mutex::new(obj),
                pos: 0.into(),
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
            return Err(TarError::InvalidArchive(InvalidArchive::ArchiveNotAtStart {
                method: "entries".to_string(),
            }).into());
        }

        Ok(Entries {
            archive: self.clone(),
            pending: None,
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
    pub fn entries_raw(&mut self) -> io::Result<RawEntries<R>> {
        if self.inner.pos.load(Ordering::SeqCst) != 0 {
            return Err(TarError::InvalidArchive(InvalidArchive::ArchiveNotAtStart {
                method: "entries_raw".to_string(),
            }).into());
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
                .map_err(|e| TarError::CreateFailed {
                    path: dst.to_path_buf(),
                    source: e,
                })?;
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
            let mut file = entry.map_err(TarError::IterationFailed)?;
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
        directories.sort_by(|a, b| b.path_bytes().cmp(&a.path_bytes()));
        for mut dir in directories {
            dir.unpack_in_raw(&dst, &mut targets).await?;
        }

        Ok(())
    }
}

/// Stream of `Entry`s.
pub struct Entries<R: Read + Unpin> {
    archive: Archive<R>,
    current: (u64, Option<Header>, usize, Option<GnuExtSparseHeader>),
    /// The [`Entry`] that is currently being processed.
    pending: Option<Entry<Archive<R>>>,
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

macro_rules! ready_opt_err {
    ($val:expr) => {
        match futures_core::ready!($val) {
            Some(Ok(val)) => val,
            Some(Err(err)) => return Poll::Ready(Some(Err(err))),
            None => return Poll::Ready(None),
        }
    };
}

macro_rules! ready_err {
    ($val:expr) => {
        match futures_core::ready!($val) {
            Ok(val) => val,
            Err(err) => return Poll::Ready(Some(Err(err))),
        }
    };
}

impl<R: Read + Unpin> Stream for Entries<R> {
    type Item = io::Result<Entry<Archive<R>>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            let archive = self.archive.clone();

            let entry = if let Some(entry) = self.pending.take() {
                entry
            } else {
                let (next, current_header, current_header_pos, _) = &mut self.current;
                ready_opt_err!(poll_next_raw(
                    archive,
                    next,
                    current_header,
                    current_header_pos,
                    cx
                ))
            };

            let is_recognized_header =
                entry.header().as_gnu().is_some() || entry.header().as_ustar().is_some();

            if is_recognized_header && entry.header().entry_type().is_gnu_longname() {
                if self.gnu_longname.0 {
                    return Poll::Ready(Some(Err(TarError::InvalidArchive(InvalidArchive::DuplicateEntries {
                        entry_type: "long name".to_string(),
                    }).into())));
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

            if is_recognized_header && entry.header().entry_type().is_gnu_longlink() {
                if self.gnu_longlink.0 {
                    return Poll::Ready(Some(Err(TarError::InvalidArchive(InvalidArchive::DuplicateEntries {
                        entry_type: "long link".to_string(),
                    }).into())));
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

            if is_recognized_header && entry.header().entry_type().is_pax_local_extensions() {
                if self.pax_extensions.0 {
                    return Poll::Ready(Some(Err(TarError::InvalidArchive(InvalidArchive::DuplicateEntries {
                        entry_type: "pax extensions".to_string(),
                    }).into())));
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

                self.pax_extensions.0 = true;
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
            }

            let archive = self.archive.clone();
            let (next, _, current_pos, current_ext) = &mut self.current;

            ready_err!(poll_parse_sparse_header(
                archive,
                next,
                current_ext,
                current_pos,
                &mut fields,
                cx
            ));

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
        poll_next_raw(archive, next, current_header, current_header_pos, cx)
    }
}

fn poll_next_raw<R: Read + Unpin>(
    mut archive: Archive<R>,
    next: &mut u64,
    current_header: &mut Option<Header>,
    current_header_pos: &mut usize,
    cx: &mut Context<'_>,
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
        return Poll::Ready(Some(Err(TarError::InvalidArchive(InvalidArchive::InvalidChecksum).into())));
    }

    let file_pos = *next;
    let size = header.entry_size()?;

    let mut data = VecDeque::with_capacity(1);
    data.push_back(EntryIo::Data(archive.clone().take(size)));

    let header = current_header.take().unwrap();

    let ret = EntryFields {
        size,
        header_pos,
        file_pos,
        data,
        header,
        long_pathname: None,
        long_linkname: None,
        pax_extensions: None,
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
        .ok_or(TarError::InvalidArchive(InvalidArchive::SizeOverflow))?;
    *next = next
        .checked_add(size & !(BLOCK_SIZE - 1))
        .ok_or(TarError::InvalidArchive(InvalidArchive::SizeOverflow))?;

    Poll::Ready(Some(Ok(ret.into_entry())))
}

fn poll_parse_sparse_header<R: Read + Unpin>(
    mut archive: Archive<R>,
    next: &mut u64,
    current_ext: &mut Option<GnuExtSparseHeader>,
    current_ext_pos: &mut usize,
    entry: &mut EntryFields<Archive<R>>,
    cx: &mut Context<'_>,
) -> Poll<io::Result<()>> {
    if !entry.header.entry_type().is_gnu_sparse() {
        return Poll::Ready(Ok(()));
    }

    let gnu = match entry.header.as_gnu() {
        Some(gnu) => gnu,
        None => return Poll::Ready(Err(TarError::InvalidArchive(InvalidArchive::SparseNotGnu).into())),
    };

    // Sparse files are represented internally as a list of blocks that are
    // read. Blocks are either a bunch of 0's or they're data from the
    // underlying archive.
    //
    // Blocks of a sparse file are described by the `GnuSparseHeader`
    // structure, some of which are contained in `GnuHeader` but some of
    // which may also be contained after the first header in further
    // headers.
    //
    // We read off all the blocks here and use the `add_block` function to
    // incrementally add them to the list of I/O block (in `entry.data`).
    // The `add_block` function also validates that each chunk comes after
    // the previous, we don't overrun the end of the file, and each block is
    // aligned to a 512-byte boundary in the archive itself.
    //
    // At the end we verify that the sparse file size (`Header::size`) is
    // the same as the current offset (described by the list of blocks) as
    // well as the amount of data read equals the size of the entry
    // (`Header::entry_size`).
    entry.data.truncate(0);

    let mut cur = 0;
    let mut remaining = entry.size;
    {
        let data = &mut entry.data;
        let reader = archive.clone();
        let size = entry.size;
        let mut add_block = |block: &GnuSparseHeader| -> io::Result<_> {
            if block.is_empty() {
                return Ok(());
            }
            let off = block.offset()?;
            let len = block.length()?;

            if len != 0 && (size - remaining) % BLOCK_SIZE != 0 {
                return Err(TarError::InvalidArchive(InvalidArchive::SparseAlignment).into());
            } else if off < cur {
                return Err(TarError::InvalidArchive(InvalidArchive::SparseOrdering).into());
            } else if cur < off {
                let block = io::repeat(0).take(off - cur);
                data.push_back(EntryIo::Pad(block));
            }
            cur = off
                .checked_add(len)
                .ok_or(TarError::InvalidArchive(InvalidArchive::SparseOverflow))?;
            remaining = remaining.checked_sub(len).ok_or(TarError::InvalidArchive(InvalidArchive::SparseDataMismatch))?;
            data.push_back(EntryIo::Data(reader.clone().take(len)));
            Ok(())
        };
        for block in gnu.sparse.iter() {
            add_block(block)?
        }
        if gnu.is_extended() {
            let started_header = current_ext.is_some();
            if !started_header {
                let mut ext = GnuExtSparseHeader::new();
                ext.isextended[0] = 1;
                *current_ext = Some(ext);
                *current_ext_pos = 0;
            }

            let ext = current_ext.as_mut().unwrap();
            while ext.is_extended() {
                match futures_core::ready!(poll_try_read_all(
                    &mut archive,
                    cx,
                    ext.as_mut_bytes(),
                    current_ext_pos,
                )) {
                    Ok(true) => {}
                    Ok(false) => return Poll::Ready(Err(TarError::ExtensionReadFailed.into())),
                    Err(err) => return Poll::Ready(Err(err)),
                }

                *next += BLOCK_SIZE;
                for block in ext.sparse.iter() {
                    add_block(block)?;
                }
            }
        }
    }
    if cur != gnu.real_size()? {
        return Poll::Ready(Err(TarError::InvalidArchive(InvalidArchive::SparseSizeMismatch).into()));
    }
    entry.size = cur;
    if remaining > 0 {
        return Poll::Ready(Err(TarError::InvalidArchive(InvalidArchive::SparseEntrySizeMismatch).into()));
    }

    Poll::Ready(Ok(()))
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

                return Poll::Ready(Err(TarError::IncompleteBlockRead.into()));
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
                return Poll::Ready(Err(TarError::UnexpectedEofDuringSkip.into()));
            }
            Ok(()) => {
                amt -= read_buf.filled().len() as u64;
            }
            Err(err) => return Poll::Ready(Err(err)),
        }
    }

    Poll::Ready(Ok(()))
}
