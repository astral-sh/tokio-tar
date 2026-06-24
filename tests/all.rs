extern crate tokio_tar as async_tar;

extern crate tempfile;
#[cfg(all(unix, feature = "xattr"))]
extern crate xattr;

use std::{
    io::Cursor,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tokio::{
    fs::{self, File},
    io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
};
use tokio_stream::*;

use async_tar::{Archive, ArchiveBuilder, Builder, EntryType, Header};
use tempfile::{Builder as TempBuilder, TempDir};

macro_rules! t {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => panic!("{} returned {}", stringify!($e), e),
        }
    };
}

macro_rules! tar {
    ($e:expr) => {
        &include_bytes!(concat!("archives/", $e))[..]
    };
}

mod header;

/// test that we can concatenate the simple.tar archive and extract the same entries twice when we
/// use the ignore_zeros option.
#[tokio::test]
async fn simple_concat() {
    let bytes = tar!("simple.tar");
    let mut archive_bytes = Vec::new();
    archive_bytes.extend(bytes);

    let original_names: Vec<String> =
        decode_names(&mut Archive::new(Cursor::new(&archive_bytes))).await;
    let expected: Vec<&str> = original_names.iter().map(|n| n.as_str()).collect();

    // concat two archives (with null in-between);
    archive_bytes.extend(bytes);

    // test now that when we read the archive, it stops processing at the first zero header.
    let actual = decode_names(&mut Archive::new(Cursor::new(&archive_bytes))).await;
    assert_eq!(expected, actual);

    // extend expected by itself.
    let expected: Vec<&str> = {
        let mut o = Vec::new();
        o.extend(&expected);
        o.extend(&expected);
        o
    };

    let builder = ArchiveBuilder::new(Cursor::new(&archive_bytes)).set_ignore_zeros(true);
    let mut ar = builder.build();

    let actual = decode_names(&mut ar).await;
    assert_eq!(expected, actual);

    async fn decode_names<R>(ar: &mut Archive<R>) -> Vec<String>
    where
        R: AsyncRead + Unpin,
    {
        let mut names = Vec::new();
        let mut entries = t!(ar.entries());

        while let Some(entry) = entries.next().await {
            let e = t!(entry);
            names.push(t!(::std::str::from_utf8(&t!(e.path_bytes()))).to_string());
        }

        names
    }
}

#[tokio::test]
async fn header_impls() {
    let mut ar = Archive::new(Cursor::new(tar!("simple.tar")));
    let hn = Header::new_old();
    let hnb = hn.as_bytes();
    let mut entries = t!(ar.entries());
    while let Some(file) = entries.next().await {
        let file = t!(file);
        let h1 = file.header();
        let h1b = h1.as_bytes();
        let h2 = h1.clone();
        let h2b = h2.as_bytes();
        assert!(h1b[..] == h2b[..] && h2b[..] != hnb[..])
    }
}

#[tokio::test]
async fn header_impls_missing_last_header() {
    let mut ar = Archive::new(Cursor::new(tar!("simple_missing_last_header.tar")));
    let hn = Header::new_old();
    let hnb = hn.as_bytes();
    let mut entries = t!(ar.entries());

    while let Some(file) = entries.next().await {
        let file = t!(file);
        let h1 = file.header();
        let h1b = h1.as_bytes();
        let h2 = h1.clone();
        let h2b = h2.as_bytes();
        assert!(h1b[..] == h2b[..] && h2b[..] != hnb[..])
    }
}

#[tokio::test]
async fn reading_files() {
    let rdr = Cursor::new(tar!("reading_files.tar"));
    let mut ar = Archive::new(rdr);
    let mut entries = t!(ar.entries());

    let mut a = t!(entries.next().await.unwrap());
    assert_eq!(&*a.header().path_bytes(), b"a");
    let mut s = String::new();
    t!(a.read_to_string(&mut s).await);
    assert_eq!(s, "a\na\na\na\na\na\na\na\na\na\na\n");

    let mut b = t!(entries.next().await.unwrap());
    assert_eq!(&*b.header().path_bytes(), b"b");
    s.truncate(0);
    t!(b.read_to_string(&mut s).await);
    assert_eq!(s, "b\nb\nb\nb\nb\nb\nb\nb\nb\nb\nb\n");

    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn unknown_typeflag_entry_payload_is_readable() {
    let mut builder = Builder::new(Vec::new());

    let mut mystery = Header::new_ustar();
    mystery.set_size(4);
    mystery.set_entry_type(EntryType::new(b'Z'));
    t!(builder
        .append_data(&mut mystery, "mystery", &b"DATA"[..])
        .await);

    let mut after = Header::new_ustar();
    after.set_size(5);
    t!(builder
        .append_data(&mut after, "after", &b"after"[..])
        .await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let mut entries = t!(archive.entries());

    let mut mystery = t!(entries.next().await.unwrap());
    assert_eq!(&*t!(mystery.path_bytes()), b"mystery");
    let mut mystery_data = Vec::new();
    t!(mystery.read_to_end(&mut mystery_data).await);
    assert_eq!(mystery_data, b"DATA");

    let mut after = t!(entries.next().await.unwrap());
    assert_eq!(&*t!(after.path_bytes()), b"after");
    let mut after_data = String::new();
    t!(after.read_to_string(&mut after_data).await);
    assert_eq!(after_data, "after");

    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn writing_files() {
    let mut ar = Builder::new(Vec::new());
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());

    let path = td.path().join("test");
    let mut file = t!(File::create(&path).await);
    t!(file.write_all(b"test").await);
    t!(file.flush().await);

    t!(ar
        .append_file("test2", &mut t!(File::open(&path).await))
        .await);

    let data = t!(ar.into_inner().await);
    let mut ar = Archive::new(Cursor::new(data));
    let mut entries = t!(ar.entries());
    let mut f = t!(entries.next().await.unwrap());

    assert_eq!(&*f.header().path_bytes(), b"test2");
    assert_eq!(f.header().size().unwrap(), 4);
    let mut s = String::new();
    t!(f.read_to_string(&mut s).await);
    assert_eq!(s, "test");

    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn large_filename() {
    let mut ar = Builder::new(Vec::new());
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());

    let path = td.path().join("test");
    let mut file = t!(File::create(&path).await);
    t!(file.write_all(b"test").await);
    t!(file.flush().await);

    let filename = "abcd/".repeat(50);
    let mut header = Header::new_ustar();
    header.set_path(&filename).unwrap();
    header.set_metadata(&t!(fs::metadata(&path).await));
    header.set_cksum();
    t!(ar.append(&header, &b"test"[..]).await);
    let too_long = "abcd".repeat(200);
    t!(ar
        .append_file(&too_long, &mut t!(File::open(&path).await))
        .await);
    t!(ar.append_data(&mut header, &too_long, &b"test"[..]).await);

    let rd = Cursor::new(t!(ar.into_inner().await));
    let mut ar = Archive::new(rd);
    let mut entries = t!(ar.entries());

    // The short entry added with `append`
    let mut f = entries.next().await.unwrap().unwrap();
    assert_eq!(&*f.header().path_bytes(), filename.as_bytes());
    assert_eq!(f.header().size().unwrap(), 4);
    let mut s = String::new();
    t!(f.read_to_string(&mut s).await);
    assert_eq!(s, "test");

    // The long entry added with `append_file`
    let mut f = entries.next().await.unwrap().unwrap();
    assert_eq!(&*t!(f.path_bytes()), too_long.as_bytes());
    assert_eq!(f.header().size().unwrap(), 4);
    let mut s = String::new();
    t!(f.read_to_string(&mut s).await);
    assert_eq!(s, "test");

    // The long entry added with `append_data`
    let mut f = entries.next().await.unwrap().unwrap();
    assert!(f.header().path_bytes().len() < too_long.len());
    assert_eq!(&*t!(f.path_bytes()), too_long.as_bytes());
    assert_eq!(f.header().size().unwrap(), 4);
    let mut s = String::new();
    t!(f.read_to_string(&mut s).await);
    assert_eq!(s, "test");

    assert!(entries.next().await.is_none());
}

// This test checks very particular scenario where a path component starting
// with ".." of a long path gets split at 100-byte mark so that ".." part goes
// into header and gets interpreted as parent dir (and rejected) .
#[tokio::test]
async fn large_filename_with_dot_dot_at_100_byte_mark() {
    let mut ar = Builder::new(Vec::new());

    let mut header = Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(4);

    let mut long_name_with_dot_dot = "tdir/".repeat(19);
    long_name_with_dot_dot.push_str("tt/..file");

    t!(ar
        .append_data(&mut header, &long_name_with_dot_dot, b"test".as_slice())
        .await);

    let rd = Cursor::new(t!(ar.into_inner().await));
    let mut ar = Archive::new(rd);
    let mut entries = t!(ar.entries());

    let mut f = t!(entries.next().await.unwrap());
    assert_eq!(&*t!(f.path_bytes()), long_name_with_dot_dot.as_bytes());
    assert_eq!(f.header().size().unwrap(), 4);
    let mut s = String::new();
    t!(f.read_to_string(&mut s).await);
    assert_eq!(s, "test");
    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn reading_entries() {
    let rdr = Cursor::new(tar!("reading_files.tar"));
    let mut ar = Archive::new(rdr);
    let mut entries = t!(ar.entries());
    let mut a = t!(entries.next().await.unwrap());
    assert_eq!(&*a.header().path_bytes(), b"a");
    let mut s = String::new();
    t!(a.read_to_string(&mut s).await);
    assert_eq!(s, "a\na\na\na\na\na\na\na\na\na\na\n");
    s.truncate(0);
    t!(a.read_to_string(&mut s).await);
    assert_eq!(s, "");
    let mut b = t!(entries.next().await.unwrap());

    assert_eq!(&*b.header().path_bytes(), b"b");
    s.truncate(0);
    t!(b.read_to_string(&mut s).await);
    assert_eq!(s, "b\nb\nb\nb\nb\nb\nb\nb\nb\nb\nb\n");
    assert!(entries.next().await.is_none());
}

async fn check_dirtree(td: &TempDir) {
    let dir_a = td.path().join("a");
    let dir_b = td.path().join("a/b");
    let file_c = td.path().join("a/c");
    assert!(fs::metadata(&dir_a)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false));
    assert!(fs::metadata(&dir_b)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false));
    assert!(fs::metadata(&file_c)
        .await
        .map(|m| m.is_file())
        .unwrap_or(false));
}

#[tokio::test]
async fn extracting_directories() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let rdr = Cursor::new(tar!("directory.tar"));
    let mut ar = Archive::new(rdr);
    t!(ar.unpack(td.path()).await);
    check_dirtree(&td).await;
}

#[tokio::test]
async fn extracting_duplicate_file_fail() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let path_present = td.path().join("a");
    let mut file = t!(File::create(path_present).await);
    t!(file.write_all(b"").await);
    t!(file.flush().await);

    let rdr = Cursor::new(tar!("reading_files.tar"));
    let builder = ArchiveBuilder::new(rdr).set_overwrite(false);
    let mut ar = builder.build();
    if let Err(err) = ar.unpack(td.path()).await {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            // as expected with overwrite false
            return;
        }
        panic!("unexpected error: {:?}", err);
    }
    panic!(
        "unpack() should have returned an error of kind {:?}, returned Ok",
        std::io::ErrorKind::AlreadyExists
    )
}

