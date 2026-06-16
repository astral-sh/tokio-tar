use std::str;

use tokio::io;

use crate::other;

/// An iterator over the pax extensions in an archive entry.
///
/// This iterator yields structures which can themselves be parsed into
/// key/value pairs.
pub struct PaxExtensions<'entry> {
    data: &'entry [u8],
}

/// A key/value pair corresponding to a pax extension.
pub struct PaxExtension<'entry> {
    key: &'entry [u8],
    value: &'entry [u8],
}

pub fn pax_extensions(a: &[u8]) -> PaxExtensions<'_> {
    // Empty PAX bodies apply no extensions and are accepted by CPython,
    // Go, and libarchive despite POSIX's one-or-more-records wording.
    PaxExtensions { data: a }
}

impl<'entry> Iterator for PaxExtensions<'entry> {
    type Item = io::Result<PaxExtension<'entry>>;

    fn next(&mut self) -> Option<io::Result<PaxExtension<'entry>>> {
        if self.data.is_empty() {
            return None;
        }

        let data = self.data;
        let parsed = data
            .iter()
            .position(|b| *b == b' ')
            .and_then(|length_end| {
                str::from_utf8(&data[..length_end])
                    .ok()
                    .and_then(|length| length.parse::<usize>().ok())
                    .map(|length| (length_end + 1, length))
            })
            .and_then(|(kvstart, record_len)| {
                let record = data.get(..record_len)?;
                let value_end = record_len.checked_sub(1)?;
                if record.last() != Some(&b'\n') {
                    return None;
                }

                record
                    .get(kvstart..value_end)?
                    .iter()
                    .position(|b| *b == b'=')
                    .map(|equals| (record_len, kvstart, kvstart + equals, value_end))
            });

        let Some((record_len, kvstart, equals, value_end)) = parsed else {
            self.data = &[];
            return Some(Err(other("malformed pax extension")));
        };

        self.data = &data[record_len..];
        Some(Ok(PaxExtension {
            key: &data[kvstart..equals],
            value: &data[equals + 1..value_end],
        }))
    }
}

impl<'entry> PaxExtension<'entry> {
    /// Returns the key for this key/value pair parsed as a string.
    ///
    /// May fail if the key isn't actually utf-8.
    pub fn key(&self) -> Result<&'entry str, str::Utf8Error> {
        str::from_utf8(self.key)
    }

    /// Returns the underlying raw bytes for the key of this key/value pair.
    pub fn key_bytes(&self) -> &'entry [u8] {
        self.key
    }

    /// Returns the value for this key/value pair parsed as a string.
    ///
    /// May fail if the value isn't actually utf-8.
    pub fn value(&self) -> Result<&'entry str, str::Utf8Error> {
        str::from_utf8(self.value)
    }

    /// Returns the underlying raw bytes for this value of this key/value pair.
    pub fn value_bytes(&self) -> &'entry [u8] {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::pax_extensions;

    #[test]
    fn values_can_contain_newlines() {
        let value = b"line one\nline two";
        let data = pax_record("SCHILY.xattr.user.comment", value);
        let mut extensions = pax_extensions(&data);

        let extension = extensions.next().unwrap().unwrap();
        assert_eq!(extension.key_bytes(), b"SCHILY.xattr.user.comment");
        assert_eq!(extension.value_bytes(), value);
        assert!(extensions.next().is_none());
    }

    fn pax_record(key: &str, value: &[u8]) -> Vec<u8> {
        let mut len = key.len() + value.len() + 3;

        loop {
            let prefix = format!("{len} {key}=");
            let record_len = prefix.len() + value.len() + 1;
            if record_len == len {
                let mut record = prefix.into_bytes();
                record.extend_from_slice(value);
                record.push(b'\n');
                return record;
            }
            len = record_len;
        }
    }
}
