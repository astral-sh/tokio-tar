extern crate tokio_tar as async_tar;

extern crate tempfile;

#[cfg_attr(windows, allow(unused_imports))]
use tokio::{fs, fs::File, io::AsyncReadExt};
use tokio_stream::*;

use async_tar::ArchiveBuilder;
use tempfile::Builder;

macro_rules! t {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => panic!("{} returned {}", stringify!($e), e),
        }
    };
}

fn raw_header(
    path: &[u8],
    target: Option<&[u8]>,
    typeflag: u8,
    size: u64,
    mode: u32,
) -> async_tar::UstarHeader {
    let mut header: async_tar::UstarHeader = unsafe { std::mem::zeroed() };
    header.name[..path.len()].copy_from_slice(path);
    if let Some(target) = target {
        header.linkname[..target.len()].copy_from_slice(target);
    }
    header.typeflag = [typeflag];
    header.magic = *b"ustar\0";
    header.version = *b"00";
    encode_octal(&mut header.mode, mode as u64);
    encode_octal(&mut header.uid, 0);
    encode_octal(&mut header.gid, 0);
    encode_octal(&mut header.size, size);
    encode_octal(&mut header.mtime, 0);
    let checksum = header
        .as_header()
        .as_bytes()
        .iter()
        .enumerate()
        .map(|(offset, byte)| {
            if (148..156).contains(&offset) {
                b' ' as u32
            } else {
                *byte as u32
            }
        })
        .sum::<u32>();
    encode_octal(&mut header.cksum, checksum as u64);
    header
}

fn raw_link_header(path: &[u8], target: &[u8]) -> async_tar::UstarHeader {
    raw_header(path, Some(target), b'1', 0, 0o644)
}

fn encode_octal(dst: &mut [u8], value: u64) {
    let octal = format!("{value:o}");
    let value = std::iter::once(b'\0').chain(octal.bytes().rev().chain(std::iter::repeat(b'0')));
    for (slot, value) in dst.iter_mut().rev().zip(value) {
        *slot = value;
    }
}

fn append_raw(bytes: &mut Vec<u8>, header: &async_tar::Header, data: &[u8]) {
    bytes.extend_from_slice(header.as_bytes());
    bytes.extend_from_slice(data);
    let padding = (512 - data.len() % 512) % 512;
    bytes.extend_from_slice(&vec![0; padding]);
}

#[tokio::test]
async fn absolute_symlink() {
    let mut ar = async_tar::Builder::new(Vec::new());
    t!(ar.append_symlink("foo", "/bar").await);

    let bytes = t!(ar.into_inner().await);
    let mut ar = async_tar::Archive::new(&bytes[..]);

    let td = t!(Builder::new().prefix("tar").tempdir());
    t!(ar.unpack(td.path()).await);

    t!(td.path().join("foo").symlink_metadata());

    let mut ar = async_tar::Archive::new(&bytes[..]);
    let mut entries = t!(ar.entries());
    let entry = t!(entries.next().await.unwrap());
    assert_eq!(&*t!(entry.link_name_bytes()).unwrap(), b"/bar");
}

#[tokio::test]
async fn absolute_hardlink() {
    let td = t!(Builder::new().prefix("tar").tempdir());
    let mut bytes = Vec::new();
    let file_header = raw_header(b"foo", None, b'0', 0, 0o644);
    append_raw(&mut bytes, file_header.as_header(), &[]);

    // This absolute path under tempdir will be created at unpack time
    let target = td.path().join("foo");
    let header = raw_link_header(b"bar", target.to_str().unwrap().as_bytes());
    append_raw(&mut bytes, header.as_header(), &[]);
    bytes.extend_from_slice(&[0; 1024]);

    let mut ar = async_tar::Archive::new(&bytes[..]);

    t!(ar.unpack(td.path()).await);
    t!(td.path().join("foo").metadata());
    t!(td.path().join("bar").metadata());
}

#[tokio::test]
async fn relative_hardlink() {
    let mut ar = async_tar::Builder::new(Vec::new());
    t!(ar.append_data("foo", 0, &[][..]).await);
    t!(ar.append_hard_link("bar", "foo").await);

    let bytes = t!(ar.into_inner().await);
    let mut ar = async_tar::Archive::new(&bytes[..]);

    let td = t!(Builder::new().prefix("tar").tempdir());
    t!(ar.unpack(td.path()).await);
    t!(td.path().join("foo").metadata());
    t!(td.path().join("bar").metadata());
}