#[tokio::test]
async fn extracting_duplicate_file_succeed() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let path_present = td.path().join("a");
    let mut file = t!(File::create(path_present).await);
    t!(file.write_all(b"").await);
    t!(file.flush().await);

    let rdr = Cursor::new(tar!("reading_files.tar"));
    let builder = ArchiveBuilder::new(rdr).set_overwrite(true);
    let mut ar = builder.build();
    t!(ar.unpack(td.path()).await);
}

#[tokio::test]
#[cfg(unix)]
async fn extracting_duplicate_link_fail() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let path_present = td.path().join("lnk");
    t!(std::os::unix::fs::symlink("file", path_present));

    let rdr = Cursor::new(tar!("link.tar"));
    let builder = ArchiveBuilder::new(rdr).set_overwrite(false);
    let mut ar = builder.build();
    if let Err(err) = ar.unpack(td.path()).await {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            // as expected with overwrite false
            return;
        }
        panic!("unexpected error: {:?}", err);
    }
    panic!(
        "unpack() should have returned an error of kind {:?}, returned Ok",
        std::io::ErrorKind::AlreadyExists
    )
}

#[tokio::test]
#[cfg(unix)]
async fn extracting_duplicate_link_succeed() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let path_present = td.path().join("lnk");
    t!(std::os::unix::fs::symlink("file", path_present));

    let rdr = Cursor::new(tar!("link.tar"));
    let builder = ArchiveBuilder::new(rdr).set_overwrite(true);
    let mut ar = builder.build();
    t!(ar.unpack(td.path()).await);
}

#[tokio::test]
#[cfg(all(unix, feature = "xattr"))]
async fn xattrs() {
    // If /tmp is a tmpfs, xattr will fail
    // The xattr crate's unit tests also use /var/tmp for this reason
    let td = t!(TempBuilder::new()
        .prefix("async-tar")
        .tempdir_in("/var/tmp"));
    let rdr = Cursor::new(tar!("xattrs.tar"));
    let builder = ArchiveBuilder::new(rdr).set_unpack_xattrs(true);
    let mut ar = builder.build();
    t!(ar.unpack(td.path()).await);

    let val = xattr::get(td.path().join("a/b"), "user.pax.flags").unwrap();
    assert_eq!(val.unwrap(), b"epm");
}

#[tokio::test]
#[cfg(all(unix, feature = "xattr"))]
async fn no_xattrs() {
    // If /tmp is a tmpfs, xattr will fail
    // The xattr crate's unit tests also use /var/tmp for this reason
    let td = t!(TempBuilder::new()
        .prefix("async-tar")
        .tempdir_in("/var/tmp"));
    let rdr = Cursor::new(tar!("xattrs.tar"));
    let builder = ArchiveBuilder::new(rdr).set_unpack_xattrs(false);
    let mut ar = builder.build();
    t!(ar.unpack(td.path()).await);

    assert_eq!(
        xattr::get(td.path().join("a/b"), "user.pax.flags").unwrap(),
        None
    );
}

#[tokio::test]
async fn writing_and_extracting_directories() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());

    let mut ar = Builder::new(Vec::new());
    let tmppath = td.path().join("tmpfile");
    let mut file = t!(File::create(&tmppath).await);
    t!(file.write_all(b"c").await);
    t!(file.flush().await);
    t!(ar.append_dir("a", ".").await);
    t!(ar.append_dir("a/b", ".").await);
    t!(ar
        .append_file("a/c", &mut t!(File::open(&tmppath).await))
        .await);
    t!(ar.finish().await);

    let rdr = Cursor::new(t!(ar.into_inner().await));
    let mut ar = Archive::new(rdr);
    t!(ar.unpack(td.path()).await);
    check_dirtree(&td).await;
}

#[tokio::test]
async fn writing_directories_recursively() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());

    let base_dir = td.path().join("base");
    t!(fs::create_dir(&base_dir).await);
    let mut file1 = t!(File::create(base_dir.join("file1")).await);
    t!(file1.write_all(b"file1").await);
    t!(file1.flush().await);
    let sub_dir = base_dir.join("sub");
    t!(fs::create_dir(&sub_dir).await);
    let mut file2 = t!(File::create(sub_dir.join("file2")).await);
    t!(file2.write_all(b"file2").await);
    t!(file2.flush().await);

    let mut ar = Builder::new(Vec::new());
    t!(ar.append_dir_all("foobar", base_dir).await);
    let data = t!(ar.into_inner().await);

    let mut ar = Archive::new(Cursor::new(data));
    t!(ar.unpack(td.path()).await);
    let base_dir = td.path().join("foobar");
    assert!(fs::metadata(&base_dir)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false));
    let file1_path = base_dir.join("file1");
    assert!(fs::metadata(&file1_path)
        .await
        .map(|m| m.is_file())
        .unwrap_or(false));
    let sub_dir = base_dir.join("sub");
    assert!(fs::metadata(&sub_dir)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false));
    let file2_path = sub_dir.join("file2");
    assert!(fs::metadata(&file2_path)
        .await
        .map(|m| m.is_file())
        .unwrap_or(false));
}

#[tokio::test]
async fn append_dir_all_blank_dest() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());

    let base_dir = td.path().join("base");
    t!(fs::create_dir(&base_dir).await);
    let mut file1 = t!(File::create(base_dir.join("file1")).await);
    t!(file1.write_all(b"file1").await);
    t!(file1.flush().await);
    let sub_dir = base_dir.join("sub");
    t!(fs::create_dir(&sub_dir).await);
    let mut file2 = t!(File::create(sub_dir.join("file2")).await);
    t!(file2.write_all(b"file2").await);
    t!(file2.flush().await);

    let mut ar = Builder::new(Vec::new());
    t!(ar.append_dir_all("", base_dir).await);
    let data = t!(ar.into_inner().await);

    let mut ar = Archive::new(Cursor::new(data));
    t!(ar.unpack(td.path()).await);
    let base_dir = td.path();
    assert!(fs::metadata(&base_dir)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false));
    let file1_path = base_dir.join("file1");
    assert!(fs::metadata(&file1_path)
        .await
        .map(|m| m.is_file())
        .unwrap_or(false));
    let sub_dir = base_dir.join("sub");
    assert!(fs::metadata(&sub_dir)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false));
    let file2_path = sub_dir.join("file2");
    assert!(fs::metadata(&file2_path)
        .await
        .map(|m| m.is_file())
        .unwrap_or(false));
}

#[tokio::test]
async fn append_dir_all_does_not_work_on_non_directory() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let path = td.path().join("test");
    let mut file = t!(File::create(&path).await);
    t!(file.write_all(b"test").await);
    t!(file.flush().await);

    let mut ar = Builder::new(Vec::new());
    let result = ar.append_dir_all("test", path).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn extracting_duplicate_dirs() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let rdr = Cursor::new(tar!("duplicate_dirs.tar"));
    let mut ar = Archive::new(rdr);
    t!(ar.unpack(td.path()).await);

    let some_dir = td.path().join("some_dir");
    assert!(fs::metadata(&some_dir)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false));
}

#[tokio::test]
async fn unpack_old_style_bsd_dir() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());

    let mut ar = Builder::new(Vec::new());

    let mut header = Header::new_old();
    header.set_entry_type(EntryType::Regular);
    t!(header.set_path("testdir/"));
    header.set_size(0);
    header.set_cksum();
    t!(ar.append(&header, &mut io::empty()).await);

    // Extracting
    let rdr = Cursor::new(t!(ar.into_inner().await));
    let mut ar = Archive::new(rdr);
    t!(ar.unpack(td.path()).await);

    // Iterating
    let rdr = Cursor::new(ar.into_inner().map_err(|_| ()).unwrap().into_inner());
    let mut ar = Archive::new(rdr);
    let mut entries = t!(ar.entries());

    while let Some(e) = entries.next().await {
        assert!(e.is_ok());
    }

    assert!(td.path().join("testdir").is_dir());
}

#[tokio::test]
async fn handling_incorrect_file_size() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());

    let mut ar = Builder::new(Vec::new());

    let path = td.path().join("tmpfile");
    let mut file = t!(File::create(&path).await);
    t!(file.write_all(b"").await);
    t!(file.flush().await);
    let mut file = t!(File::open(&path).await);
    let mut header = Header::new_old();
    t!(header.set_path("somepath"));
    header.set_metadata(&t!(file.metadata().await));
    header.set_size(2048); // past the end of file null blocks
    header.set_cksum();
    t!(ar.append(&header, &mut file).await);

    // Extracting
    let rdr = Cursor::new(t!(ar.into_inner().await));
    let mut ar = Archive::new(rdr);
    assert!(ar.unpack(td.path()).await.is_err());

    // Iterating
    let rdr = Cursor::new(ar.into_inner().map_err(|_| ()).unwrap().into_inner());
    let mut ar = Archive::new(rdr);
    let mut entries = t!(ar.entries());
    while let Some(fr) = entries.next().await {
        if fr.is_err() {
            return;
        }
    }
    panic!("Should have errorred");
}

#[tokio::test]
async fn extracting_malicious_tarball() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());

    let mut evil_tar = Vec::new();

    evil_tar = {
        let mut a = Builder::new(evil_tar);
        async fn append<R: AsyncWrite + Send + Unpin>(a: &mut Builder<R>, path: &'static str) {
            let mut header = Header::new_gnu();
            assert!(header.set_path(path).is_err(), "was ok: {:?}", path);
            {
                let h = header.as_gnu_mut().unwrap();
                for (a, b) in h.name.iter_mut().zip(path.as_bytes()) {
                    *a = *b;
                }
            }
            header.set_size(1);
            header.set_cksum();
            t!(a.append(&header, io::repeat(1).take(1)).await);
        }

        append(&mut a, "/tmp/abs_evil.txt").await;
        // std parse `//` as UNC path, see rust-lang/rust#100833
        append(
            &mut a,
            #[cfg(not(windows))]
            "//tmp/abs_evil2.txt",
            #[cfg(windows)]
            "C://tmp/abs_evil2.txt",
        )
        .await;
        append(&mut a, "///tmp/abs_evil3.txt").await;
        append(&mut a, "/./tmp/abs_evil4.txt").await;
        append(
            &mut a,
            #[cfg(not(windows))]
            "//./tmp/abs_evil5.txt",
            #[cfg(windows)]
            "C://./tmp/abs_evil5.txt",
        )
        .await;
        append(&mut a, "///./tmp/abs_evil6.txt").await;
        append(&mut a, "/../tmp/rel_evil.txt").await;
        append(&mut a, "../rel_evil2.txt").await;
        append(&mut a, "./../rel_evil3.txt").await;
        append(&mut a, "some/../../rel_evil4.txt").await;
        append(&mut a, "").await;
        append(&mut a, "././//./..").await;
        append(&mut a, "..").await;
        append(&mut a, "/////////..").await;
        append(&mut a, "/////////").await;
        a.into_inner().await.unwrap()
    };

    let mut ar = Archive::new(&evil_tar[..]);
    t!(ar.unpack(td.path()).await);

    assert!(fs::metadata("/tmp/abs_evil.txt").await.is_err());
    assert!(fs::metadata("/tmp/abs_evil.txt2").await.is_err());
    assert!(fs::metadata("/tmp/abs_evil.txt3").await.is_err());
    assert!(fs::metadata("/tmp/abs_evil.txt4").await.is_err());
    assert!(fs::metadata("/tmp/abs_evil.txt5").await.is_err());
    assert!(fs::metadata("/tmp/abs_evil.txt6").await.is_err());
    assert!(fs::metadata("/tmp/rel_evil.txt").await.is_err());
    assert!(fs::metadata("/tmp/rel_evil.txt").await.is_err());
    assert!(fs::metadata(td.path().join("../tmp/rel_evil.txt"))
        .await
        .is_err());
    assert!(fs::metadata(td.path().join("../rel_evil2.txt"))
        .await
        .is_err());
    assert!(fs::metadata(td.path().join("../rel_evil3.txt"))
        .await
        .is_err());
    assert!(fs::metadata(td.path().join("../rel_evil4.txt"))
        .await
        .is_err());

    // The `some` subdirectory should not be created because the only
    // filename that references this has '..'.
    assert!(fs::metadata(td.path().join("some")).await.is_err());

    // The `tmp` subdirectory should be created and within this
    // subdirectory, there should be files named `abs_evil.txt` through
    // `abs_evil6.txt`.
    assert!(fs::metadata(td.path().join("tmp"))
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false));
    assert!(fs::metadata(td.path().join("tmp/abs_evil.txt"))
        .await
        .map(|m| m.is_file())
        .unwrap_or(false));
    assert!(fs::metadata(td.path().join("tmp/abs_evil2.txt"))
        .await
        .map(|m| m.is_file())
        .unwrap_or(false));
    assert!(fs::metadata(td.path().join("tmp/abs_evil3.txt"))
        .await
        .map(|m| m.is_file())
        .unwrap_or(false));
    assert!(fs::metadata(td.path().join("tmp/abs_evil4.txt"))
        .await
        .map(|m| m.is_file())
        .unwrap_or(false));
    assert!(fs::metadata(td.path().join("tmp/abs_evil5.txt"))
        .await
        .map(|m| m.is_file())
        .unwrap_or(false));
    assert!(fs::metadata(td.path().join("tmp/abs_evil6.txt"))
        .await
        .map(|m| m.is_file())
        .unwrap_or(false));

    // Paths "//tmp/abs_evil2.txt" and "//./tmp/abs_evil5.txt" are not absolute for Windows,
    // hence this test part does not work as expected on this OS.
    if cfg!(not(windows)) {
        assert!(fs::metadata(td.path().join("tmp/abs_evil2.txt"))
            .await
            .map(|m| m.is_file())
            .unwrap_or(false));
        assert!(fs::metadata(td.path().join("tmp/abs_evil5.txt"))
            .await
            .map(|m| m.is_file())
            .unwrap_or(false));
    }
}

