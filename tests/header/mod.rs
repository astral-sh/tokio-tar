use std::{mem, path::Path};

use async_tar::{GnuHeader, Header, OldHeader, UstarHeader};

fn gnu_header() -> GnuHeader {
    let mut header: GnuHeader = unsafe { mem::zeroed() };
    header.magic = *b"ustar ";
    header.version = *b" \0";
    header.mtime = *b"00000000000\0";
    header
}

fn old_header() -> OldHeader {
    let mut header: OldHeader = unsafe { mem::zeroed() };
    header.mtime = *b"00000000000\0";
    header
}

fn ustar_header() -> UstarHeader {
    let mut header: UstarHeader = unsafe { mem::zeroed() };
    header.magic = *b"ustar\0";
    header.version = *b"00";
    header.mtime = *b"00000000000\0";
    header
}

#[test]
fn goto_gnu() {
    let h = gnu_header();
    assert!(h.as_header().as_gnu().is_some());
    assert!(h.as_header().as_ustar().is_none());
}

#[test]
fn goto_old() {
    let h = old_header();
    assert!(h.as_header().as_gnu().is_none());
    assert!(h.as_header().as_ustar().is_none());
}

#[test]
fn goto_ustar() {
    let h = ustar_header();
    assert!(h.as_header().as_gnu().is_none());
    assert!(h.as_header().as_ustar().is_some());
}

#[test]
fn link_name() {
    let mut h = ustar_header();
    h.linkname[..10].copy_from_slice(b"../foo/bar");
    assert_eq!(
        t!(h.as_header().link_name()).unwrap().to_str(),
        Some("../foo/bar")
    );

    h.linkname = [0; 100];
    h.linkname[..8].copy_from_slice(b"foo\\bar\0");
    assert_eq!(
        t!(h.as_header().link_name()).unwrap().to_str(),
        Some("foo\\bar")
    );
}

#[test]
fn mtime() {
    assert_eq!(t!(gnu_header().as_header().mtime()), 0);
    assert_eq!(t!(ustar_header().as_header().mtime()), 0);
    assert_eq!(t!(old_header().as_header().mtime()), 0);
}

#[test]
fn user_and_group_name() {
    let mut h = gnu_header();
    h.uname[..3].copy_from_slice(b"foo");
    h.gname[..3].copy_from_slice(b"bar");
    assert_eq!(t!(h.as_header().username()), Some("foo"));
    assert_eq!(t!(h.as_header().groupname()), Some("bar"));

    let mut h = ustar_header();
    h.uname[..3].copy_from_slice(b"foo");
    h.gname[..3].copy_from_slice(b"bar");
    assert_eq!(t!(h.as_header().username()), Some("foo"));
    assert_eq!(t!(h.as_header().groupname()), Some("bar"));

    let h = old_header();
    assert_eq!(t!(h.as_header().username()), None);
    assert_eq!(t!(h.as_header().groupname()), None);
}

#[test]
fn dev_major_minor() {
    let mut h = ustar_header();
    h.dev_major = *b"0000001\0";
    h.dev_minor = *b"0000002\0";
    assert_eq!(t!(h.as_header().device_major()), Some(1));
    assert_eq!(t!(h.as_header().device_minor()), Some(2));

    h.dev_major[0] = 0x7f;
    h.dev_minor[0] = b'g';
    assert!(h.as_header().device_major().is_err());
    assert!(h.as_header().device_minor().is_err());

    let h = old_header();
    assert_eq!(t!(h.as_header().device_major()), None);
    assert_eq!(t!(h.as_header().device_minor()), None);
}

#[test]
fn path() {
    let mut h = ustar_header();
    h.name[..3].copy_from_slice(b"bar");
    h.prefix[..3].copy_from_slice(b"foo");
    assert_eq!(t!(h.as_header().path()), Path::new("foo/bar"));

    h.name = [0; 100];
    h.prefix = [0; 155];
    h.name[..7].copy_from_slice(b"foo\\bar");
    assert_eq!(t!(h.as_header().path()), Path::new("foo\\bar"));
}

#[test]
fn extended_numeric_format_reading() {
    let mut h = ustar_header();
    h.size = *b"00000000052\0";
    h.gid = *b"0000052\0";
    assert_eq!(t!(h.as_header().size()), 42);
    assert_eq!(t!(h.as_header().gid()), 42);

    let mut h = gnu_header();
    h.size = [0x80, 0, 0, 0, 0, 0, 0, 0x02, 0, 0, 0, 0];
    assert_eq!(h.as_header().entry_size().unwrap(), 0x0200000000);
    h.uid = [0x80, 0x00, 0x00, 0x00, 0x12, 0x34, 0x56, 0x78];
    assert_eq!(h.as_header().uid().unwrap(), 0x12345678);
    h.mtime = [
        0x80, 0, 0, 0, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef,
    ];
    assert_eq!(h.as_header().mtime().unwrap(), 0x0123456789abcdef);
    h.realsize = [0x80, 0, 0, 0, 0, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde];
    assert_eq!(h.real_size().unwrap(), 0x00123456789abcde);
    h.sparse[0].offset = [0x80, 0, 0, 0, 0, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd];
    assert_eq!(h.sparse[0].offset().unwrap(), 0x000123456789abcd);
    h.sparse[0].numbytes = [0x80, 0, 0, 0, 0, 0, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc];
    assert_eq!(h.sparse[0].length().unwrap(), 0x0000123456789abc);
}

#[test]
fn byte_slice_conversion() {
    let h = ustar_header();
    let b = h.as_header().as_bytes();
    let b_conv = Header::from_byte_slice(b).as_bytes();
    assert_eq!(b, b_conv);
}
