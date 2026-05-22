use crate::{
    header::HeaderMode,
    name::{into_link_target, into_name},
    other, EntryType, Header, LinkTarget, Name,
};
use std::{fmt, fs::Metadata, path::Path};
use tokio::{
    fs,
    io::{self, AsyncRead as Read, AsyncWrite as Write, AsyncWriteExt},
};

/// A structure for building archives
///
/// This structure has methods for building up an archive from scratch into any
/// arbitrary writer.
pub struct Builder<W: Write + Unpin + Send> {
    mode: HeaderMode,
    follow: bool,
    finished: bool,
    obj: Option<W>,
    cancellation: Option<tokio::sync::oneshot::Sender<W>>,
}

/// Metadata overrides for a generated entry written through [`Builder`].
///
/// Filesystem-based builder methods take metadata from their source path.
/// Generated entry methods use conventional permissions unless `mode` is
/// overridden; the other numeric fields default to zero.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EntryMetadata {
    mode: Option<u32>,
    uid: u64,
    gid: u64,
    mtime: u64,
}

impl EntryMetadata {
    /// Creates empty metadata overrides.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the Unix mode bits for the entry.
    pub fn mode(mut self, mode: u32) -> Self {
        self.mode = Some(mode);
        self
    }

    /// Sets the owner user ID for the entry.
    pub fn uid(mut self, uid: u64) -> Self {
        self.uid = uid;
        self
    }

    /// Sets the owner group ID for the entry.
    pub fn gid(mut self, gid: u64) -> Self {
        self.gid = gid;
        self
    }

    /// Sets the modification time, as Unix seconds, for the entry.
    pub fn mtime(mut self, mtime: u64) -> Self {
        self.mtime = mtime;
        self
    }

    fn apply(self, header: &mut Header, default_mode: u32) {
        header.set_mode(self.mode.unwrap_or(default_mode));
        header.set_uid(self.uid);
        header.set_gid(self.gid);
        header.set_mtime(self.mtime);
    }
}

const TERMINATION: &[u8; 1024] = &[0; 1024];
const DEFAULT_FILE_MODE: u32 = 0o644;
const DEFAULT_DIRECTORY_MODE: u32 = 0o755;
const DEFAULT_SYMLINK_MODE: u32 = 0o777;

impl<W: Write + Unpin + Send + 'static> Builder<W> {
    /// Create a new archive builder with the underlying object as the
    /// destination of all data written. The builder will use
    /// `HeaderMode::Complete` by default.
    ///
    /// On drop, would write [`TERMINATION`] into the end of the archive,
    /// use `skip_termination` method to disable this.
    pub fn new(obj: W) -> Builder<W> {
        let (tx, rx) = tokio::sync::oneshot::channel::<W>();
        tokio::spawn(async move {
            if let Ok(mut w) = rx.await {
                let _ = w.write_all(TERMINATION).await;
            }
        });
        Builder {
            mode: HeaderMode::Complete,
            follow: true,
            finished: false,
            obj: Some(obj),
            cancellation: Some(tx),
        }
    }
}

impl<W: Write + Unpin + Send> Builder<W> {
    /// Create a new archive builder with the underlying object as the
    /// destination of all data written. The builder will use
    /// `HeaderMode::Complete` by default.
    ///
    /// The [`TERMINATION`] symbol would not be written to the archive in the end.
    pub fn new_non_terminated(obj: W) -> Builder<W> {
        Builder {
            mode: HeaderMode::Complete,
            follow: true,
            finished: false,
            obj: Some(obj),
            cancellation: None,
        }
    }

    /// Changes the HeaderMode that will be used when reading fs Metadata for
    /// methods that implicitly read metadata for an input Path. Notably, this
    /// does not apply to generated entries supplied with [`EntryMetadata`].
    pub fn mode(&mut self, mode: HeaderMode) {
        self.mode = mode;
    }

    /// Follow symlinks, archiving the contents of the file they point to rather
    /// than adding a symlink to the archive. Defaults to true.
    pub fn follow_symlinks(&mut self, follow: bool) {
        self.follow = follow;
    }

    /// Skip writing final termination bytes into the archive.
    pub fn skip_termination(&mut self) {
        drop(self.cancellation.take());
    }