#[tokio::test]
async fn octal_spaces() {
    let rdr = Cursor::new(tar!("spaces.tar"));
    let mut ar = Archive::new(rdr);

    let entry = ar.entries().unwrap().next().await.unwrap().unwrap();
    assert_eq!(entry.header().mode().unwrap() & 0o777, 0o777);
    assert_eq!(entry.header().uid().unwrap(), 0);
    assert_eq!(entry.header().gid().unwrap(), 0);
    assert_eq!(entry.header().size().unwrap(), 2);
    assert_eq!(entry.header().mtime().unwrap(), 0o12_440_016_664);
    assert_eq!(entry.header().cksum().unwrap(), 0o4253);
}

#[tokio::test]
async fn extracting_malformed_tar_null_blocks() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());

    let mut ar = Builder::new(Vec::new());

    let path1 = td.path().join("tmpfile1");
    let path2 = td.path().join("tmpfile2");
    t!(File::create(&path1).await);
    t!(File::create(&path2).await);
    t!(ar
        .append_file("tmpfile1", &mut t!(File::open(&path1).await))
        .await);
    let mut data = t!(ar.into_inner().await);
    let amt = data.len();
    data.truncate(amt - 512);
    let mut ar = Builder::new(data);
    t!(ar
        .append_file("tmpfile2", &mut t!(File::open(&path2).await))
        .await);
    t!(ar.finish().await);

    let data = t!(ar.into_inner().await);
    let mut ar = Archive::new(&data[..]);
    assert!(ar.unpack(td.path()).await.is_ok());
}

#[tokio::test]
async fn empty_filename() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let rdr = Cursor::new(tar!("empty_filename.tar"));
    let mut ar = Archive::new(rdr);
    assert!(ar.unpack(td.path()).await.is_ok());
}

#[tokio::test]
async fn file_times() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let rdr = Cursor::new(tar!("file_times.tar"));
    let mut ar = Archive::new(rdr);
    t!(ar.unpack(td.path()).await);

    let meta = fs::metadata(td.path().join("a")).await.unwrap();
    let mtime = t!(t!(meta.modified()).duration_since(UNIX_EPOCH));
    let atime = t!(t!(meta.accessed()).duration_since(UNIX_EPOCH));
    assert_eq!(mtime.as_secs(), 1_000_000_000);
    assert_eq!(mtime.subsec_nanos(), 0);
    assert_eq!(atime.as_secs(), 1_000_000_000);
    assert_eq!(atime.subsec_nanos(), 0);
}

#[tokio::test]
async fn backslash_treated_well() {
    // Insert a file into an archive with a backslash
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let mut ar = Builder::new(Vec::<u8>::new());
    t!(ar.append_dir("foo\\bar", td.path()).await);
    let mut ar = Archive::new(Cursor::new(t!(ar.into_inner().await)));
    let f = t!(t!(ar.entries()).next().await.unwrap());
    if cfg!(unix) {
        assert_eq!(t!(f.header().path()).to_str(), Some("foo\\bar"));
    } else {
        assert_eq!(t!(f.header().path()).to_str(), Some("foo/bar"));
    }

    // Unpack an archive with a backslash in the name
    let mut ar = Builder::new(Vec::<u8>::new());
    let mut header = Header::new_gnu();
    header.set_metadata(&t!(fs::metadata(td.path()).await));
    header.set_size(0);
    for (a, b) in header.as_old_mut().name.iter_mut().zip(b"foo\\bar\x00") {
        *a = *b;
    }
    header.set_cksum();
    t!(ar.append(&header, &mut io::empty()).await);
    let data = t!(ar.into_inner().await);
    let mut ar = Archive::new(&data[..]);
    let f = t!(t!(ar.entries()).next().await.unwrap());
    assert_eq!(t!(f.header().path()).to_str(), Some("foo\\bar"));

    let mut ar = Archive::new(&data[..]);
    t!(ar.unpack(td.path()).await);
    assert!(fs::metadata(td.path().join("foo\\bar")).await.is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn nul_bytes_in_path() {
    use std::{ffi::OsStr, os::unix::prelude::*};

    let nul_path = OsStr::from_bytes(b"foo\0");
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let mut ar = Builder::new(Vec::<u8>::new());
    let err = ar.append_dir(nul_path, td.path()).await.unwrap_err();
    assert!(err.to_string().contains("contains a nul byte"));
}

#[tokio::test]
async fn links() {
    let mut ar = Archive::new(Cursor::new(tar!("link.tar")));
    let mut entries = t!(ar.entries());
    let link = t!(entries.next().await.unwrap());
    assert_eq!(
        t!(link.header().link_name()).as_ref().map(|p| &**p),
        Some(Path::new("file"))
    );
    let other = t!(entries.next().await.unwrap());
    assert!(t!(other.header().link_name()).is_none());
}

#[tokio::test]
#[cfg(unix)] // making symlinks on windows is hard
async fn unpack_links() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let mut ar = Archive::new(Cursor::new(tar!("link.tar")));
    t!(ar.unpack(td.path()).await);

    let md = t!(fs::symlink_metadata(td.path().join("lnk")).await);
    assert!(md.file_type().is_symlink());

    let mtime = t!(t!(md.modified()).duration_since(UNIX_EPOCH));
    assert_eq!(mtime.as_secs(), 1448291033);

    assert_eq!(
        &*t!(fs::read_link(td.path().join("lnk")).await),
        Path::new("file")
    );
    t!(File::open(td.path().join("lnk")).await);
}

#[tokio::test]
async fn pax_simple() {
    let mut ar = Archive::new(tar!("pax.tar"));
    let mut entries = t!(ar.entries());

    let mut first = t!(entries.next().await.unwrap());
    let mut attributes = t!(first.pax_extensions().await).unwrap();
    let first = t!(attributes.next().unwrap());
    let second = t!(attributes.next().unwrap());
    let third = t!(attributes.next().unwrap());
    assert!(attributes.next().is_none());

    assert_eq!(first.key(), Ok("mtime"));
    assert_eq!(first.value(), Ok("1453146164.953123768"));
    assert_eq!(second.key(), Ok("atime"));
    assert_eq!(second.value(), Ok("1453251915.24892486"));
    assert_eq!(third.key(), Ok("ctime"));
    assert_eq!(third.value(), Ok("1453146164.953123768"));
}

#[tokio::test]
async fn unterminated_pax_record_is_rejected() {
    let mut ar = Archive::new(tar!("diff-014-unterminated-pax.tar"));
    let mut entries = t!(ar.entries());

    let err = match entries.next().await.unwrap() {
        Ok(_) => panic!("expected unterminated PAX record to be rejected"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("malformed pax extension"));
}

#[tokio::test]
async fn unterminated_pax_extensions_iterator_stops_after_error() {
    let mut ar = Archive::new(tar!("diff-014-unterminated-pax.tar"));
    let mut entries = t!(ar.entries_raw());
    let mut entry = t!(entries.next().await.unwrap());
    let mut extensions = t!(entry.pax_extensions().await).unwrap();

    assert!(extensions.next().unwrap().is_err());
    assert!(extensions.next().is_none());
}

#[tokio::test]
async fn pax_only_accepts_local_pax_entries() {
    for entry_type in [
        EntryType::Regular,
        EntryType::Link,
        EntryType::Symlink,
        EntryType::Char,
        EntryType::Block,
        EntryType::Directory,
        EntryType::Fifo,
    ] {
        let mut header = regular_ustar_header();
        header.set_entry_type(entry_type);
        header.set_cksum();

        let mut ar = ArchiveBuilder::new(Cursor::new(local_pax_archive(&header).await))
            .set_pax_only(true)
            .build();
        let mut entries = t!(ar.entries());

        let entry = t!(entries.next().await.unwrap());
        assert_eq!(entry.header().entry_type(), entry_type);
        assert!(entries.next().await.is_none());
    }
}

#[tokio::test]
async fn pax_only_rejects_raw_entries() {
    let header = regular_ustar_header();
    let mut ar = ArchiveBuilder::new(Cursor::new(local_pax_archive(&header).await))
        .set_pax_only(true)
        .build();
    let err = match ar.entries_raw() {
        Ok(_) => panic!("expected raw iteration in pax-only mode to be rejected"),
        Err(err) => err,
    };
    assert_eq!(
        err.to_string(),
        "raw entries are not supported by pax-only mode"
    );
}

#[tokio::test]
async fn pax_only_accepts_bare_ustar_entries() {
    let bytes = archive_with_header(&regular_ustar_header()).await;

    let mut ar = ArchiveBuilder::new(Cursor::new(bytes))
        .set_pax_only(true)
        .build();
    let mut entries = t!(ar.entries());
    let entry = t!(entries.next().await.unwrap());
    assert_eq!(entry.header().entry_type(), EntryType::Regular);
    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn pax_only_accepts_selective_local_pax_entries() {
    let mut builder = Builder::new(Vec::new());
    append_local_pax_header(&mut builder).await;
    let header = regular_ustar_header();
    t!(builder.append(&header, io::empty()).await);

    let mut header = regular_ustar_header();
    t!(header.set_path("bare"));
    header.set_cksum();
    t!(builder.append(&header, io::empty()).await);

    let mut ar = ArchiveBuilder::new(Cursor::new(t!(builder.into_inner().await)))
        .set_pax_only(true)
        .build();
    let mut entries = t!(ar.entries());

    let first = t!(entries.next().await.unwrap());
    assert_eq!(&*t!(first.path_bytes()), b"a");

    let second = t!(entries.next().await.unwrap());
    assert_eq!(&*t!(second.path_bytes()), b"bare");

    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn pax_only_rejects_non_pax_headers() {
    for mut header in [Header::new_old(), Header::new_gnu()] {
        t!(header.set_path("file"));
        header.set_size(0);
        header.set_cksum();

        let mut ar = ArchiveBuilder::new(Cursor::new(archive_with_header(&header).await))
            .set_pax_only(true)
            .build();
        let err = t!(ar.entries()).next().await.unwrap().unwrap_err();

        assert_eq!(
            err.to_string(),
            "archive header is not allowed by pax-only mode"
        );
    }
}

#[tokio::test]
async fn pax_only_rejects_non_pax_typeflags() {
    for entry_type in [
        EntryType::Continuous,
        EntryType::XGlobalHeader,
        EntryType::GNULongName,
        EntryType::GNULongLink,
        EntryType::GNUSparse,
        EntryType::new(b'X'),
    ] {
        let mut header = regular_ustar_header();
        header.set_entry_type(entry_type);
        header.set_cksum();

        let mut ar = ArchiveBuilder::new(Cursor::new(archive_with_header(&header).await))
            .set_pax_only(true)
            .build();
        let err = t!(ar.entries()).next().await.unwrap().unwrap_err();

        assert_eq!(
            err.to_string(),
            "archive header is not allowed by pax-only mode"
        );
    }
}

#[tokio::test]
async fn pax_only_rejects_non_octal_numeric_fields() {
    for (field, byte) in [
        ("mode", b'8'),
        ("uid", 0x80),
        ("gid", 0x80),
        ("size", 0x80),
        ("mtime", 0x80),
        ("dev_major", 0x80),
        ("dev_minor", 0x80),
    ] {
        let mut header = regular_ustar_header();
        let ustar = header.as_ustar_mut().unwrap();
        match field {
            "mode" => ustar.mode[0] = byte,
            "uid" => ustar.uid[0] = byte,
            "gid" => ustar.gid[0] = byte,
            "size" => {
                ustar.size = [0; 12];
                ustar.size[0] = byte;
            }
            "mtime" => ustar.mtime[0] = byte,
            "dev_major" => ustar.dev_major[0] = byte,
            "dev_minor" => ustar.dev_minor[0] = byte,
            _ => unreachable!(),
        }
        header.set_cksum();

        let mut ar = ArchiveBuilder::new(Cursor::new(local_pax_archive(&header).await))
            .set_pax_only(true)
            .build();
        let err = t!(ar.entries()).next().await.unwrap().unwrap_err();

        assert_eq!(
            err.to_string(),
            "archive header is not allowed by pax-only mode",
            "field: {field}"
        );
    }
}

#[tokio::test]
async fn pax_only_rejects_dangling_local_pax_headers() {
    let mut ar = ArchiveBuilder::new(Cursor::new(local_pax_header_archive().await))
        .set_pax_only(true)
        .build();
    let err = t!(ar.entries()).next().await.unwrap().unwrap_err();
    assert_eq!(
        err.to_string(),
        "local pax header is not followed by an archive entry"
    );
}

#[tokio::test]
async fn pax_only_rejects_stacked_local_pax_headers() {
    let mut ar = ArchiveBuilder::new(Cursor::new(stacked_local_pax_header_archive().await))
        .set_pax_only(true)
        .build();
    let err = t!(ar.entries()).next().await.unwrap().unwrap_err();
    assert_eq!(
        err.to_string(),
        "two pax extensions entries describing the same member"
    );
}

fn regular_ustar_header() -> Header {
    let mut header = Header::new_ustar();
    t!(header.set_path("file"));
    header.set_size(0);
    header.set_cksum();
    header
}

async fn archive_with_header(header: &Header) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    t!(builder.append(header, io::empty()).await);
    t!(builder.into_inner().await)
}

async fn local_pax_archive(header: &Header) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    append_local_pax_header(&mut builder).await;
    t!(builder.append(header, io::empty()).await);
    t!(builder.into_inner().await)
}

async fn local_pax_header_archive() -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    append_local_pax_header(&mut builder).await;
    t!(builder.into_inner().await)
}

async fn stacked_local_pax_header_archive() -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    append_local_pax_header(&mut builder).await;
    append_local_pax_header(&mut builder).await;
    t!(builder.into_inner().await)
}

