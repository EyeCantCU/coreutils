// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use std::cmp::max;

#[derive(Debug, Default, Copy, Clone)]
pub struct WordCount {
    pub bytes: usize,
    pub chars: usize,
    pub lines: usize,
    pub words: usize,
    pub max_line_length: usize,
}

/// Records which counters of a running total could not hold their true value.
#[derive(Debug, Default, Copy, Clone)]
pub struct CountOverflow {
    pub bytes: bool,
    pub chars: bool,
    pub lines: bool,
    pub words: bool,
}

impl WordCount {
    /// Adds `other` to `self`. A counter that would exceed `usize::MAX` stays at
    /// `usize::MAX` and is flagged in `overflow`.
    pub fn add_saturating(&mut self, other: &Self, overflow: &mut CountOverflow) {
        fn add(sum: &mut usize, n: usize, overflowed: &mut bool) {
            *sum = sum.checked_add(n).unwrap_or_else(|| {
                *overflowed = true;
                usize::MAX
            });
        }
        add(&mut self.bytes, other.bytes, &mut overflow.bytes);
        add(&mut self.chars, other.chars, &mut overflow.chars);
        add(&mut self.lines, other.lines, &mut overflow.lines);
        add(&mut self.words, other.words, &mut overflow.words);
        self.max_line_length = max(self.max_line_length, other.max_line_length);
    }
}