    /// Gets shared reference to the underlying object.
    pub fn get_ref(&self) -> &W {
        self.obj.as_ref().unwrap()
    }

    fn get_mut(&mut self) -> &mut W {
        self.obj.as_mut().unwrap()
    }

    /// Unwrap this archive, returning the underlying object.
    ///
    /// This function will finish writing the archive if the `finish` function
    /// hasn't yet been called, returning any I/O error which happens during
    /// that operation.
    pub async fn into_inner(mut self) -> io::Result<W> {
        if !self.finished {
            self.finish().await?;
        }
        Ok(self.obj.take().unwrap())
    }

    /// Adds a generated regular-file entry to this archive.
    ///
    /// A PAX `path` extension is emitted when the validated [`Name`] is too
    /// long for USTAR or contains non-ASCII UTF-8. `size` must equal the
    /// number of bytes read from `data`.
    ///
    /// Note that this will not attempt to seek the archive to a valid position,
    /// so if the archive is in the middle of a read or some other similar
    /// operation then this may corrupt the archive.
    ///
    /// Also note that after all entries have been written to an archive the
    /// `finish` function needs to be called to finish writing the archive.
    ///
    /// # Errors
    ///
    /// This function will return an error for any intermittent I/O error which
    /// occurs when either reading or writing.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> { tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// #
    /// use tokio_tar::Builder;
    ///
    /// let mut data: &[u8] = &[1, 2, 3, 4];
    ///
    /// let mut ar = Builder::new(Vec::new());
    /// ar.append_data("really/long/path/to/foo", 4, data).await?;
    /// let data = ar.into_inner().await?;
    /// #
    /// # Ok(()) }) }
    /// ```
    pub async fn append_data<N, R>(&mut self, path: N, size: u64, data: R) -> io::Result<()>
    where
        N: TryInto<Name>,
        N::Error: fmt::Display,
        R: Read + Unpin,
    {
        self.append_data_with_metadata(path, size, EntryMetadata::new(), data)
            .await
    }

    /// Adds a generated regular-file entry with explicitly supplied metadata.
    pub async fn append_data_with_metadata<N, R>(
        &mut self,
        path: N,
        size: u64,
        metadata: EntryMetadata,
        data: R,
    ) -> io::Result<()>
    where
        N: TryInto<Name>,
        N::Error: fmt::Display,
        R: Read + Unpin,
    {
        let path = into_name(path)?;
        let mut header = Header::new_ustar();
        metadata.apply(&mut header, DEFAULT_FILE_MODE);
        header.set_size(size);
        let mut pax_extensions = Vec::new();
        prepare_header_path(&mut header, &path, &mut pax_extensions)?;
        prepare_header_numeric_fields(&mut header, &mut pax_extensions)?;
        validate_header_for_write(&header)?;
        append_pax_extensions(self.get_mut(), &pax_extensions).await?;
        header.set_cksum();
        append_entry(self.get_mut(), &header, data).await
    }

    /// Adds a new hard-link entry to this archive with the specified name and destination.
    ///
    /// This function sets the specified member name and hard-link destination
    /// in the header, appending a
    /// PAX extension entry first when either value is too long for USTAR or
    /// contains non-ASCII characters. The checksum for the header is updated
    /// automatically.
    ///
    /// The entry type is set to [`EntryType::Link`] and the size is set to zero.
    ///
    /// # Errors
    ///
    /// This function will return an error for any intermittent I/O error which
    /// occurs when writing, or if the header cannot describe a USTAR/PAX link
    /// entry.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> { tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// #
    /// use tokio_tar::Builder;
    ///
    /// let mut ar = Builder::new(Vec::new());
    /// ar.append_hard_link("link", "really/long/link/target").await?;
    /// let data = ar.into_inner().await?;
    /// #
    /// # Ok(()) }) }
    /// ```
    pub async fn append_hard_link<N, T>(&mut self, path: N, target: T) -> io::Result<()>
    where
        N: TryInto<Name>,
        N::Error: fmt::Display,
        T: TryInto<Name>,
        T::Error: fmt::Display,
    {
        let path = into_name(path)?;
        let target = into_name(target)?;
        let mut header = Header::new_ustar();
        header.set_mode(DEFAULT_FILE_MODE);
        header.set_entry_type(EntryType::Link)?;
        header.set_size(0);
        let mut pax_extensions = Vec::new();
        prepare_header_path(&mut header, &path, &mut pax_extensions)?;
        prepare_header_hard_link(&mut header, &target, &mut pax_extensions)?;
        prepare_header_numeric_fields(&mut header, &mut pax_extensions)?;
        validate_header_for_write(&header)?;
        append_pax_extensions(self.get_mut(), &pax_extensions).await?;
        header.set_cksum();
        append_entry(self.get_mut(), &header, &[][..]).await
    }