async fn append_local_pax_header(builder: &mut Builder<Vec<u8>>) {
    let record = b"9 path=a\n";
    let mut header = Header::new_ustar();
    t!(header.set_path("PaxHeaders/file"));
    header.set_entry_type(EntryType::XHeader);
    header.set_size(record.len() as u64);
    header.set_cksum();
    t!(builder.append(&header, &record[..]).await);
}

#[tokio::test]
async fn pending_pax_record_at_archive_end_is_rejected() {
    let mut ar = Archive::new(tar!("diff-024-pending-pax-boundary.tar"));
    let mut entries = t!(ar.entries());

    let err = match entries.next().await.unwrap() {
        Ok(_) => panic!("expected unterminated PAX state to be rejected"),
        Err(err) => err,
    };
    assert!(err
        .to_string()
        .contains("extension entry was not followed by a member"));
}

#[tokio::test]
async fn pending_pax_record_does_not_cross_ignored_terminator() {
    let mut bytes = tar!("diff-024-pending-pax-boundary.tar").to_vec();
    bytes.extend_from_slice(tar!("simple.tar"));

    let builder = ArchiveBuilder::new(Cursor::new(bytes)).set_ignore_zeros(true);
    let mut ar = builder.build();
    let mut entries = t!(ar.entries());

    let err = match entries.next().await.unwrap() {
        Ok(_) => panic!("expected PAX state at a terminator to be rejected"),
        Err(err) => err,
    };
    assert!(err
        .to_string()
        .contains("extension entry was not followed by a member"));
}

#[tokio::test]
async fn pax_pending_interrupted() {
    use std::pin::Pin;

    /// A [`AsyncRead`] that returns `Pending` on every other poll.
    struct PendingReader<R> {
        inner: R,
        n: usize,
    }

    impl<R> PendingReader<R>
    where
        R: AsyncRead + Unpin,
    {
        fn new(reader: R) -> Self {
            Self {
                inner: reader,
                n: 0,
            }
        }

        fn project(self: Pin<&mut Self>) -> (Pin<&mut R>, &mut usize) {
            let Self { inner, n } = std::pin::Pin::into_inner(self);
            (Pin::new(inner), n)
        }
    }
    impl<R> AsyncRead for PendingReader<R>
    where
        R: AsyncRead + Unpin,
    {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            use std::task::Poll;

            let (inner, n) = self.project();

            let pend = *n % 2 == 0;
            *n += 1;

            if pend {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }

            inner.poll_read(cx, buf)
        }
    }

    let ar = tar!("paxlongname.tar");
    let ar = PendingReader::new(ar);
    let mut ar = Archive::new(ar);
    let mut entries = t!(ar.entries());

    let entry = t!(entries.next().await.unwrap());
    let path = t!(entry.path());
    let path = path.to_str().unwrap();

    assert_eq!(path, "this_file_name_will_be_one_hundred_and_one_characters_long_once_i_add_some_more_characters_at_the_end");
}

#[tokio::test]
async fn pax_path() {
    let mut ar = Archive::new(tar!("pax2.tar"));
    let mut entries = t!(ar.entries());

    let first = t!(entries.next().await.unwrap());
    assert!(first.path().unwrap().ends_with("aaaaaaaaaaaaaaa"));
}

#[tokio::test]
async fn pax_precedence() {
    let mut ar = Archive::new(tar!("pax-header-precedence.tar"));
    let mut entries = t!(ar.entries());

    let first = t!(entries.next().await.unwrap());
    assert!(first.path().unwrap().ends_with("normal.txt"));

    let second = t!(entries.next().await.unwrap());
    assert!(second.path().unwrap().ends_with("blob.bin"));

    let third = t!(entries.next().await.unwrap());
    assert!(third.path().unwrap().ends_with("marker.txt"));

    assert!(entries.next().await.is_none());
}

async fn pax_numeric_override_archive(pax: &[u8]) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());

    let mut pax_header = Header::new_ustar();
    pax_header.set_entry_type(EntryType::new(b'x'));
    t!(pax_header.set_path("PaxHeaders/file"));
    pax_header.set_size(pax.len() as u64);
    pax_header.set_cksum();
    t!(builder.append(&pax_header, pax).await);

    let mut file_header = Header::new_ustar();
    t!(file_header.set_path("file"));
    file_header.set_size(2);
    file_header.set_cksum();
    t!(builder.append(&file_header, b"ok".as_slice()).await);

    t!(builder.into_inner().await)
}