#[tokio::test]
async fn absolute_link_deref_error() {
    let mut ar = async_tar::Builder::new(Vec::new());
    t!(ar.append_symlink("foo", "/").await);
    t!(ar.append_data("foo/bar", 0, &[][..]).await);

    let bytes = t!(ar.into_inner().await);
    let mut ar = async_tar::Archive::new(&bytes[..]);

    let td = t!(Builder::new().prefix("tar").tempdir());
    assert!(ar.unpack(td.path()).await.is_err());
    t!(td.path().join("foo").symlink_metadata());
    assert!(File::open(td.path().join("foo").join("bar")).await.is_err());
}

#[tokio::test]
async fn relative_link_deref_error() {
    let mut ar = async_tar::Builder::new(Vec::new());
    t!(ar.append_symlink("foo", "../../../../").await);
    t!(ar.append_data("foo/bar", 0, &[][..]).await);

    let bytes = t!(ar.into_inner().await);
    let mut ar = async_tar::Archive::new(&bytes[..]);

    let td = t!(Builder::new().prefix("tar").tempdir());
    assert!(ar.unpack(td.path()).await.is_err());
    t!(td.path().join("foo").symlink_metadata());
    assert!(File::open(td.path().join("foo").join("bar")).await.is_err());
}

#[tokio::test]
#[cfg(unix)]
async fn directory_permissions_are_cleared() {
    use ::std::os::unix::fs::PermissionsExt;

    let mut ar = async_tar::Builder::new(Vec::new());
    t!(ar
        .append_directory_with_metadata("foo", async_tar::EntryMetadata::new().mode(0o777))
        .await);

    let bytes = t!(ar.into_inner().await);
    let mut ar = async_tar::Archive::new(&bytes[..]);

    let td = t!(Builder::new().prefix("tar").tempdir());
    t!(ar.unpack(td.path()).await);
    let f = t!(File::open(td.path().join("foo")).await);
    let from_archive_md = t!(f.metadata().await);
    assert!(from_archive_md.is_dir());

    // To determine the default umask, create a fresh directory that gets it assigned by the OS.
    let manually_created = td.path().join("bar");
    t!(fs::create_dir(&manually_created).await);
    let f = t!(File::open(&manually_created).await);
    let loca_md = t!(f.metadata().await);

    assert_eq!(
        from_archive_md.permissions().mode(),
        loca_md.permissions().mode()
    );
}

#[tokio::test]
#[cfg(not(windows))] // dangling symlinks have weird permissions
async fn modify_link_just_created() {
    let mut ar = async_tar::Builder::new(Vec::new());
    t!(ar.append_symlink("foo", "bar").await);
    t!(ar.append_data("bar/foo", 0, &[][..]).await);
    t!(ar.append_data("foo/bar", 0, &[][..]).await);

    let bytes = t!(ar.into_inner().await);
    let mut ar = async_tar::Archive::new(&bytes[..]);

    let td = t!(Builder::new().prefix("tar").tempdir());
    t!(ar.unpack(td.path()).await);

    t!(File::open(td.path().join("bar/foo")).await);
    t!(File::open(td.path().join("bar/bar")).await);
    t!(File::open(td.path().join("foo/foo")).await);
    t!(File::open(td.path().join("foo/bar")).await);
}

#[tokio::test]
#[cfg(not(windows))] // dangling symlinks have weird permissions
async fn modify_outside_with_relative_symlink() {
    let mut ar = async_tar::Builder::new(Vec::new());
    t!(ar.append_symlink("symlink", "..").await);
    t!(ar.append_data("symlink/foo/bar", 0, &[][..]).await);

    let bytes = t!(ar.into_inner().await);
    let mut ar = async_tar::Archive::new(&bytes[..]);

    let td = t!(Builder::new().prefix("tar").tempdir());
    let tar_dir = td.path().join("tar");
    assert!(ar.unpack(tar_dir).await.is_err());
    assert!(!td.path().join("foo").exists());
}

