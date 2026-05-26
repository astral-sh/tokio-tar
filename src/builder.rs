use crate::{
    header::HeaderMode,
    name::{archive_name as checked, link_target},
    other, EntryType, Header,
};
use std::{fs::Metadata, path::Path};
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
    /// A PAX `path` extension is emitted when the validated name is too
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
    pub async fn append_data<N: AsRef<Path>, R>(
        &mut self,
        path: N,
        size: u64,
        data: R,
    ) -> io::Result<()>
    where
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
        N: AsRef<Path>,
        R: Read + Unpin,
    {
        let entry = generated_entry(
            &checked(path.as_ref())?,
            size,
            EntryType::Regular,
            metadata,
            DEFAULT_FILE_MODE,
        )?;
        append_generated(self.get_mut(), entry, data).await
    }

    /// Adds a generated hard-link entry, using PAX for an overlong or non-ASCII value.
    pub async fn append_hard_link<N: AsRef<Path>, T: AsRef<Path>>(
        &mut self,
        path: N,
        target: T,
    ) -> io::Result<()> {
        let mut entry = generated_entry(
            &checked(path.as_ref())?,
            0,
            EntryType::Link,
            EntryMetadata::new(),
            DEFAULT_FILE_MODE,
        )?;
        let target = checked(target.as_ref())?;
        prepare_header_link(&mut entry.0, &mut entry.1, &target, PAX_HARD_LINK_FALLBACK);
        append_generated(self.get_mut(), entry, &[][..]).await
    }

    /// Adds a new symbolic-link entry to this archive with the specified name and target.
    ///
    /// Valid non-ASCII or overlong values are written using PAX `path` or
    /// `linkpath` records. The entry type is set to [`EntryType::Symlink`] and
    /// the size is set to zero.
    pub async fn append_symlink<N: AsRef<Path>, T: AsRef<Path>>(
        &mut self,
        path: N,
        target: T,
    ) -> io::Result<()> {
        let mut entry = generated_entry(
            &checked(path.as_ref())?,
            0,
            EntryType::Symlink,
            EntryMetadata::new(),
            DEFAULT_SYMLINK_MODE,
        )?;
        let target = link_target(target.as_ref())?;
        prepare_header_link(&mut entry.0, &mut entry.1, &target, PAX_SYM_LINK_FALLBACK);
        append_generated(self.get_mut(), entry, &[][..]).await
    }

    /// Adds a generated directory entry with conventional default permissions.
    pub async fn append_directory<N: AsRef<Path>>(&mut self, path: N) -> io::Result<()> {
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
        N: AsRef<Path>,
    {
        let entry = generated_entry(
            &checked(path.as_ref())?,
            0,
            EntryType::Directory,
            metadata,
            DEFAULT_DIRECTORY_MODE,
        )?;
        append_generated(self.get_mut(), entry, &[][..]).await
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
        let name = checked(path.as_ref())?;
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
        N: AsRef<Path>,
    {
        let mode = self.mode;
        let follow = self.follow;
        let name = checked(name.as_ref())?;
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
    pub async fn append_file<N: AsRef<Path>>(
        &mut self,
        path: N,
        file: &mut fs::File,
    ) -> io::Result<()> {
        let mode = self.mode;
        let path = checked(path.as_ref())?;
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
        N: AsRef<Path>,
        P: AsRef<Path>,
    {
        let mode = self.mode;
        let path = checked(path.as_ref())?;
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
        N: AsRef<Path>,
        P: AsRef<Path>,
    {
        let mode = self.mode;
        let follow = self.follow;
        let path = (!path.as_ref().as_os_str().is_empty())
            .then(|| checked(path.as_ref()))
            .transpose()?;
        append_dir_all(
            self.get_mut(),
            path.as_deref(),
            src_path.as_ref(),
            mode,
            follow,
        )
        .await?;
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

type PaxExtensions = Vec<(&'static str, String)>;
type GeneratedEntry = (Header, PaxExtensions);

fn generated_entry(
    path: &str,
    size: u64,
    entry_type: EntryType,
    metadata: EntryMetadata,
    default_mode: u32,
) -> io::Result<GeneratedEntry> {
    let mut header = Header::new_ustar();
    metadata.apply(&mut header, default_mode);
    header.set_size(size);
    header.set_entry_type(entry_type);
    let mut extensions = Vec::new();
    prepare_header_path(&mut header, path, &mut extensions);
    prepare_header_numeric_fields(&mut header, &mut extensions)?;
    Ok((header, extensions))
}

async fn append_generated<Dst: Write + Unpin + ?Sized, Data: Read + Unpin>(
    dst: &mut Dst,
    (mut header, extensions): GeneratedEntry,
    data: Data,
) -> io::Result<()> {
    append_pax_extensions(dst, &extensions).await?;
    header.set_cksum();
    append_entry(dst, &header, data).await
}

async fn append_pax_extensions<Dst: Write + Unpin + ?Sized>(
    dst: &mut Dst,
    extensions: &PaxExtensions,
) -> io::Result<()> {
    if extensions.is_empty() {
        return Ok(());
    }

    let mut data = Vec::new();
    for (key, value) in extensions {
        append_pax_record(&mut data, key, value);
    }

    let mut header = Header::new_ustar();
    header.set_path(PAX_HEADER_PATH)?;
    EntryMetadata::new().apply(&mut header, DEFAULT_FILE_MODE);
    header.set_size(data.len() as u64);
    header.set_entry_type(EntryType::XHeader);
    header.set_cksum();

    append_entry(dst, &header, &data[..]).await
}

fn append_pax_record(dst: &mut Vec<u8>, key: &str, value: &str) {
    let record_content_len = key.len() + value.len() + 3;
    let mut record_len = record_content_len + 1;
    loop {
        let next_len = record_content_len + record_len.to_string().len();
        if next_len == record_len {
            break;
        }
        record_len = next_len;
    }

    dst.extend_from_slice(record_len.to_string().as_bytes());
    dst.push(b' ');
    dst.extend_from_slice(key.as_bytes());
    dst.push(b'=');
    dst.extend_from_slice(value.as_bytes());
    dst.push(b'\n');
}

fn prepare_header_path(header: &mut Header, path: &str, pax_extensions: &mut PaxExtensions) {
    if !path.is_ascii() || header.set_path(path).is_err() {
        let ustar = header.as_ustar_mut().expect("writer uses USTAR headers");
        ustar.name.fill(0);
        ustar.prefix.fill(0);
        header.set_path(PAX_PAYLOAD_PATH).unwrap();
        pax_extensions.push(("path", path.to_owned()));
    }
}

fn prepare_header_link(
    header: &mut Header,
    pax_extensions: &mut PaxExtensions,
    target: &str,
    fallback: &[u8],
) {
    header.as_old_mut().linkname.fill(0);
    if !target.is_ascii() || header.set_link_name(target).is_err() {
        let linkname = &mut header.as_old_mut().linkname;
        linkname.fill(0);
        linkname[..fallback.len()].copy_from_slice(fallback);
        pax_extensions.push(("linkpath", target.to_owned()));
    }
}

fn prepare_header_numeric_fields(
    header: &mut Header,
    pax_extensions: &mut PaxExtensions,
) -> io::Result<()> {
    fn is_base256(field: &[u8]) -> bool {
        field.first().is_some_and(|byte| byte & 0x80 != 0)
    }

    macro_rules! pax_field {
        ($field:ident, $get:ident, $set:ident) => {
            if is_base256(&header.as_old().$field) {
                pax_extensions.push((stringify!($field), header.$get()?.to_string()));
                header.$set(0);
            }
        };
    }
    pax_field!(size, entry_size, set_size);
    pax_field!(uid, uid, set_uid);
    pax_field!(gid, gid, set_gid);
    pax_field!(mtime, mtime, set_mtime);

    Ok(())
}

async fn append_path_with_name<Dst: Write + Unpin + ?Sized>(
    dst: &mut Dst,
    path: &Path,
    name: &str,
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
        let link_name = link_target(&fs::read_link(path).await?)?;
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
    path: &str,
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
    prepare_header_path(&mut header, path, &mut pax_extensions);

    header.set_entry_type(entry_type);
    let dev_id = stat.rdev();
    let dev_major = ((dev_id >> 32) & 0xffff_f000) | ((dev_id >> 8) & 0x0000_0fff);
    let dev_minor = ((dev_id >> 12) & 0xffff_ff00) | ((dev_id) & 0x0000_00ff);
    header.set_device_major(dev_major as u32)?;
    header.set_device_minor(dev_minor as u32)?;

    prepare_header_numeric_fields(&mut header, &mut pax_extensions)?;
    append_generated(dst, (header, pax_extensions), &[][..]).await
}

async fn append_file<Dst: Write + Unpin + ?Sized>(
    dst: &mut Dst,
    path: &str,
    file: &mut fs::File,
    mode: HeaderMode,
) -> io::Result<()> {
    let stat = file.metadata().await?;
    append_fs(dst, path, &stat, file, mode, None).await?;
    Ok(())
}

async fn append_dir<Dst: Write + Unpin + ?Sized>(
    dst: &mut Dst,
    path: &str,
    src_path: &Path,
    mode: HeaderMode,
) -> io::Result<()> {
    let stat = fs::metadata(src_path).await?;
    append_fs(dst, path, &stat, &mut io::empty(), mode, None).await?;
    Ok(())
}

async fn append_fs<Dst: Write + Unpin + ?Sized, R: Read + Unpin + ?Sized>(
    dst: &mut Dst,
    path: &str,
    meta: &Metadata,
    read: &mut R,
    mode: HeaderMode,
    link_name: Option<&str>,
) -> io::Result<()> {
    let mut header = Header::new_ustar();
    let mut pax_extensions = Vec::new();

    prepare_header_path(&mut header, path, &mut pax_extensions);
    header.set_metadata_in_mode(meta, mode);
    if let Some(link_name) = link_name {
        prepare_header_link(
            &mut header,
            &mut pax_extensions,
            link_name,
            PAX_SYM_LINK_FALLBACK,
        );
    }
    prepare_header_numeric_fields(&mut header, &mut pax_extensions)?;
    append_generated(dst, (header, pax_extensions), read).await
}

async fn append_dir_all<Dst: Write + Unpin + ?Sized>(
    dst: &mut Dst,
    path: Option<&str>,
    src_path: &Path,
    mode: HeaderMode,
    follow: bool,
) -> io::Result<()> {
    let mut stack = vec![(src_path.to_path_buf(), true, false)];
    while let Some((src, is_dir, is_symlink)) = stack.pop() {
        let relative = src.strip_prefix(src_path).unwrap();
        let dest = match (path, relative.as_os_str().is_empty()) {
            (Some(prefix), true) => Some(prefix.to_owned()),
            (Some(prefix), false) => Some(checked(&Path::new(prefix).join(relative))?),
            (None, true) => None,
            (None, false) => Some(checked(relative)?),
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
            let link_name = link_target(&fs::read_link(&src).await?)?;
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