#[tokio::test]
async fn pax_numeric_overrides_accept_decimal_digits() {
    let contents = pax_numeric_override_archive(b"9 size=2\n9 uid=42\n9 gid=43\n").await;
    let mut archive = Archive::new(&contents[..]);
    let mut entries = t!(archive.entries());

    let entry = t!(entries.next().await.unwrap());
    assert_eq!(t!(entry.header().size()), 2);
    assert_eq!(t!(entry.header().uid()), 42);
    assert_eq!(t!(entry.header().gid()), 43);
    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn pax_numeric_overrides_reject_sign_prefixes() {
    for pax in [
        b"11 size=+2\n".as_slice(),
        b"11 uid=+42\n".as_slice(),
        b"11 gid=+43\n".as_slice(),
    ] {
        let contents = pax_numeric_override_archive(pax).await;
        let mut archive = Archive::new(&contents[..]);
        let mut entries = t!(archive.entries());
        let entry = entries.next().await.unwrap();
        let error = entry.expect_err("expected a sign-prefixed PAX numeric value to fail");

        assert!(
            error.to_string().contains("failed to parse pax"),
            "bad error: {}",
            error
        );
    }
}

#[tokio::test]
async fn pax_owner_names_override_entry_metadata() {
    let mut builder = Builder::new(Vec::new());

    let owner_names = b"18 uname=pax-user\n19 gname=pax-group\n";
    let mut extension = Header::new_ustar();
    extension.set_size(owner_names.len() as u64);
    extension.set_entry_type(EntryType::new(b'x'));
    t!(builder
        .append_data(&mut extension, "pax", &owner_names[..])
        .await);

    let mut file = Header::new_ustar();
    file.set_size(4);
    t!(file.set_username("raw-user"));
    t!(file.set_groupname("raw-group"));
    t!(builder.append_data(&mut file, "file", &b"DATA"[..]).await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let mut entries = t!(archive.entries());

    let file = t!(entries.next().await.unwrap());
    assert_eq!(t!(file.username()), Some("pax-user"));
    assert_eq!(t!(file.groupname()), Some("pax-group"));
    assert_eq!(t!(file.header().username()), Some("pax-user"));
    assert_eq!(t!(file.header().groupname()), Some("pax-group"));
    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn last_pax_owner_metadata_wins() {
    let mut owner_metadata = pax_record("uid", b"111");
    owner_metadata.extend(pax_record("uid", b"222"));
    owner_metadata.extend(pax_record("gid", b"333"));
    owner_metadata.extend(pax_record("gid", b"444"));
    owner_metadata.extend(pax_record("uname", b"pax-user-first"));
    owner_metadata.extend(pax_record("uname", b"pax-user-last"));
    owner_metadata.extend(pax_record("gname", b"pax-group-first"));
    owner_metadata.extend(pax_record("gname", b"pax-group-last"));

    let mut builder = Builder::new(Vec::new());
    let mut extension = Header::new_ustar();
    extension.set_size(owner_metadata.len() as u64);
    extension.set_entry_type(EntryType::new(b'x'));
    t!(builder
        .append_data(&mut extension, "pax", &owner_metadata[..])
        .await);

    let mut file = Header::new_ustar();
    file.set_size(4);
    file.set_uid(1);
    file.set_gid(2);
    t!(file.set_username("raw-user"));
    t!(file.set_groupname("raw-group"));
    t!(builder.append_data(&mut file, "file", &b"DATA"[..]).await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let mut entries = t!(archive.entries());

    let file = t!(entries.next().await.unwrap());
    assert_eq!(t!(file.header().uid()), 222);
    assert_eq!(t!(file.header().gid()), 444);
    assert_eq!(file.username_bytes(), Some(&b"pax-user-last"[..]));
    assert_eq!(file.groupname_bytes(), Some(&b"pax-group-last"[..]));
    assert_eq!(t!(file.header().username()), Some("pax-user-last"));
    assert_eq!(t!(file.header().groupname()), Some("pax-group-last"));
    assert!(entries.next().await.is_none());
}

fn pax_record(key: &str, value: &[u8]) -> Vec<u8> {
    let body_len = 1 + key.len() + 1 + value.len() + 1;
    let mut len = body_len + 1;
    loop {
        let actual_len = len.to_string().len() + body_len;
        if actual_len == len {
            break;
        }
        len = actual_len;
    }

    let mut record = format!("{len} {key}=").into_bytes();
    record.extend_from_slice(value);
    record.push(b'\n');
    assert_eq!(record.len(), len);
    record
}

async fn local_pax_record_archive(entry_type: EntryType, record: &[u8]) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());

    let mut extension = Header::new_ustar();
    extension.set_size(record.len() as u64);
    extension.set_entry_type(entry_type);
    t!(builder.append_data(&mut extension, "pax", record).await);

    let mut file = Header::new_ustar();
    file.set_size(4);
    t!(builder
        .append_data(&mut file, "raw-name", &b"DATA"[..])
        .await);

    t!(builder.into_inner().await)
}

#[tokio::test]
async fn pax_long_owner_names_are_exposed_by_entry() {
    let uname = "pax-user-name-longer-than-the-fixed-width-header-slot";
    let gname = "pax-group-name-longer-than-the-fixed-width-header-slot";
    let mut owner_names = pax_record("uname", uname.as_bytes());
    owner_names.extend(pax_record("gname", gname.as_bytes()));

    let mut builder = Builder::new(Vec::new());
    let mut extension = Header::new_ustar();
    extension.set_size(owner_names.len() as u64);
    extension.set_entry_type(EntryType::new(b'x'));
    t!(builder
        .append_data(&mut extension, "pax", &owner_names[..])
        .await);

    let mut file = Header::new_ustar();
    file.set_size(4);
    t!(file.set_username("raw-user"));
    t!(file.set_groupname("raw-group"));
    t!(builder.append_data(&mut file, "file", &b"DATA"[..]).await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let mut entries = t!(archive.entries());

    let file = t!(entries.next().await.unwrap());
    assert_eq!(file.username_bytes(), Some(uname.as_bytes()));
    assert_eq!(file.groupname_bytes(), Some(gname.as_bytes()));
    assert_eq!(t!(file.username()), Some(uname));
    assert_eq!(t!(file.groupname()), Some(gname));
    assert_eq!(t!(file.header().username()), Some("raw-user"));
    assert_eq!(t!(file.header().groupname()), Some("raw-group"));
    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn pax_empty_owner_names_are_rejected() {
    for key in ["uname", "gname"] {
        let record = pax_record(key, b"");
        let bytes = local_pax_record_archive(EntryType::XHeader, &record).await;
        let mut archive = Archive::new(&bytes[..]);
        let mut entries = t!(archive.entries());

        let err = entries.next().await.unwrap().unwrap_err();
        assert_eq!(
            err.to_string(),
            "empty values are not supported in local pax extensions"
        );
    }
}

#[tokio::test]
async fn unknown_empty_local_pax_values_are_rejected() {
    let record = pax_record("VENDOR.unknown", b"");

    for entry_type in [EntryType::XHeader, EntryType::SolarisXHeader] {
        let bytes = local_pax_record_archive(entry_type, &record).await;
        let mut archive = Archive::new(&bytes[..]);
        let mut entries = t!(archive.entries());

        let err = entries.next().await.unwrap().unwrap_err();
        assert_eq!(
            err.to_string(),
            "empty values are not supported in local pax extensions"
        );
    }
}

#[tokio::test]
async fn unknown_nonempty_local_pax_values_are_accepted() {
    let record = pax_record("VENDOR.unknown", b"opaque");

    for entry_type in [EntryType::XHeader, EntryType::SolarisXHeader] {
        let bytes = local_pax_record_archive(entry_type, &record).await;
        let mut archive = Archive::new(&bytes[..]);
        let mut entries = t!(archive.entries());

        let mut file = t!(entries.next().await.unwrap());
        assert_eq!(&*t!(file.path_bytes()), b"raw-name");
        let mut contents = Vec::new();
        t!(file.read_to_end(&mut contents).await);
        assert_eq!(contents, b"DATA");
        assert!(entries.next().await.is_none());
    }
}

#[tokio::test]
async fn empty_global_pax_values_remain_visible() {
    let record = pax_record("comment", b"");
    let mut builder = Builder::new(Vec::new());
    let mut extension = Header::new_ustar();
    extension.set_size(record.len() as u64);
    extension.set_entry_type(EntryType::XGlobalHeader);
    t!(builder
        .append_data(&mut extension, "global-pax", &record[..])
        .await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let mut entries = t!(archive.entries());

    let mut entry = t!(entries.next().await.unwrap());
    assert_eq!(entry.header().entry_type(), EntryType::XGlobalHeader);
    let mut extensions = t!(entry.pax_extensions().await).unwrap();
    let extension = t!(extensions.next().unwrap());
    assert_eq!(extension.key_bytes(), b"comment");
    assert_eq!(extension.value_bytes(), b"");
    assert!(extensions.next().is_none());
    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn pax_non_utf8_owner_names_are_exposed_as_bytes() {
    let uname = b"\xffpax-user";
    let gname = b"\xfepax-group";
    let mut owner_names = pax_record("uname", uname);
    owner_names.extend(pax_record("gname", gname));

    let mut builder = Builder::new(Vec::new());
    let mut extension = Header::new_ustar();
    extension.set_size(owner_names.len() as u64);
    extension.set_entry_type(EntryType::new(b'x'));
    t!(builder
        .append_data(&mut extension, "pax", &owner_names[..])
        .await);

    let mut file = Header::new_ustar();
    file.set_size(4);
    t!(builder.append_data(&mut file, "file", &b"DATA"[..]).await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let mut entries = t!(archive.entries());

    let file = t!(entries.next().await.unwrap());
    assert_eq!(file.username_bytes(), Some(&uname[..]));
    assert_eq!(file.groupname_bytes(), Some(&gname[..]));
    assert!(file.username().is_err());
    assert!(file.groupname().is_err());
    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn pax_size_updates_header() {
    let mut ar = Archive::new(tar!("pax-header-precedence.tar"));
    let mut entries = t!(ar.entries());

    let first = t!(entries.next().await.unwrap());
    assert!(first.path().unwrap().ends_with("normal.txt"));

    let second = t!(entries.next().await.unwrap());
    assert!(second.path().unwrap().ends_with("blob.bin"));
    assert_eq!(second.header().size().unwrap(), 1024);
}

#[tokio::test]
async fn last_pax_size_wins() {
    let mut sizes = pax_record("size", b"3");
    sizes.extend(pax_record("size", b"4"));

    let mut builder = Builder::new(Vec::new());
    let mut extension = Header::new_ustar();
    extension.set_size(sizes.len() as u64);
    extension.set_entry_type(EntryType::new(b'x'));
    t!(builder.append_data(&mut extension, "pax", &sizes[..]).await);

    let mut file = Header::new_ustar();
    file.set_size(4);
    t!(builder.append_data(&mut file, "file", &b"DATA"[..]).await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let mut entries = t!(archive.entries());

    let mut file = t!(entries.next().await.unwrap());
    assert_eq!(t!(file.header().size()), 4);

    let mut contents = Vec::new();
    t!(file.read_to_end(&mut contents).await);
    assert_eq!(contents, b"DATA");
    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn solaris_extended_header_applies_pax_path() {
    let mut builder = Builder::new(Vec::new());

    let pax_path = b"21 path=solaris-path\n";
    let mut extension = Header::new_ustar();
    extension.set_size(pax_path.len() as u64);
    extension.set_entry_type(EntryType::new(b'X'));
    t!(builder
        .append_data(&mut extension, "SolarisHeader", &pax_path[..])
        .await);

    let mut file = Header::new_ustar();
    file.set_size(4);
    t!(builder
        .append_data(&mut file, "raw-name", &b"DATA"[..])
        .await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let mut entries = t!(archive.entries());

    let mut file = t!(entries.next().await.unwrap());
    assert_eq!(&*t!(file.path_bytes()), b"solaris-path");
    let mut contents = String::new();
    t!(file.read_to_string(&mut contents).await);
    assert_eq!(contents, "DATA");
    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn gnu_x_typeflag_is_not_a_solaris_pax_extension() {
    let mut builder = Builder::new(Vec::new());

    let pax_path = b"21 path=solaris-path\n";
    let mut extension = Header::new_gnu();
    extension.set_size(pax_path.len() as u64);
    extension.set_entry_type(EntryType::new(b'X'));
    t!(builder
        .append_data(&mut extension, "GNUHeader", &pax_path[..])
        .await);

    let mut file = Header::new_ustar();
    file.set_size(4);
    t!(builder
        .append_data(&mut file, "raw-name", &b"DATA"[..])
        .await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let mut entries = t!(archive.entries());

    let mut extension = t!(entries.next().await.unwrap());
    assert_eq!(&*t!(extension.path_bytes()), b"GNUHeader");
    let mut contents = String::new();
    t!(extension.read_to_string(&mut contents).await);
    assert_eq!(contents, "21 path=solaris-path\n");

    let file = t!(entries.next().await.unwrap());
    assert_eq!(&*t!(file.path_bytes()), b"raw-name");

    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn orphaned_gnu_sparse_pax_metadata_is_rejected() {
    let cases: &[&[u8]] = &[
        b"34 GNU.sparse.name=sparse-aliased\n",
        b"21 path=path-aliased\n34 GNU.sparse.name=sparse-aliased\n",
        b"34 GNU.sparse.name=sparse-aliased\n21 path=path-aliased\n",
    ];

    for &extensions in cases {
        let mut builder = Builder::new(Vec::new());

        let mut extension = Header::new_ustar();
        extension.set_size(extensions.len() as u64);
        extension.set_entry_type(EntryType::new(b'x'));
        t!(builder.append_data(&mut extension, "pax", extensions).await);

        let mut file = Header::new_ustar();
        file.set_size(4);
        t!(builder
            .append_data(&mut file, "raw-name", &b"DATA"[..])
            .await);

        let bytes = t!(builder.into_inner().await);
        let mut archive = Archive::new(&bytes[..]);
        let mut entries = t!(archive.entries());

        let error = entries
            .next()
            .await
            .unwrap()
            .expect_err("expected orphaned GNU sparse PAX metadata to be rejected");
        assert_eq!(
            error.to_string(),
            "orphaned GNU sparse pax metadata is not supported"
        );
    }
}

#[tokio::test]
async fn long_name_trailing_nul() {
    let mut b = Builder::new(Vec::<u8>::new());

    let mut h = Header::new_gnu();
    t!(h.set_path("././@LongLink"));
    h.set_size(4);
    h.set_entry_type(EntryType::new(b'L'));
    h.set_cksum();
    t!(b.append(&h, b"foo\0" as &[u8]).await);
    let mut h = Header::new_gnu();

    t!(h.set_path("bar"));
    h.set_size(6);
    h.set_entry_type(EntryType::file());
    h.set_cksum();
    t!(b.append(&h, b"foobar" as &[u8]).await);

    let contents = t!(b.into_inner().await);
    let mut a = Archive::new(&contents[..]);

    let e = t!(t!(a.entries()).next().await.unwrap());
    assert_eq!(&*t!(e.path_bytes()), b"foo");
}

#[tokio::test]
async fn long_name_stops_at_first_nul() {
    let mut b = Builder::new(Vec::<u8>::new());

    let mut h = Header::new_gnu();
    t!(h.set_path("././@LongLink"));
    h.set_size(8);
    h.set_entry_type(EntryType::new(b'L'));
    h.set_cksum();
    t!(b.append(&h, b"foo\0bar\0" as &[u8]).await);

    let mut h = Header::new_gnu();
    t!(h.set_path("fallback"));
    h.set_size(0);
    h.set_entry_type(EntryType::file());
    h.set_cksum();
    t!(b.append(&h, &[][..]).await);

    let contents = t!(b.into_inner().await);
    let mut a = Archive::new(&contents[..]);
    let e = t!(t!(a.entries()).next().await.unwrap());

    assert_eq!(&*t!(e.path_bytes()), b"foo");
}

#[tokio::test]
async fn long_linkname_trailing_nul() {
    let mut b = Builder::new(Vec::<u8>::new());

    let mut h = Header::new_gnu();
    t!(h.set_path("././@LongLink"));
    h.set_size(4);
    h.set_entry_type(EntryType::new(b'K'));
    h.set_cksum();
    t!(b.append(&h, b"foo\0" as &[u8]).await);
    let mut h = Header::new_gnu();

    t!(h.set_path("bar"));
    h.set_size(6);
    h.set_entry_type(EntryType::file());
    h.set_cksum();
    t!(b.append(&h, b"foobar" as &[u8]).await);

    let contents = t!(b.into_inner().await);
    let mut a = Archive::new(&contents[..]);

    let e = t!(t!(a.entries()).next().await.unwrap());
    assert_eq!(&*t!(e.link_name_bytes()).unwrap(), b"foo");
}

#[tokio::test]
async fn encoded_long_name_has_trailing_nul() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let path = td.path().join("foo");
    t!(t!(File::create(&path).await).write_all(b"test").await);

    let mut b = Builder::new(Vec::<u8>::new());
    let long = "abcd".repeat(200);

    t!(b.append_file(&long, &mut t!(File::open(&path).await)).await);

    let contents = t!(b.into_inner().await);
    let mut a = Archive::new(&contents[..]);

    let mut e = t!(t!(a.entries_raw()).next().await.unwrap());
    let mut name = Vec::new();
    t!(e.read_to_end(&mut name).await);
    assert_eq!(name[name.len() - 1], 0);

    let header_name = &e.header().as_gnu().unwrap().name;
    assert!(header_name.starts_with(b"././@LongLink\x00"));
}

#[tokio::test]
async fn reading_sparse() {
    let rdr = Cursor::new(tar!("sparse.tar"));
    let mut ar = Archive::new(rdr);
    let mut entries = t!(ar.entries());

    let mut a = t!(entries.next().await.unwrap());
    let mut s = String::new();
    assert_eq!(&*a.header().path_bytes(), b"sparse_begin.txt");
    t!(a.read_to_string(&mut s).await);
    assert_eq!(&s[..5], "test\n");
    assert!(s[5..].chars().all(|x| x == '\u{0}'));

    let mut a = t!(entries.next().await.unwrap());
    let mut s = String::new();
    assert_eq!(&*a.header().path_bytes(), b"sparse_end.txt");
    t!(a.read_to_string(&mut s).await);
    assert!(s[..s.len() - 9].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[s.len() - 9..], "test_end\n");

    let mut a = t!(entries.next().await.unwrap());
    let mut s = String::new();
    assert_eq!(&*a.header().path_bytes(), b"sparse_ext.txt");
    t!(a.read_to_string(&mut s).await);
    assert!(s[..0x1000].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0x1000..0x1000 + 5], "text\n");
    assert!(s[0x1000 + 5..0x3000].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0x3000..0x3000 + 5], "text\n");
    assert!(s[0x3000 + 5..0x5000].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0x5000..0x5000 + 5], "text\n");
    assert!(s[0x5000 + 5..0x7000].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0x7000..0x7000 + 5], "text\n");
    assert!(s[0x7000 + 5..0x9000].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0x9000..0x9000 + 5], "text\n");
    assert!(s[0x9000 + 5..0xb000].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0xb000..0xb000 + 5], "text\n");

    let mut a = t!(entries.next().await.unwrap());
    let mut s = String::new();
    assert_eq!(&*a.header().path_bytes(), b"sparse.txt");
    t!(a.read_to_string(&mut s).await);
    assert!(s[..0x1000].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0x1000..0x1000 + 6], "hello\n");
    assert!(s[0x1000 + 6..0x2fa0].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0x2fa0..0x2fa0 + 6], "world\n");
    assert!(s[0x2fa0 + 6..0x4000].chars().all(|x| x == '\u{0}'));

    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn extract_sparse() {
    let rdr = Cursor::new(tar!("sparse.tar"));
    let mut ar = Archive::new(rdr);
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    t!(ar.unpack(td.path()).await);

    let mut s = String::new();
    t!(t!(File::open(td.path().join("sparse_begin.txt")).await)
        .read_to_string(&mut s)
        .await);
    assert_eq!(&s[..5], "test\n");
    assert!(s[5..].chars().all(|x| x == '\u{0}'));

    s.truncate(0);
    t!(t!(File::open(td.path().join("sparse_end.txt")).await)
        .read_to_string(&mut s)
        .await);
    assert!(s[..s.len() - 9].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[s.len() - 9..], "test_end\n");

    s.truncate(0);
    t!(t!(File::open(td.path().join("sparse_ext.txt")).await)
        .read_to_string(&mut s)
        .await);
    assert!(s[..0x1000].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0x1000..0x1000 + 5], "text\n");
    assert!(s[0x1000 + 5..0x3000].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0x3000..0x3000 + 5], "text\n");
    assert!(s[0x3000 + 5..0x5000].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0x5000..0x5000 + 5], "text\n");
    assert!(s[0x5000 + 5..0x7000].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0x7000..0x7000 + 5], "text\n");
    assert!(s[0x7000 + 5..0x9000].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0x9000..0x9000 + 5], "text\n");
    assert!(s[0x9000 + 5..0xb000].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0xb000..0xb000 + 5], "text\n");

    s.truncate(0);
    t!(t!(File::open(td.path().join("sparse.txt")).await)
        .read_to_string(&mut s)
        .await);
    assert!(s[..0x1000].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0x1000..0x1000 + 6], "hello\n");
    assert!(s[0x1000 + 6..0x2fa0].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0x2fa0..0x2fa0 + 6], "world\n");
    assert!(s[0x2fa0 + 6..0x4000].chars().all(|x| x == '\u{0}'));
}

