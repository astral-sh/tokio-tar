# Changelog

## 0.5.6

* Fixed a parser desynchronization vulnerability when reading tar archives that
  contain mismatched size information in PAX/ustar headers.

    This vulnerability is being tracked as GHSA-j5gw-2vrg-8fgx
    and CVE-2025-62518.

## 0.5.5

* This is a corrective release for 0.5.4 to fix a debugging artifact that
  was accidentally left in the release.

## 0.5.4

* Fixed a path traversal vulnerability when using the `unpack_in_raw` API
  by @charliermarsh

    This vulnerability is being tracked as GHSA-3wgq-wrwc-vqmv.

## 0.5.3

* Expose `TarError` publicly by @konstin in https://github.com/astral-sh/tokio-tar/pull/52

## 0.5.2

* Enable opt-in to deny creation of symlinks outside target directory by @charliermarsh in https://github.com/astral-sh/tokio-tar/pull/46

## 0.5.1

* Add test to reproduce issue in `impl Stream for Entries` causing filename truncation by @charliermarsh in https://github.com/astral-sh/tokio-tar/pull/41
* Avoid truncation during pending reads by @charliermarsh in https://github.com/astral-sh/tokio-tar/pull/40

## 0.5.0

* Setting `preserve_permissions` to `false` will avoid setting _any_ permissions on extracted files.
  In [`alexcrichton/tar-rs`](https://github.com/alexcrichton/tar-rs), setting `preserve_permissions`
  to `false` will still set read, write, and execute permissions on extracted files, but will avoid
  setting extended permissions (e.g., `setuid`, `setgid`, and `sticky` bits).
* Avoid creating directories outside the unpack target (see: [`alexcrichton/tar-rs#259`](https://github.com/alexcrichton/tar-rs/pull/259)).
* Added `unpack_in_raw` which memoizes the set of validated paths (and assumes a pre-canonicalized)
  unpack target to avoid redundant filesystem operations.
