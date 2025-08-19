# Changelog

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