#[tokio::test]
async fn parent_paths_error() {
    let mut ar = async_tar::Builder::new(Vec::new());
    t!(ar.append_symlink("foo", "..").await);
    t!(ar.append_data("foo/bar", 0, &[][..]).await);

    let bytes = t!(ar.into_inner().await);
    let mut ar = async_tar::Archive::new(&bytes[..]);

    let td = t!(Builder::new().prefix("tar").tempdir());
    assert!(ar.unpack(td.path()).await.is_err());
    t!(td.path().join("foo").symlink_metadata());
    assert!(File::open(td.path().join("foo").join("bar")).await.is_err());
}

#[tokio::test]
#[cfg(unix)]
async fn good_parent_paths_ok() {
    use std::path::PathBuf;
    let mut ar = async_tar::Builder::new(Vec::new());
    t!(ar
        .append_symlink(
            PathBuf::from("foo").join("bar"),
            PathBuf::from("..").join("bar")
        )
        .await);
    t!(ar.append_data("bar", 0, &[][..]).await);

    let bytes = t!(ar.into_inner().await);
    let mut ar = async_tar::Archive::new(&bytes[..]);

    let td = t!(Builder::new().prefix("tar").tempdir());
    t!(ar.unpack(td.path()).await);
    t!(td.path().join("foo").join("bar").read_link());
    let dst = t!(td.path().join("foo").join("bar").canonicalize());
    t!(File::open(dst).await);
}

#[tokio::test]
async fn modify_hard_link_just_created() {
    let mut bytes = Vec::new();
    let header = raw_link_header(b"foo", b"../test");
    append_raw(&mut bytes, header.as_header(), &[]);
    let mut ar = async_tar::Builder::new(bytes);

    t!(ar.append_data("foo", 1, &b"x"[..]).await);

    let bytes = t!(ar.into_inner().await);
    let mut ar = async_tar::Archive::new(&bytes[..]);

    let td = t!(Builder::new().prefix("tar").tempdir());

    let test = td.path().join("test");
    t!(File::create(&test).await);

    let dir = td.path().join("dir");
    assert!(ar.unpack(&dir).await.is_err());

    let mut contents = Vec::new();
    t!(t!(File::open(&test).await).read_to_end(&mut contents).await);
    assert_eq!(contents.len(), 0);
}

#[tokio::test]
async fn modify_symlink_just_created() {
    let mut ar = async_tar::Builder::new(Vec::new());
    t!(ar.append_symlink("foo", "../test").await);
    t!(ar.append_data("foo", 1, &b"x"[..]).await);

    let bytes = t!(ar.into_inner().await);
    let mut ar = async_tar::Archive::new(&bytes[..]);

    let td = t!(Builder::new().prefix("tar").tempdir());

    let test = td.path().join("test");
    t!(File::create(&test).await);

    let dir = td.path().join("dir");
    t!(ar.unpack(&dir).await);

    let mut contents = Vec::new();
    t!(t!(File::open(&test).await).read_to_end(&mut contents).await);
    assert_eq!(contents.len(), 0);
}

#[tokio::test]
async fn deny_absolute_symlink() {
    let mut ar = async_tar::Builder::new(Vec::new());
    t!(ar.append_symlink("foo", "/bar").await);

    let bytes = t!(ar.into_inner().await);

    let builder = ArchiveBuilder::new(&bytes[..]).set_allow_external_symlinks(false);
    let mut ar = builder.build();

    let td = t!(Builder::new().prefix("tar").tempdir());
    assert!(ar.unpack(td.path()).await.is_err());
}

#[tokio::test]
async fn deny_relative_link() {
    let mut ar = async_tar::Builder::new(Vec::new());
    t!(ar.append_symlink("foo", "../../../../").await);

    let bytes = t!(ar.into_inner().await);

    let builder = ArchiveBuilder::new(&bytes[..]).set_allow_external_symlinks(false);
    let mut ar = builder.build();

    let td = t!(Builder::new().prefix("tar").tempdir());
    assert!(ar.unpack(td.path()).await.is_err());
}