#[tokio::test]
async fn large_sparse() {
    let rdr = Cursor::new(tar!("sparse-large.tar"));
    let mut ar = Archive::new(rdr);
    let mut entries = t!(ar.entries());
    // Only check the header info without extracting, as the file is very large,
    // and not all filesystems support sparse files.
    let a = t!(entries.next().await.unwrap());
    let h = a.header().as_gnu().unwrap();
    assert_eq!(h.real_size().unwrap(), 12626929280);
}

#[tokio::test]
async fn sparse_with_trailing() {
    let rdr = Cursor::new(tar!("sparse-1.tar"));
    let mut ar = Archive::new(rdr);
    let mut entries = t!(ar.entries());
    let mut a = t!(entries.next().await.unwrap());
    let mut s = String::new();
    t!(a.read_to_string(&mut s).await);
    assert_eq!(0x100_00c, s.len());
    assert_eq!(&s[..0xc], "0MB through\n");
    assert!(s[0xc..0x100_000].chars().all(|x| x == '\u{0}'));
    assert_eq!(&s[0x100_000..], "1MB through\n");
}

#[tokio::test]
async fn sparse_continuation_partial_record_is_rejected() {
    // `sparse_ext.txt` begins an extended sparse header at offset 2560. Its
    // first two sparse chunks are populated; make the first otherwise-empty
    // chunk contain either an offset or a length without its paired field.
    for field_offset in [0, 12] {
        let mut bytes = tar!("sparse.tar").to_vec();
        bytes[2560 + 2 * 24 + field_offset] = b'1';
        let mut ar = Archive::new(&bytes[..]);
        let mut entries = t!(ar.entries());

        assert!(entries.next().await.unwrap().is_ok());
        assert!(entries.next().await.unwrap().is_ok());
        assert!(matches!(entries.next().await, Some(Err(_))));
    }
}

#[tokio::test]
async fn path_separators() {
    let mut ar = Builder::new(Vec::new());
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());

    let path = td.path().join("test");
    t!(t!(File::create(&path).await).write_all(b"test").await);

    let short_path: PathBuf = std::iter::repeat_n("abcd", 2).collect();
    let long_path: PathBuf = std::iter::repeat_n("abcd", 50).collect();

    // Make sure UStar headers normalize to Unix path separators
    let mut header = Header::new_ustar();

    t!(header.set_path(&short_path));
    assert_eq!(t!(header.path()), short_path);
    assert!(!header.path_bytes().contains(&b'\\'));

    t!(header.set_path(&long_path));
    assert_eq!(t!(header.path()), long_path);
    assert!(!header.path_bytes().contains(&b'\\'));

    // Make sure GNU headers normalize to Unix path separators,
    // including the `@LongLink` fallback used by `append_file`.
    t!(ar
        .append_file(&short_path, &mut t!(File::open(&path).await))
        .await);
    t!(ar
        .append_file(&long_path, &mut t!(File::open(&path).await))
        .await);

    let rd = Cursor::new(t!(ar.into_inner().await));
    let mut ar = Archive::new(rd);
    let mut entries = t!(ar.entries());

    let entry = t!(entries.next().await.unwrap());
    assert_eq!(t!(entry.path()), short_path);
    assert!(!t!(entry.path_bytes()).contains(&b'\\'));

    let entry = t!(entries.next().await.unwrap());
    assert_eq!(t!(entry.path()), long_path);
    assert!(!t!(entry.path_bytes()).contains(&b'\\'));

    assert!(entries.next().await.is_none());
}

