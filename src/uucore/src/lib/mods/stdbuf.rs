// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

//! Honour the stdout buffering mode requested with `stdbuf -o`.
//!
//! `stdbuf` passes the requested mode in the `_STDBUF_O` environment variable
//! to a preloaded library that adjusts C stdio. Rust's stdout does not go
//! through C stdio, so utilities that put a buffer in front of stdout use the
//! helpers here to size it.

use std::ffi::OsStr;
use std::io::{BufWriter, Write};

/// Capacity for a buffer placed in front of stdout.
///
/// - `L` (line buffered) and `0` (unbuffered) give `0`: every write then goes
///   straight to stdout's own line buffer, so each complete line is written
///   as soon as it is produced;
/// - a size gives a buffer of that many bytes;
/// - otherwise (no request, or an unrecognised one) `default` is returned.
pub fn stdout_buffer_capacity(default: usize) -> usize {
    requested_capacity().unwrap_or(default)
}

/// Wrap `inner`, which must write to stdout, in a [`BufWriter`] sized by
/// [`stdout_buffer_capacity`]. Without a `stdbuf` request this is
/// [`BufWriter::new`].
pub fn stdout_buf_writer<W: Write>(inner: W) -> BufWriter<W> {
    match requested_capacity() {
        Some(capacity) => BufWriter::with_capacity(capacity, inner),
        None => BufWriter::new(inner),
    }
}

fn requested_capacity() -> Option<usize> {
    parse_mode(std::env::var_os("_STDBUF_O").as_deref())
}

fn parse_mode(value: Option<&OsStr>) -> Option<usize> {
    match value?.to_str()? {
        "L" => Some(0),
        size => size.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_mode;
    use std::ffi::OsStr;

    #[test]
    fn modes() {
        assert_eq!(parse_mode(None), None);
        assert_eq!(parse_mode(Some(OsStr::new("L"))), Some(0));
        assert_eq!(parse_mode(Some(OsStr::new("0"))), Some(0));
        assert_eq!(parse_mode(Some(OsStr::new("4096"))), Some(4096));
        assert_eq!(parse_mode(Some(OsStr::new("l"))), None);
    }
}
