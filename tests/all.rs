extern crate tokio_tar as async_tar;

extern crate filetime;
extern crate tempfile;
#[cfg(all(unix, feature = "xattr"))]
extern crate xattr;

use std::{
    io::Cursor,
    path::{Path, PathBuf},
};
use tokio::{
    fs::{self, File},
    io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
};
use tokio_stream::*;

use async_tar::{Archive, ArchiveBuilder, Builder, EntryType, Header};
use filetime::FileTime;
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
            names.push(t!(::std::str::from_utf8(&e.path_bytes())).to_string());
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
    assert_eq!(&*f.path_bytes(), too_long.as_bytes());
    assert_eq!(f.header().size().unwrap(), 4);
    let mut s = String::new();
    t!(f.read_to_string(&mut s).await);
    assert_eq!(s, "test");

    // The long entry added with `append_data`
    let mut f = entries.next().await.unwrap().unwrap();
    assert!(f.header().path_bytes().len() < too_long.len());
    assert_eq!(&*f.path_bytes(), too_long.as_bytes());
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
    assert_eq!(&*f.path_bytes(), long_name_with_dot_dot.as_bytes());
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
    let mtime = FileTime::from_last_modification_time(&meta);
    let atime = FileTime::from_last_access_time(&meta);
    assert_eq!(mtime.unix_seconds(), 1_000_000_000);
    assert_eq!(mtime.nanoseconds(), 0);
    assert_eq!(atime.unix_seconds(), 1_000_000_000);
    assert_eq!(atime.nanoseconds(), 0);
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

    let mtime = FileTime::from_last_modification_time(&md);
    assert_eq!(mtime.unix_seconds(), 1448291033);

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
    assert_eq!(&*e.path_bytes(), b"foo");
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
    assert_eq!(&*e.link_name_bytes().unwrap(), b"foo");
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
async fn path_separators() {
    let mut ar = Builder::new(Vec::new());
    let td = t!(TempBuilder::new().prefix("async-tar").tempdir());

    let path = td.path().join("test");
    t!(t!(File::create(&path).await).write_all(b"test").await);

    let short_path: PathBuf = std::iter::repeat("abcd").take(2).collect();
    let long_path: PathBuf = std::iter::repeat("abcd").take(50).collect();

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
    assert!(!entry.path_bytes().contains(&b'\\'));

    let entry = t!(entries.next().await.unwrap());
    assert_eq!(t!(entry.path()), long_path);
    assert!(!entry.path_bytes().contains(&b'\\'));

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