#[tokio::test]
#[cfg(unix)]
async fn append_path_symlink() {
    use std::{borrow::Cow, env, os::unix::fs::symlink};

    let mut ar = Builder::new(Vec::new());
    ar.follow_symlinks(false);
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());

    let long_linkname = "abcd".repeat(30);
    let long_pathname = "dcba".repeat(30);
    t!(env::set_current_dir(td.path()));
    // "short" path name / short link name
    t!(symlink("testdest", "test"));
    t!(ar.append_path("test").await);
    // short path name / long link name
    t!(symlink(&long_linkname, "test2"));
    t!(ar.append_path("test2").await);
    // long path name / long link name
    t!(symlink(&long_linkname, &long_pathname));
    t!(ar.append_path(&long_pathname).await);

    let rd = Cursor::new(t!(ar.into_inner().await));
    let mut ar = Archive::new(rd);
    let mut entries = t!(ar.entries());

    let entry = t!(entries.next().await.unwrap());
    assert_eq!(t!(entry.path()), Path::new("test"));
    assert_eq!(
        t!(entry.link_name()),
        Some(Cow::from(Path::new("testdest")))
    );
    assert_eq!(t!(entry.header().size()), 0);

    let entry = t!(entries.next().await.unwrap());
    assert_eq!(t!(entry.path()), Path::new("test2"));
    assert_eq!(
        t!(entry.link_name()),
        Some(Cow::from(Path::new(&long_linkname)))
    );
    assert_eq!(t!(entry.header().size()), 0);

    let entry = t!(entries.next().await.unwrap());
    assert_eq!(t!(entry.path()), Path::new(&long_pathname));
    assert_eq!(
        t!(entry.link_name()),
        Some(Cow::from(Path::new(&long_linkname)))
    );
    assert_eq!(t!(entry.header().size()), 0);

    assert!(entries.next().await.is_none());
}

#[tokio::test]
async fn name_with_slash_doesnt_fool_long_link_and_bsd_compat() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());

    let mut ar = Builder::new(Vec::new());

    let mut h = Header::new_gnu();
    t!(h.set_path("././@LongLink"));
    h.set_size(4);
    h.set_entry_type(EntryType::new(b'L'));
    h.set_cksum();
    t!(ar.append(&h, b"foo\0" as &[u8]).await);

    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Regular);
    t!(header.set_path("testdir/"));
    header.set_size(0);
    header.set_cksum();
    t!(ar.append(&header, &mut io::empty()).await);

    // Extracting
    let rdr = Cursor::new(t!(ar.into_inner().await));
    let mut ar = Archive::new(rdr);
    t!(ar.unpack(td.path()).await);

    // Iterating
    let rdr = Cursor::new(ar.into_inner().map_err(|_| ()).unwrap().into_inner());
    let mut ar = Archive::new(rdr);
    let mut entries = t!(ar.entries());
    while let Some(entry) = entries.next().await {
        assert!(entry.is_ok());
    }

    assert!(td.path().join("foo").is_file());
}

#[tokio::test]
async fn insert_local_file_different_name() {
    let mut ar = Builder::new(Vec::new());
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let path = td.path().join("directory");
    t!(fs::create_dir(&path).await);
    ar.append_path_with_name(&path, "archive/dir")
        .await
        .unwrap();
    let path = td.path().join("file");
    let mut file = t!(File::create(&path).await);
    t!(file.write_all(b"test").await);
    t!(file.flush().await);
    ar.append_path_with_name(&path, "archive/dir/f")
        .await
        .unwrap();

    let rd = Cursor::new(t!(ar.into_inner().await));
    let mut ar = Archive::new(rd);
    let mut entries = t!(ar.entries());
    let entry = t!(entries.next().await.unwrap());
    assert_eq!(t!(entry.path()), Path::new("archive/dir"));
    let entry = t!(entries.next().await.unwrap());
    assert_eq!(t!(entry.path()), Path::new("archive/dir/f"));
    assert!(entries.next().await.is_none());
}

#[tokio::test]
#[cfg(unix)]
async fn tar_directory_containing_symlink_to_directory() {
    use std::os::unix::fs::symlink;

    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let dummy_src = t!(TempBuilder::new().prefix("dummy_src").tempdir());
    let dummy_dst = td.path().join("dummy_dst");
    let mut ar = Builder::new(Vec::new());
    t!(symlink(dummy_src.path().display().to_string(), &dummy_dst));

    assert!(dummy_dst.read_link().is_ok());
    assert!(dummy_dst.read_link().unwrap().is_dir());
    ar.append_dir_all("symlinks", td.path()).await.unwrap();
    ar.finish().await.unwrap();
}

#[tokio::test]
async fn long_path() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let rdr = Cursor::new(tar!("7z_long_path.tar"));
    let mut ar = Archive::new(rdr);
    ar.unpack(td.path()).await.unwrap();
}

#[tokio::test]
async fn unpack_path_larger_than_windows_max_path() {
    let dir_name = "iamaprettylongnameandtobepreciseiam91characterslongwhichsomethinkisreallylongandothersdonot";
    // 183 character directory name
    let really_long_path = format!("{}{}", dir_name, dir_name);
    let td = t!(TempBuilder::new().prefix(&really_long_path).tempdir());
    // directory in 7z_long_path.tar is over 100 chars
    let rdr = Cursor::new(tar!("7z_long_path.tar"));
    let mut ar = Archive::new(rdr);
    // should unpack path greater than windows MAX_PATH length of 260 characters
    assert!(ar.unpack(td.path()).await.is_ok());
}

#[tokio::test]
async fn append_long_multibyte() {
    let mut x = Builder::new(Vec::new());
    let mut name = String::new();
    let data: &[u8] = &[];
    for _ in 0..512 {
        name.push('a');
        name.push('𑢮');
        x.append_data(&mut Header::new_gnu(), &name, data)
            .await
            .unwrap();
        name.pop();
    }
}

#[tokio::test]
async fn read_only_directory_containing_files() {
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());

    let mut b = Builder::new(Vec::<u8>::new());

    let mut h = Header::new_gnu();
    t!(h.set_path("dir/"));
    h.set_size(0);
    h.set_entry_type(EntryType::dir());
    h.set_mode(0o444);
    h.set_cksum();
    t!(b.append(&h, "".as_bytes()).await);

    let mut h = Header::new_gnu();
    t!(h.set_path("dir/file"));
    h.set_size(2);
    h.set_entry_type(EntryType::file());
    h.set_cksum();
    t!(b.append(&h, "hi".as_bytes()).await);

    let contents = t!(b.into_inner().await);
    let mut ar = Archive::new(&contents[..]);
    assert!(ar.unpack(td.path()).await.is_ok());
}

// This test was marked linux only due to macOS CI can't handle `set_current_dir` correctly
#[tokio::test]
#[cfg(target_os = "linux")]
async fn tar_directory_containing_special_files() {
    use std::env;
    use std::ffi::CString;

    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());
    let fifo = td.path().join("fifo");

    unsafe {
        let fifo_path = t!(CString::new(fifo.to_str().unwrap()));
        let ret = libc::mknod(fifo_path.as_ptr(), libc::S_IFIFO | 0o644, 0);
        if ret != 0 {
            libc::perror(fifo_path.as_ptr());
            panic!("Failed to create a FIFO file");
        }
    }

    t!(env::set_current_dir(td.path()));
    let mut ar = Builder::new(Vec::new());
    // append_path has a different logic for processing files, so we need to test it as well
    t!(ar.append_path("fifo").await);
    t!(ar.append_dir_all("special", td.path()).await);
    t!(env::set_current_dir("/dev/"));
    // CI systems seem to have issues with creating a chr device
    t!(ar.append_path("null").await);
    t!(ar.finish().await);
}

#[tokio::test]
async fn header_size_overflow() {
    // maximal file size doesn't overflow anything
    let mut ar = Builder::new(Vec::new());
    let mut header = Header::new_gnu();
    header.set_size(u64::MAX);
    header.set_cksum();
    t!(ar.append(&header, "x".as_bytes()).await);
    let result = t!(ar.into_inner().await);
    let mut ar = Archive::new(&result[..]);
    let mut e = t!(ar.entries());
    let entry = e.next().await.unwrap();
    assert!(entry.is_err(), "expected error for size overflow");
    let err = entry.unwrap_err();
    assert!(
        err.to_string().contains("size overflow"),
        "bad error: {}",
        err
    );

    // back-to-back entries that would overflow also don't panic
    let mut ar = Builder::new(Vec::new());
    let mut header = Header::new_gnu();
    header.set_size(1_000);
    header.set_cksum();
    t!(ar.append(&header, &[0u8; 1_000][..]).await);
    let mut header = Header::new_gnu();
    header.set_size(u64::MAX - 513);
    header.set_cksum();
    t!(ar.append(&header, "x".as_bytes()).await);
    let result = t!(ar.into_inner().await);
    let mut ar = Archive::new(&result[..]);
    let mut e = t!(ar.entries());
    let first = e.next().await.unwrap();
    t!(first); // First entry should be ok
    let second = e.next().await.unwrap();
    assert!(second.is_err(), "expected error for size overflow");
    let err = second.unwrap_err();
    assert!(
        err.to_string().contains("size overflow"),
        "bad error: {}",
        err
    );
}