    /// Adds a new symbolic-link entry to this archive with the specified name and target.
    ///
    /// Valid non-ASCII or overlong values are written using PAX `path` or
    /// `linkpath` records. The entry type is set to [`EntryType::Symlink`] and
    /// the size is set to zero.
    pub async fn append_symlink<N, T>(&mut self, path: N, target: T) -> io::Result<()>
    where
        N: TryInto<Name>,
        N::Error: fmt::Display,
        T: TryInto<LinkTarget>,
        T::Error: fmt::Display,
    {
        let path = into_name(path)?;
        let target = into_link_target(target)?;
        let mut header = Header::new_ustar();
        header.set_mode(DEFAULT_SYMLINK_MODE);
        header.set_entry_type(EntryType::Symlink)?;
        header.set_size(0);
        let mut pax_extensions = Vec::new();
        prepare_header_path(&mut header, &path, &mut pax_extensions)?;
        prepare_header_symlink_target(&mut header, &target, &mut pax_extensions)?;
        prepare_header_numeric_fields(&mut header, &mut pax_extensions)?;
        validate_header_for_write(&header)?;
        append_pax_extensions(self.get_mut(), &pax_extensions).await?;
        header.set_cksum();
        append_entry(self.get_mut(), &header, &[][..]).await
    }

    /// Adds a generated directory entry with conventional default permissions.
    pub async fn append_directory<N>(&mut self, path: N) -> io::Result<()>
    where
        N: TryInto<Name>,
        N::Error: fmt::Display,
    {
        self.append_directory_with_metadata(path, EntryMetadata::new())
            .await
    }

    /// Adds a generated directory entry with explicitly supplied metadata.
    pub async fn append_directory_with_metadata<N>(
        &mut self,
        path: N,
        metadata: EntryMetadata,
    ) -> io::Result<()>
    where
        N: TryInto<Name>,
        N::Error: fmt::Display,
    {
        let path = into_name(path)?;
        let mut header = Header::new_ustar();
        metadata.apply(&mut header, DEFAULT_DIRECTORY_MODE);
        header.set_entry_type(EntryType::Directory)?;
        header.set_size(0);
        let mut pax_extensions = Vec::new();
        prepare_header_path(&mut header, &path, &mut pax_extensions)?;
        prepare_header_numeric_fields(&mut header, &mut pax_extensions)?;
        validate_header_for_write(&header)?;
        append_pax_extensions(self.get_mut(), &pax_extensions).await?;
        header.set_cksum();
        append_entry(self.get_mut(), &header, &[][..]).await
    }

    /// Adds a file on the local filesystem to this archive.
    ///
    /// This function will open the file specified by `path` and insert the file
    /// into the archive with the appropriate metadata set, returning any I/O
    /// error which occurs while writing. The path name for the file inside of
    /// this archive will be the same as `path`, and it is required that the
    /// path is a relative path.
    ///
    /// Note that this will not attempt to seek the archive to a valid position,
    /// so if the archive is in the middle of a read or some other similar
    /// operation then this may corrupt the archive.
    ///
    /// Also note that after all files have been written to an archive the
    /// `finish` function needs to be called to finish writing the archive.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> { tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// #
    /// use tokio_tar::Builder;
    ///
    /// let mut ar = Builder::new(Vec::new());
    ///
    /// ar.append_path("foo/bar.txt").await?;
    /// #
    /// # Ok(()) }) }
    /// ```
    pub async fn append_path<P: AsRef<Path>>(&mut self, path: P) -> io::Result<()> {
        let mode = self.mode;
        let follow = self.follow;
        let name = Name::try_from(path.as_ref())?;
        append_path_with_name(self.get_mut(), path.as_ref(), &name, mode, follow).await?;
        Ok(())
    }

