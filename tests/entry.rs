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

#[tokio::test]
async fn absolute_symlink() {
    let mut ar = async_tar::Builder::new(Vec::new());

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Symlink);
    t!(header.set_path("foo"));
    t!(header.set_link_name("/bar"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

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
    let mut ar = async_tar::Builder::new(Vec::new());

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Regular);
    t!(header.set_path("foo"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Link);
    t!(header.set_path("bar"));
    // This absolute path under tempdir will be created at unpack time
    t!(header.set_link_name(td.path().join("foo")));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

    let bytes = t!(ar.into_inner().await);
    let mut ar = async_tar::Archive::new(&bytes[..]);

    t!(ar.unpack(td.path()).await);
    t!(td.path().join("foo").metadata());
    t!(td.path().join("bar").metadata());
}

#[tokio::test]
async fn relative_hardlink() {
    let mut ar = async_tar::Builder::new(Vec::new());

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Regular);
    t!(header.set_path("foo"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Link);
    t!(header.set_path("bar"));
    t!(header.set_link_name("foo"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

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

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Symlink);
    t!(header.set_path("foo"));
    t!(header.set_link_name("/"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Regular);
    t!(header.set_path("foo/bar"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

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

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Symlink);
    t!(header.set_path("foo"));
    t!(header.set_link_name("../../../../"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Regular);
    t!(header.set_path("foo/bar"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

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

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Directory);
    t!(header.set_path("foo"));
    header.set_mode(0o777);
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

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

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Symlink);
    t!(header.set_path("foo"));
    t!(header.set_link_name("bar"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Regular);
    t!(header.set_path("bar/foo"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Regular);
    t!(header.set_path("foo/bar"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

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

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Symlink);
    t!(header.set_path("symlink"));
    t!(header.set_link_name(".."));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Regular);
    t!(header.set_path("symlink/foo/bar"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

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

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Symlink);
    t!(header.set_path("foo"));
    t!(header.set_link_name(".."));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Regular);
    t!(header.set_path("foo/bar"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

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

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Symlink);
    t!(header.set_path(PathBuf::from("foo").join("bar")));
    t!(header.set_link_name(PathBuf::from("..").join("bar")));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Regular);
    t!(header.set_path("bar"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

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
    let mut ar = async_tar::Builder::new(Vec::new());

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Link);
    t!(header.set_path("foo"));
    t!(header.set_link_name("../test"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

    let mut header = async_tar::Header::new_gnu();
    header.set_size(1);
    header.set_entry_type(async_tar::EntryType::Regular);
    t!(header.set_path("foo"));
    header.set_cksum();
    t!(ar.append(&header, &b"x"[..]).await);

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

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Symlink);
    t!(header.set_path("foo"));
    t!(header.set_link_name("../test"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

    let mut header = async_tar::Header::new_gnu();
    header.set_size(1);
    header.set_entry_type(async_tar::EntryType::Regular);
    t!(header.set_path("foo"));
    header.set_cksum();
    t!(ar.append(&header, &b"x"[..]).await);

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

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Symlink);
    t!(header.set_path("foo"));
    t!(header.set_link_name("/bar"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

    let bytes = t!(ar.into_inner().await);

    let builder = ArchiveBuilder::new(&bytes[..]).set_allow_external_symlinks(false);
    let mut ar = builder.build();

    let td = t!(Builder::new().prefix("tar").tempdir());
    assert!(ar.unpack(td.path()).await.is_err());
}

#[tokio::test]
async fn deny_relative_link() {
    let mut ar = async_tar::Builder::new(Vec::new());

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Symlink);
    t!(header.set_path("foo"));
    t!(header.set_link_name("../../../../"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

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

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Symlink);
    t!(header.set_path("foo/bar"));
    t!(header.set_link_name("../baz"));
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

    let mut header = async_tar::Header::new_gnu();
    header.set_size(1);
    header.set_entry_type(async_tar::EntryType::Regular);
    t!(header.set_path("baz"));
    header.set_cksum();
    t!(ar.append(&header, &b"x"[..]).await);

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
    let mut pax_header = async_tar::Header::new_gnu();
    pax_header.set_entry_type(async_tar::EntryType::new(b'x')); // PAX local extensions
    t!(pax_header.set_path("PaxHeaders/file"));

    // Create malformed PAX extension data - the length field doesn't match the actual content length
    // Format is: "<length> <key>=<value>\n" where length includes itself
    let malformed_pax = b"99 path=test.txt\n"; // Claims to be 99 bytes but is only 17 bytes
    pax_header.set_size(malformed_pax.len() as u64);
    pax_header.set_cksum();

    // Manually write the archive
    ar_bytes.extend_from_slice(pax_header.as_bytes());
    ar_bytes.extend_from_slice(malformed_pax);
    // Pad to 512 byte boundary
    let padding = (512 - (malformed_pax.len() % 512)) % 512;
    ar_bytes.extend_from_slice(&vec![0u8; padding]);

    // Now add the actual file entry
    let mut header = async_tar::Header::new_gnu();
    header.set_size(4);
    header.set_entry_type(async_tar::EntryType::Regular);
    t!(header.set_path("file"));
    header.set_cksum();
    ar_bytes.extend_from_slice(header.as_bytes());
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
    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Symlink);
    t!(header.set_path("foo"));
    t!(header.set_link_name(&target));
    header.set_mode(0o777);
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

    let mut header = async_tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(async_tar::EntryType::Directory);
    t!(header.set_path("foo"));
    header.set_mode(0o777);
    header.set_cksum();
    t!(ar.append(&header, &[][..]).await);

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