#[tokio::test]
#[cfg(unix)]
async fn accept_relative_link() {
    let mut ar = async_tar::Builder::new(Vec::new());
    t!(ar.append_symlink("foo/bar", "../baz").await);
    t!(ar.append_data("baz", 1, &b"x"[..]).await);

    let bytes = t!(ar.into_inner().await);

    let builder = ArchiveBuilder::new(&bytes[..]).set_allow_external_symlinks(false);
    let mut ar = builder.build();

    let td = t!(Builder::new().prefix("tar").tempdir());
    t!(ar.unpack(td.path()).await);
    t!(td.path().join("foo/bar").symlink_metadata());
    t!(File::open(td.path().join("foo").join("bar")).await);
}

#[tokio::test]
async fn malformed_pax_path_extension() {
    // Create a tar archive with a malformed PAX extension for path
    let mut ar_bytes = Vec::new();

    // First, create a PAX extension header with malformed content
    // Create malformed PAX extension data - the length field doesn't match the actual content length
    // Format is: "<length> <key>=<value>\n" where length includes itself
    let malformed_pax = b"99 path=test.txt\n"; // Claims to be 99 bytes but is only 17 bytes
    let pax_header = raw_header(
        b"PaxHeaders/file",
        None,
        b'x',
        malformed_pax.len() as u64,
        0o644,
    );

    // Manually write the archive
    ar_bytes.extend_from_slice(pax_header.as_header().as_bytes());
    ar_bytes.extend_from_slice(malformed_pax);
    // Pad to 512 byte boundary
    let padding = (512 - (malformed_pax.len() % 512)) % 512;
    ar_bytes.extend_from_slice(&vec![0u8; padding]);

    // Now add the actual file entry
    let header = raw_header(b"file", None, b'0', 4, 0o644);
    ar_bytes.extend_from_slice(header.as_header().as_bytes());
    ar_bytes.extend_from_slice(b"test");
    // Pad to 512 byte boundary
    ar_bytes.extend_from_slice(&vec![0u8; 508]);

    // Try to read the entries - malformed PAX data is now detected during iteration
    let mut ar = async_tar::Archive::new(&ar_bytes[..]);
    let mut entries = t!(ar.entries());

    // This should return an error because the PAX extension is malformed
    let result = entries.next().await.unwrap();
    assert!(
        result.is_err(),
        "Expected error for malformed PAX extension"
    );

    // Verify it's a PAX-related error
    match result {
        Err(e) => {
            let err_str = e.to_string();
            assert!(
                err_str.contains("malformed pax extension"),
                "Expected 'malformed pax extension' error, got: {}",
                err_str
            );
        }
        Ok(_) => panic!("Expected error but got Ok"),
    }
}

#[tokio::test]
#[cfg(unix)]
async fn symlink_dir_collision_does_not_chmod_external_dir() {
    use std::os::unix::fs::PermissionsExt;

    let td = t!(Builder::new().prefix("tar").tempdir());

    // Set up our target directory outside the extraction root with 0700.
    let target = td.path().join("target-dir");
    t!(fs::create_dir(&target).await);
    t!(fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).await);
    let before = t!(fs::metadata(&target).await).permissions().mode() & 0o7777;
    assert_eq!(before, 0o700);

    // Set up our extraction directory.
    let extract = td.path().join("extract");
    t!(fs::create_dir(&extract).await);

    let mut ar = async_tar::Builder::new(Vec::new());

    // Add the entries to the archive: one symlink to the target, followed by
    // another entry of the same name that sets the target to 0777.
    t!(ar.append_symlink("foo", &target).await);
    t!(ar
        .append_directory_with_metadata("foo", async_tar::EntryMetadata::new().mode(0o777))
        .await);

    // Get our raw archive.
    let bytes = t!(ar.into_inner().await);

    // Reopen it with permission preservation enabled.
    let mut ar = ArchiveBuilder::new(&bytes[..])
        .set_preserve_permissions(true)
        .build();

    // Ensure that we get an error on unpack.
    assert!(ar.unpack(&extract).await.is_err());

    // Also ensure that we still have a symlink and that it didn't turn into a
    // directory.
    assert!(t!(extract.join("foo").symlink_metadata())
        .file_type()
        .is_symlink());

    // Finally, ensure that the target directory is still 0700.
    let after = t!(fs::metadata(&target).await).permissions().mode() & 0o7777;
    assert_eq!(after, 0o700);
}