    /// Adds a file on the local filesystem to this archive under another name.
    ///
    /// This function will open the file specified by `path` and insert the file
    /// into the archive as `name` with appropriate metadata set, returning any
    /// I/O error which occurs while writing. The path name for the file inside
    /// of this archive will be `name` is required to be a relative path.
    ///
    /// Note that this will not attempt to seek the archive to a valid position,
    /// so if the archive is in the middle of a read or some other similar
    /// operation then this may corrupt the archive.
    ///
    /// Also note that after all files have been written to an archive the
    /// `finish` function needs to be called to finish writing the archive.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> { tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// #
    /// use tokio_tar::Builder;
    ///
    /// let mut ar = Builder::new(Vec::new());
    ///
    /// // Insert the local file "foo/bar.txt" in the archive but with the name
    /// // "bar/foo.txt".
    /// ar.append_path_with_name("foo/bar.txt", "bar/foo.txt").await?;
    /// #
    /// # Ok(()) }) }
    /// ```
    pub async fn append_path_with_name<P, N>(&mut self, path: P, name: N) -> io::Result<()>
    where
        P: AsRef<Path>,
        N: TryInto<Name>,
        N::Error: fmt::Display,
    {
        let mode = self.mode;
        let follow = self.follow;
        let name = into_name(name)?;
        append_path_with_name(self.get_mut(), path.as_ref(), &name, mode, follow).await?;
        Ok(())
    }

    /// Adds a file to this archive with the given path as the name of the file
    /// in the archive.
    ///
    /// This will use the metadata of `file` to populate a `Header`, and it will
    /// then append the file to the archive with the name `path`.
    ///
    /// Note that this will not attempt to seek the archive to a valid position,
    /// so if the archive is in the middle of a read or some other similar
    /// operation then this may corrupt the archive.
    ///
    /// Also note that after all files have been written to an archive the
    /// `finish` function needs to be called to finish writing the archive.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> { tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// #
    /// use tokio::fs::File;
    /// use tokio_tar::Builder;
    ///
    /// let mut ar = Builder::new(Vec::new());
    ///
    /// // Open the file at one location, but insert it into the archive with a
    /// // different name.
    /// let mut f = File::open("foo/bar/baz.txt").await?;
    /// ar.append_file("bar/baz.txt", &mut f).await?;
    /// #
    /// # Ok(()) }) }
    /// ```
    pub async fn append_file<N>(&mut self, path: N, file: &mut fs::File) -> io::Result<()>
    where
        N: TryInto<Name>,
        N::Error: fmt::Display,
    {
        let mode = self.mode;
        let path = into_name(path)?;
        append_file(self.get_mut(), &path, file, mode).await?;
        Ok(())
    }

    /// Adds a directory to this archive with the given path as the name of the
    /// directory in the archive.
    ///
    /// This will use `stat` to populate a `Header`, and it will then append the
    /// directory to the archive with the name `path`.
    ///
    /// Note that this will not attempt to seek the archive to a valid position,
    /// so if the archive is in the middle of a read or some other similar
    /// operation then this may corrupt the archive.
    ///
    /// Also note that after all files have been written to an archive the
    /// `finish` function needs to be called to finish writing the archive.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> { tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// #
    /// use tokio::fs;
    /// use tokio_tar::Builder;
    ///
    /// let mut ar = Builder::new(Vec::new());
    ///
    /// // Use the directory at one location, but insert it into the archive
    /// // with a different name.
    /// ar.append_dir("bardir", ".").await?;
    /// #
    /// # Ok(()) }) }
    /// ```
    pub async fn append_dir<N, P>(&mut self, path: N, src_path: P) -> io::Result<()>
    where
        N: TryInto<Name>,
        N::Error: fmt::Display,
        P: AsRef<Path>,
    {
        let mode = self.mode;
        let path = into_name(path)?;
        append_dir(self.get_mut(), &path, src_path.as_ref(), mode).await?;
        Ok(())
    }

