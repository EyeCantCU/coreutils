// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

//! Honour the stdout buffering mode requested with `stdbuf -o`.
//!
//! `stdbuf` passes it in `_STDBUF_O` to a preloaded library that adjusts C stdio,
//! which Rust's stdout bypasses.

use std::ffi::OsStr;
use std::io::{BufWriter, Write};

/// Capacity for a buffer in front of stdout: `0` for `L` or `0`, a given size, else `default`.
///
/// With `0`, stdout's own line buffer writes each line as soon as it is produced.
pub fn stdout_buffer_capacity(default: usize) -> usize {
    requested_capacity().unwrap_or(default)
}

/// Wrap `inner` (a stdout writer) in a [`BufWriter`] sized by [`stdout_buffer_capacity`].
pub fn stdout_buf_writer<W: Write>(inner: W) -> BufWriter<W> {
    match requested_capacity() {
        Some(capacity) => BufWriter::with_capacity(capacity, inner),
        None => BufWriter::new(inner),
    }
}

fn requested_capacity() -> Option<usize> {
    let capacity = parse_mode(std::env::var_os("_STDBUF_O").as_deref())?;
    checked_capacity(capacity)
}

/// `capacity`, unless a buffer of that size cannot be allocated: then warn and use the default.
fn checked_capacity(capacity: usize) -> Option<usize> {
    if Vec::<u8>::new().try_reserve_exact(capacity).is_ok() {
        Some(capacity)
    } else {
        eprintln!("failed to allocate a {capacity} byte stdio buffer");
        None
    }
}

fn parse_mode(value: Option<&OsStr>) -> Option<usize> {
    match value?.to_str()? {
        "L" => Some(0),
        size => size.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::{checked_capacity, parse_mode};
    use std::ffi::OsStr;

    #[test]
    fn modes() {
        assert_eq!(parse_mode(None), None);
        assert_eq!(parse_mode(Some(OsStr::new("L"))), Some(0));
        assert_eq!(parse_mode(Some(OsStr::new("0"))), Some(0));
        assert_eq!(parse_mode(Some(OsStr::new("4096"))), Some(4096));
        assert_eq!(parse_mode(Some(OsStr::new("l"))), None);
    }

    #[test]
    fn unallocatable_size_falls_back() {
        assert_eq!(checked_capacity(4096), Some(4096));
        assert_eq!(checked_capacity(usize::MAX), None);
    }
}
