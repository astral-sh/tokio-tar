# Changelog

## 0.5.0

- Setting `preserve_permissions` to `false` will avoid setting _any_ permissions on extracted files.
  In [`alexcrichton/tar-rs`](https://github.com/alexcrichton/tar-rs), setting `preserve_permissions`
  to `false` will still set read, write, and execute permissions on extracted files, but will avoid
  setting extended permissions (e.g., `setuid`, `setgid`, and `sticky` bits).
- Ban unpacking files, directories or references that are or point to locations outside the unpack
  directory.
- Added `unpack_in_memo` which memoizes the set of validated paths to avoid redundant filesystem
  operations.