    /// Adds a directory and all of its contents (recursively) to this archive
    /// with the given path as the name of the directory in the archive.
    ///
    /// Note that this will not attempt to seek the archive to a valid position,
    /// so if the archive is in the middle of a read or some other similar
    /// operation then this may corrupt the archive.
    ///
    /// Also note that after all files have been written to an archive the
    /// `finish` function needs to be called to finish writing the archive.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> { tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// #
    /// use tokio::fs;
    /// use tokio_tar::Builder;
    ///
    /// let mut ar = Builder::new(Vec::new());
    ///
    /// let td = tempfile::tempdir()?;
    ///
    /// // Use the directory at one location, but insert it into the archive
    /// // with a different name.
    /// ar.append_dir_all("bardir", td.path()).await?;
    /// #
    /// # Ok(()) }) }
    /// ```
    pub async fn append_dir_all<N, P>(&mut self, path: N, src_path: P) -> io::Result<()>
    where
        N: TryInto<Name>,
        N::Error: fmt::Display,
        P: AsRef<Path>,
    {
        let mode = self.mode;
        let follow = self.follow;
        let path = into_name(path)?;
        append_dir_all(self.get_mut(), Some(&path), src_path.as_ref(), mode, follow).await?;
        Ok(())
    }

    /// Adds a directory's contents recursively at the root of this archive.
    ///
    /// Unlike [`Builder::append_dir_all`], this method does not add an entry
    /// for `src_path` itself and derives validated archive names for its
    /// descendants.
    pub async fn append_dir_all_at_root<P: AsRef<Path>>(&mut self, src_path: P) -> io::Result<()> {
        let mode = self.mode;
        let follow = self.follow;
        append_dir_all(self.get_mut(), None, src_path.as_ref(), mode, follow).await?;
        Ok(())
    }

    /// Finish writing this archive, emitting the termination sections.
    ///
    /// This function should only be called when the archive has been written
    /// entirely and if an I/O error happens the underlying object still needs
    /// to be acquired.
    ///
    /// In most situations the `into_inner` method should be preferred.
    pub async fn finish(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.get_mut().write_all(&[0; 1024]).await?;
        Ok(())
    }
}

async fn append_entry<Dst: Write + Unpin + ?Sized, Data: Read + Unpin>(
    mut dst: &mut Dst,
    header: &Header,
    mut data: Data,
) -> io::Result<()> {
    dst.write_all(header.as_bytes()).await?;
    let len = io::copy(&mut data, &mut dst).await?;

    // Pad with zeros if necessary.
    let buf = [0; 512];
    let remaining = 512 - (len % 512);
    if remaining < 512 {
        dst.write_all(&buf[..remaining as usize]).await?;
    }

    Ok(())
}

const PAX_HEADER_PATH: &str = "PaxHeaders/pax-entry";
const PAX_PAYLOAD_PATH: &str = "pax-entry";
// A PAX-compliant reader ignores these canonical ASCII fallbacks when a
// `linkpath` extension is present.
const PAX_HARD_LINK_FALLBACK: &[u8] = b"pax-hard-link";
const PAX_SYM_LINK_FALLBACK: &[u8] = b"pax-sym-link";

struct PaxExtension {
    key: &'static [u8],
    value: Vec<u8>,
}

fn validate_header_for_write(header: &Header) -> io::Result<()> {
    validate_header_format_for_write(header)?;
    if header_has_base256_numeric_fields(header) {
        return Err(other(
            "cannot append a header with base-256 numeric fields to a USTAR/PAX archive",
        ));
    }
    Ok(())
}

fn validate_ustar_header(header: &Header) -> io::Result<()> {
    if header.as_ustar().is_none() {
        return Err(other("cannot append a non-USTAR header"));
    }
    Ok(())
}

fn validate_header_format_for_write(header: &Header) -> io::Result<()> {
    validate_ustar_header(header)?;

    let entry_type = header.entry_type();
    if !entry_type.is_ustar_or_pax_writer() {
        if entry_type.is_gnu_longname()
            || entry_type.is_gnu_longlink()
            || entry_type.is_gnu_sparse()
        {
            return Err(other("cannot append a GNU extension header"));
        }
        return Err(other("cannot append an unknown extension header"));
    }

    Ok(())
}

