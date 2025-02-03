# `astral-tokio-tar`

A `tokio`-based tar archive reader and writer.

## Provenance

This crate is a fork of [`edera-dev/tokio-tar`](https://github.com/edera-dev/tokio-tar),
which was a fork of [`vorot93/tokio-tar`](https://github.com/vorot93/tokio-tar),
which was a fork of [`dignifiedquire/async-tar`](https://github.com/dignifiedquire/async-tar),
which is based on [`alexcrichton/tar-rs`](https://github.com/alexcrichton/tar-rs).

As compared to the async tar crates, this crate includes a variety of performance improvements
and missing patches from [`alexcrichton/tar-rs`](https://github.com/alexcrichton/tar-rs).

As compared to [`alexcrichton/tar-rs`](https://github.com/alexcrichton/tar-rs), this crate features
the following modifications:

- Setting `preserve_permissions` to `false` will avoid setting _any_ permissions on extracted files.
  In [`alexcrichton/tar-rs`](https://github.com/alexcrichton/tar-rs), setting `preserve_permissions`
  to `false` will still set read, write, and execute permissions on extracted files, but will avoid
  setting extended permissions (e.g., `setuid`, `setgid`, and `sticky` bits).

## License

This project is licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