#[tokio::test]
async fn pax_hidden_entry() {
    let bytes = tar!("pax-hidden-entry.tar");
    let mut archive = Archive::new(bytes);

    // This archive has three entries: file_a, file_b, and hidden. One of these
    // entries (imaginatively called `hidden`) was invisible in older versions
    // of tokio-tar.
    let entries = t!(archive.entries())
        .map(|entry_result| {
            let entry = t!(entry_result);
            let path = t!(entry.path());
            path.to_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>()
        .await;

    assert_eq!(
        vec![
            String::from("file_a"),
            String::from("file_b"),
            String::from("hidden"),
        ],
        entries
    );
}

#[tokio::test]
async fn pax_phantom_entry() {
    let bytes = tar!("pax-phantom-entry.tar");
    let mut archive = Archive::new(bytes);

    // This archive has two entries: file_a, and file_b. Older versions of
    // tokio-tar will see a third entry called `phantom`, however, due to a bug
    // in PAX state handling.
    let entries = t!(archive.entries())
        .map(|entry_result| {
            let entry = t!(entry_result);
            let path = t!(entry.path());
            path.to_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>()
        .await;

    assert_eq!(
        vec![String::from("file_a"), String::from("file_b")],
        entries
    );
}

#[tokio::test]
async fn pax_size_does_not_apply_to_extension_headers() {
    // This archive is ordered as `x (PAX) -> L (GNU longname) -> file_a -> file_b`,
    // where the PAX `x` record declares `size=2048`. If that `size=` is wrongly
    // applied to the intermediary `L` header, the parser advances the cursor by
    // 2048 bytes after `L` instead of by `L`'s true 12-byte payload, lands in
    // the middle of `file_a`'s body, and bails out with a checksum mismatch —
    // hiding both `file_a` and `file_b` from the entry stream. Correct parsing
    // applies PAX only to the next *file* entry, so the L longname renames
    // `file_a` to `longname.txt` and the stream yields ["longname.txt", "file_b"].
    let bytes = tar!("pax-overrides-extension-header.tar");
    let mut archive = Archive::new(bytes);

    let entries = t!(archive.entries())
        .map(|entry_result| {
            let entry = t!(entry_result);
            let path = t!(entry.path());
            path.to_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>()
        .await;

    assert_eq!(
        vec![String::from("longname.txt"), String::from("file_b")],
        entries
    );
}

#[tokio::test]
async fn pax_path_and_gnu_longname_are_rejected() {
    let mut builder = Builder::new(Vec::new());

    let pax = b"17 path=pax-name\n";
    let mut header = Header::new_ustar();
    t!(header.set_path("PaxHeaders/x"));
    header.set_entry_type(EntryType::new(b'x'));
    header.set_size(pax.len() as u64);
    header.set_cksum();
    t!(builder.append(&header, &pax[..]).await);

    let mut header = Header::new_gnu();
    t!(header.set_path("././@LongLink"));
    header.set_entry_type(EntryType::new(b'L'));
    header.set_size(9);
    header.set_cksum();
    t!(builder.append(&header, b"gnu-name\0" as &[u8]).await);

    let mut header = Header::new_gnu();
    t!(header.set_path("raw-name"));
    header.set_size(0);
    header.set_cksum();
    t!(builder.append(&header, &[][..]).await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let err = t!(archive.entries())
        .next()
        .await
        .unwrap()
        .expect_err("expected competing path extensions to be rejected");
    assert_eq!(
        err.to_string(),
        "ambiguous path: pax path and GNU longname describe the same member"
    );
}

#[tokio::test]
async fn gnu_longname_and_later_pax_path_are_rejected() {
    let mut builder = Builder::new(Vec::new());

    let mut header = Header::new_gnu();
    t!(header.set_path("././@LongLink"));
    header.set_entry_type(EntryType::new(b'L'));
    header.set_size(9);
    header.set_cksum();
    t!(builder.append(&header, b"gnu-name\0" as &[u8]).await);

    let pax = b"17 path=pax-name\n";
    let mut header = Header::new_ustar();
    t!(header.set_path("PaxHeaders/x"));
    header.set_entry_type(EntryType::new(b'x'));
    header.set_size(pax.len() as u64);
    header.set_cksum();
    t!(builder.append(&header, &pax[..]).await);

    let mut header = Header::new_gnu();
    t!(header.set_path("raw-name"));
    header.set_size(0);
    header.set_cksum();
    t!(builder.append(&header, &[][..]).await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let err = t!(archive.entries())
        .next()
        .await
        .unwrap()
        .expect_err("expected competing path extensions to be rejected");
    assert_eq!(
        err.to_string(),
        "ambiguous path: pax path and GNU longname describe the same member"
    );
}

#[tokio::test]
async fn pax_linkpath_and_gnu_longlink_are_rejected() {
    let mut builder = Builder::new(Vec::new());

    let pax = b"23 linkpath=pax-target\n";
    let mut header = Header::new_ustar();
    t!(header.set_path("PaxHeaders/x"));
    header.set_entry_type(EntryType::new(b'x'));
    header.set_size(pax.len() as u64);
    header.set_cksum();
    t!(builder.append(&header, &pax[..]).await);

    let mut header = Header::new_gnu();
    t!(header.set_path("././@LongLink"));
    header.set_entry_type(EntryType::new(b'K'));
    header.set_size(11);
    header.set_cksum();
    t!(builder.append(&header, b"gnu-target\0" as &[u8]).await);

    let mut header = Header::new_gnu();
    t!(header.set_path("symlink"));
    header.set_entry_type(EntryType::symlink());
    t!(header.set_link_name("raw-target"));
    header.set_size(0);
    header.set_cksum();
    t!(builder.append(&header, &[][..]).await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let err = t!(archive.entries())
        .next()
        .await
        .unwrap()
        .expect_err("expected competing link target extensions to be rejected");
    assert_eq!(
        err.to_string(),
        "ambiguous link target: pax linkpath and GNU longlink describe the same member"
    );
}

#[tokio::test]
async fn last_pax_linkpath_wins() {
    let mut builder = Builder::new(Vec::new());

    let pax = b"22 linkpath=pax-first\n21 linkpath=pax-last\n";
    let mut header = Header::new_ustar();
    t!(header.set_path("PaxHeaders/x"));
    header.set_entry_type(EntryType::new(b'x'));
    header.set_size(pax.len() as u64);
    header.set_cksum();
    t!(builder.append(&header, &pax[..]).await);

    let mut header = Header::new_gnu();
    t!(header.set_path("symlink"));
    header.set_entry_type(EntryType::symlink());
    t!(header.set_link_name("raw-target"));
    header.set_size(0);
    header.set_cksum();
    t!(builder.append(&header, &[][..]).await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let entry = t!(t!(archive.entries()).next().await.unwrap());
    assert_eq!(&*t!(entry.link_name_bytes()).unwrap(), b"pax-last");
}

#[tokio::test]
async fn gnu_longlink_and_later_pax_linkpath_are_rejected() {
    let mut builder = Builder::new(Vec::new());

    let mut header = Header::new_gnu();
    t!(header.set_path("././@LongLink"));
    header.set_entry_type(EntryType::new(b'K'));
    header.set_size(11);
    header.set_cksum();
    t!(builder.append(&header, b"gnu-target\0" as &[u8]).await);

    let pax = b"23 linkpath=pax-target\n";
    let mut header = Header::new_ustar();
    t!(header.set_path("PaxHeaders/x"));
    header.set_entry_type(EntryType::new(b'x'));
    header.set_size(pax.len() as u64);
    header.set_cksum();
    t!(builder.append(&header, &pax[..]).await);

    let mut header = Header::new_gnu();
    t!(header.set_path("symlink"));
    header.set_entry_type(EntryType::symlink());
    t!(header.set_link_name("raw-target"));
    header.set_size(0);
    header.set_cksum();
    t!(builder.append(&header, &[][..]).await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let err = t!(archive.entries())
        .next()
        .await
        .unwrap()
        .expect_err("expected competing link target extensions to be rejected");
    assert_eq!(
        err.to_string(),
        "ambiguous link target: pax linkpath and GNU longlink describe the same member"
    );
}

#[tokio::test]
async fn gnu_long_pathname_stops_at_first_nul() {
    let mut builder = Builder::new(Vec::new());

    let mut header = Header::new_gnu();
    t!(header.set_path("././@LongLink"));
    header.set_entry_type(EntryType::new(b'L'));
    header.set_size(11);
    header.set_cksum();
    t!(builder.append(&header, b"name\0hidden" as &[u8]).await);

    let mut header = Header::new_gnu();
    t!(header.set_path("raw-name"));
    header.set_size(0);
    header.set_cksum();
    t!(builder.append(&header, &[][..]).await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let entry = t!(t!(archive.entries()).next().await.unwrap());

    assert_eq!(&*t!(entry.path_bytes()), b"name");
}

#[tokio::test]
async fn gnu_long_linkname_stops_at_first_nul() {
    let mut builder = Builder::new(Vec::new());

    let mut header = Header::new_gnu();
    t!(header.set_path("././@LongLink"));
    header.set_entry_type(EntryType::new(b'K'));
    header.set_size(13);
    header.set_cksum();
    t!(builder.append(&header, b"target\0hidden" as &[u8]).await);

    let mut header = Header::new_gnu();
    t!(header.set_path("link"));
    header.set_size(0);
    header.set_cksum();
    t!(builder.append(&header, &[][..]).await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let entry = t!(t!(archive.entries()).next().await.unwrap());

    assert_eq!(&*t!(entry.link_name_bytes()).unwrap(), b"target");
}

#[tokio::test]
async fn empty_read_does_not_report_eof_for_nonempty_entry() {
    let mut builder = Builder::new(Vec::<u8>::new());
    let mut header = Header::new_gnu();
    t!(header.set_path("body"));
    header.set_size(4);
    header.set_cksum();
    t!(builder.append(&header, &b"data"[..]).await);

    let contents = t!(builder.into_inner().await);
    let mut archive = Archive::new(&contents[..]);
    let mut entries = t!(archive.entries());
    let mut entry = t!(entries.next().await.unwrap());

    let mut empty = [];
    assert_eq!(t!(entry.read(&mut empty).await), 0);

    let mut body = Vec::new();
    t!(entry.read_to_end(&mut body).await);
    assert_eq!(body, b"data");
}

#[tokio::test]
async fn truncated_entry_reports_eof_while_reading_body() {
    let bytes = tar!("diff-039-truncated-entry-body.tar");
    let mut archive = Archive::new(bytes);
    let mut entries = t!(archive.entries());
    let mut entry = t!(entries.next().await.unwrap());

    assert_eq!(&*t!(entry.path_bytes()), b"aaa");
    let mut body = Vec::new();
    let err = entry.read_to_end(&mut body).await.unwrap_err();
    assert!(body.is_empty());
    assert!(
        err.to_string()
            .contains("unexpected EOF while reading archive entry data"),
        "bad error: {}",
        err
    );
}

#[tokio::test]
async fn nul_version_ustar_headers_are_rejected() {
    let mut builder = Builder::new(Vec::new());

    let mut header = Header::new_ustar();
    t!(header.set_path("nul-version-owner"));
    t!(header.set_username("user"));
    t!(header.set_groupname("group"));
    t!(header.set_device_major(1));
    t!(header.set_device_minor(2));
    header.as_ustar_mut().unwrap().version = [0, 0];
    header.set_size(0);
    header.set_cksum();
    t!(builder.append(&header, &[][..]).await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let err = t!(archive.entries()).next().await.unwrap().unwrap_err();
    assert_eq!(
        err.to_string(),
        "NUL-version USTAR header is ambiguous and not supported"
    );
}

#[tokio::test]
async fn extension_typeflags_on_old_headers_are_rejected() {
    for (entry_type, data) in [
        (EntryType::new(b'x'), b"17 path=pax-name\n".as_slice()),
        (EntryType::GNULongName, b"long-name\0".as_slice()),
        (EntryType::GNULongLink, b"long-link\0".as_slice()),
    ] {
        let mut builder = Builder::new(Vec::new());
        let mut header = Header::new_old();
        t!(header.set_path("ambiguous"));
        header.set_entry_type(entry_type);
        header.set_size(data.len() as u64);
        header.set_cksum();
        t!(builder.append(&header, data).await);

        let bytes = t!(builder.into_inner().await);
        let mut archive = Archive::new(&bytes[..]);
        let err = t!(archive.entries()).next().await.unwrap().unwrap_err();

        assert_eq!(
            err.to_string(),
            "extension typeflag is not permitted on an unrecognized header"
        );
    }
}

#[tokio::test]
async fn pax_typeflag_on_gnu_header_is_rejected() {
    let pax = b"17 path=pax-name\n";
    let mut builder = Builder::new(Vec::new());
    let mut header = Header::new_gnu();
    t!(header.set_path("ambiguous"));
    header.set_entry_type(EntryType::new(b'x'));
    header.set_size(pax.len() as u64);
    header.set_cksum();
    t!(builder.append(&header, &pax[..]).await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let err = t!(archive.entries()).next().await.unwrap().unwrap_err();

    assert_eq!(
        err.to_string(),
        "extension typeflag is not permitted on an unrecognized header"
    );
}

#[tokio::test]
async fn duplicate_pax_paths_use_last_value() {
    let mut builder = Builder::new(Vec::new());

    let pax = b"19 path=first-name\n20 path=second-name\n";
    let mut header = Header::new_ustar();
    t!(header.set_path("PaxHeaders/x"));
    header.set_entry_type(EntryType::new(b'x'));
    header.set_size(pax.len() as u64);
    header.set_cksum();
    t!(builder.append(&header, &pax[..]).await);

    let mut header = Header::new_ustar();
    t!(header.set_path("raw-name"));
    header.set_size(0);
    header.set_cksum();
    t!(builder.append(&header, &[][..]).await);

    let bytes = t!(builder.into_inner().await);
    let mut archive = Archive::new(&bytes[..]);
    let entry = t!(t!(archive.entries()).next().await.unwrap());
    assert_eq!(&*t!(entry.path_bytes()), b"second-name");
}

#[tokio::test]
async fn extended_sparse_data_position_skips_extension_headers() {
    let mut archive = Archive::new(Cursor::new(tar!("sparse.tar")));
    let mut entries = t!(archive.entries());

    t!(entries.next().await.unwrap());
    t!(entries.next().await.unwrap());

    let entry = t!(entries.next().await.unwrap());
    assert_eq!(&*entry.header().path_bytes(), b"sparse_ext.txt");
    assert_eq!(entry.raw_file_position(), 3072);
}