fn header_has_base256_numeric_fields(header: &Header) -> bool {
    fn is_base256(field: &[u8]) -> bool {
        field.first().is_some_and(|byte| byte & 0x80 != 0)
    }

    let old = header.as_old();
    is_base256(&old.mode)
        || is_base256(&old.uid)
        || is_base256(&old.gid)
        || is_base256(&old.size)
        || is_base256(&old.mtime)
        || header
            .as_ustar()
            .is_some_and(|ustar| is_base256(&ustar.dev_major) || is_base256(&ustar.dev_minor))
}

async fn append_pax_extensions<Dst: Write + Unpin + ?Sized>(
    dst: &mut Dst,
    extensions: &[PaxExtension],
) -> io::Result<()> {
    if extensions.is_empty() {
        return Ok(());
    }

    let mut data = Vec::new();
    for extension in extensions {
        append_pax_record(&mut data, extension.key, &extension.value)?;
    }

    let mut header = Header::new_ustar();
    let path = Name::try_from(PAX_HEADER_PATH)?;
    header.set_path(&path)?;
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(data.len() as u64);
    header.set_entry_type_unchecked(EntryType::XHeader);
    validate_header_for_write(&header)?;
    header.set_cksum();

    append_entry(dst, &header, &data[..]).await
}

fn append_pax_record(dst: &mut Vec<u8>, key: &[u8], value: &[u8]) -> io::Result<()> {
    let record_content_len = key
        .len()
        .checked_add(value.len())
        .and_then(|len| len.checked_add(3))
        .ok_or_else(|| other("pax extension is too long"))?;
    let mut record_len = record_content_len
        .checked_add(1)
        .ok_or_else(|| other("pax extension is too long"))?;

    loop {
        let next_len = record_content_len
            .checked_add(record_len.to_string().len())
            .ok_or_else(|| other("pax extension is too long"))?;
        if next_len == record_len {
            break;
        }
        record_len = next_len;
    }

    dst.extend_from_slice(record_len.to_string().as_bytes());
    dst.push(b' ');
    dst.extend_from_slice(key);
    dst.push(b'=');
    dst.extend_from_slice(value);
    dst.push(b'\n');
    Ok(())
}

fn push_pax_extension(extensions: &mut Vec<PaxExtension>, key: &'static [u8], value: Vec<u8>) {
    extensions.push(PaxExtension { key, value });
}

fn push_pax_numeric_extension(extensions: &mut Vec<PaxExtension>, key: &'static [u8], value: u64) {
    push_pax_extension(extensions, key, value.to_string().into_bytes());
}

fn prepare_header_path(
    header: &mut Header,
    path: &Name,
    pax_extensions: &mut Vec<PaxExtension>,
) -> io::Result<()> {
    clear_header_path(header)?;
    if header.set_path(path).is_err() {
        clear_header_path(header)?;
        let fallback = Name::try_from(PAX_PAYLOAD_PATH)?;
        header.set_path(&fallback)?;
        push_pax_extension(pax_extensions, b"path", path.as_bytes().to_vec());
    }
    Ok(())
}

fn clear_header_path(header: &mut Header) -> io::Result<()> {
    let ustar = header
        .as_ustar_mut()
        .ok_or_else(|| other("cannot append a non-USTAR header"))?;
    ustar.name.fill(0);
    ustar.prefix.fill(0);
    Ok(())
}

fn prepare_header_hard_link(
    header: &mut Header,
    link_name: &Name,
    pax_extensions: &mut Vec<PaxExtension>,
) -> io::Result<()> {
    header.as_old_mut().linkname.fill(0);
    if header.set_hard_link_name(link_name).is_err() {
        let linkname = &mut header.as_old_mut().linkname;
        linkname.fill(0);
        linkname[..PAX_HARD_LINK_FALLBACK.len()].copy_from_slice(PAX_HARD_LINK_FALLBACK);
        push_pax_extension(pax_extensions, b"linkpath", link_name.as_bytes().to_vec());
    }
    Ok(())
}

fn prepare_header_symlink_target(
    header: &mut Header,
    target: &LinkTarget,
    pax_extensions: &mut Vec<PaxExtension>,
) -> io::Result<()> {
    header.as_old_mut().linkname.fill(0);
    if header.set_symlink_target(target).is_err() {
        let linkname = &mut header.as_old_mut().linkname;
        linkname.fill(0);
        linkname[..PAX_SYM_LINK_FALLBACK.len()].copy_from_slice(PAX_SYM_LINK_FALLBACK);
        push_pax_extension(pax_extensions, b"linkpath", target.as_bytes().to_vec());
    }
    Ok(())
}

fn prepare_header_numeric_fields(
    header: &mut Header,
    pax_extensions: &mut Vec<PaxExtension>,
) -> io::Result<()> {
    fn is_base256(field: &[u8]) -> bool {
        field.first().is_some_and(|byte| byte & 0x80 != 0)
    }

    if is_base256(&header.as_old().size) {
        push_pax_numeric_extension(pax_extensions, b"size", header.entry_size()?);
        header.set_size(0);
    }

    if is_base256(&header.as_old().uid) {
        push_pax_numeric_extension(pax_extensions, b"uid", header.uid()?);
        header.set_uid(0);
    }

    if is_base256(&header.as_old().gid) {
        push_pax_numeric_extension(pax_extensions, b"gid", header.gid()?);
        header.set_gid(0);
    }

    if is_base256(&header.as_old().mtime) {
        push_pax_numeric_extension(pax_extensions, b"mtime", header.mtime()?);
        header.set_mtime(0);
    }

    Ok(())
}

async fn append_path_with_name<Dst: Write + Unpin + ?Sized>(
    dst: &mut Dst,
    path: &Path,
    name: &Name,
    mode: HeaderMode,
    follow: bool,
) -> io::Result<()> {
    let stat = if follow {
        fs::metadata(path).await.map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("{} when getting metadata for {}", err, path.display()),
            )
        })?
    } else {
        fs::symlink_metadata(path).await.map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("{} when getting metadata for {}", err, path.display()),
            )
        })?
    };
    if stat.is_file() {
        append_fs(
            dst,
            name,
            &stat,
            &mut fs::File::open(path).await?,
            mode,
            None,
        )
        .await?;
        Ok(())
    } else if stat.is_dir() {
        append_fs(dst, name, &stat, &mut io::empty(), mode, None).await?;
        Ok(())
    } else if stat.file_type().is_symlink() {
        let link_name = LinkTarget::try_from(fs::read_link(path).await?)?;
        append_fs(dst, name, &stat, &mut io::empty(), mode, Some(&link_name)).await?;
        Ok(())
    } else {
        #[cfg(unix)]
        {
            append_special(dst, name, &stat, mode).await
        }
        #[cfg(not(unix))]
        {
            Err(other(&format!("{} has unknown file type", path.display())))
        }
    }
}

#[cfg(unix)]
async fn append_special<Dst: Write + Unpin + ?Sized>(
    dst: &mut Dst,
    path: &Name,
    stat: &Metadata,
    mode: HeaderMode,
) -> io::Result<()> {
    use ::std::os::unix::fs::{FileTypeExt, MetadataExt};

    let file_type = stat.file_type();
    let entry_type;
    if file_type.is_socket() {
        // sockets can't be archived
        return Err(other(&format!("{}: socket can not be archived", path)));
    } else if file_type.is_fifo() {
        entry_type = EntryType::Fifo;
    } else if file_type.is_char_device() {
        entry_type = EntryType::Char;
    } else if file_type.is_block_device() {
        entry_type = EntryType::Block;
    } else {
        return Err(other(&format!("{} has unknown file type", path)));
    }

    let mut header = Header::new_ustar();
    header.set_metadata_in_mode(stat, mode);
    let mut pax_extensions = Vec::new();
    prepare_header_path(&mut header, path, &mut pax_extensions)?;

    header.set_entry_type(entry_type)?;
    let dev_id = stat.rdev();
    let dev_major = ((dev_id >> 32) & 0xffff_f000) | ((dev_id >> 8) & 0x0000_0fff);
    let dev_minor = ((dev_id >> 12) & 0xffff_ff00) | ((dev_id) & 0x0000_00ff);
    header.set_device_major(dev_major as u32)?;
    header.set_device_minor(dev_minor as u32)?;

    prepare_header_numeric_fields(&mut header, &mut pax_extensions)?;
    validate_header_for_write(&header)?;
    append_pax_extensions(dst, &pax_extensions).await?;
    header.set_cksum();
    dst.write_all(header.as_bytes()).await?;

    Ok(())
}

async fn append_file<Dst: Write + Unpin + ?Sized>(
    dst: &mut Dst,
    path: &Name,
    file: &mut fs::File,
    mode: HeaderMode,
) -> io::Result<()> {
    let stat = file.metadata().await?;
    append_fs(dst, path, &stat, file, mode, None).await?;
    Ok(())
}

async fn append_dir<Dst: Write + Unpin + ?Sized>(
    dst: &mut Dst,
    path: &Name,
    src_path: &Path,
    mode: HeaderMode,
) -> io::Result<()> {
    let stat = fs::metadata(src_path).await?;
    append_fs(dst, path, &stat, &mut io::empty(), mode, None).await?;
    Ok(())
}

async fn append_fs<Dst: Write + Unpin + ?Sized, R: Read + Unpin + ?Sized>(
    dst: &mut Dst,
    path: &Name,
    meta: &Metadata,
    read: &mut R,
    mode: HeaderMode,
    link_name: Option<&LinkTarget>,
) -> io::Result<()> {
    let mut header = Header::new_ustar();
    let mut pax_extensions = Vec::new();

    prepare_header_path(&mut header, path, &mut pax_extensions)?;
    header.set_metadata_in_mode(meta, mode);
    if let Some(link_name) = link_name {
        prepare_header_symlink_target(&mut header, link_name, &mut pax_extensions)?;
    }
    prepare_header_numeric_fields(&mut header, &mut pax_extensions)?;
    validate_header_for_write(&header)?;
    append_pax_extensions(dst, &pax_extensions).await?;
    header.set_cksum();
    append_entry(dst, &header, read).await?;

    Ok(())
}

async fn append_dir_all<Dst: Write + Unpin + ?Sized>(
    dst: &mut Dst,
    path: Option<&Name>,
    src_path: &Path,
    mode: HeaderMode,
    follow: bool,
) -> io::Result<()> {
    let mut stack = vec![(src_path.to_path_buf(), true, false)];
    while let Some((src, is_dir, is_symlink)) = stack.pop() {
        let relative = src.strip_prefix(src_path).unwrap();
        let dest = match (path, relative.as_os_str().is_empty()) {
            (Some(prefix), true) => Some(prefix.clone()),
            (Some(prefix), false) => Some(Name::try_from(prefix.as_path().join(relative))?),
            (None, true) => None,
            (None, false) => Some(Name::try_from(relative)?),
        };

        // In case of a symlink pointing to a directory, is_dir is false, but src.is_dir() will return true
        if is_dir || (is_symlink && follow && src.is_dir()) {
            let mut entries = fs::read_dir(&src).await?;
            while let Some(entry) = entries.next_entry().await.transpose() {
                let entry = entry?;
                let file_type = entry.file_type().await?;
                stack.push((entry.path(), file_type.is_dir(), file_type.is_symlink()));
            }
            if let Some(dest) = &dest {
                append_dir(dst, dest, &src, mode).await?;
            }
        } else if !follow && is_symlink {
            let stat = fs::symlink_metadata(&src).await?;
            let link_name = LinkTarget::try_from(fs::read_link(&src).await?)?;
            append_fs(
                dst,
                dest.as_ref().expect("recursive child has an archive name"),
                &stat,
                &mut io::empty(),
                mode,
                Some(&link_name),
            )
            .await?;
        } else {
            let dest = dest.as_ref().expect("recursive child has an archive name");
            #[cfg(unix)]
            {
                let stat = fs::metadata(&src).await?;
                if !stat.is_file() {
                    append_special(dst, dest, &stat, mode).await?;
                    continue;
                }
            }
            append_file(dst, dest, &mut fs::File::open(src).await?, mode).await?;
        }
    }
    Ok(())
}

impl<W: Write + Unpin + Send> Drop for Builder<W> {
    fn drop(&mut self) {
        // TODO: proper async cancellation
        if !self.finished {
            if let Some(cancellation) = self.cancellation.take() {
                cancellation.send(self.obj.take().unwrap()).ok();
            }
        }
    }
}
