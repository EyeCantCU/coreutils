// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

// spell-checker:ignore (ToDO) INFTY MULT accum breakwords linebreak linebreaking linebreaks linelen maxlength minlength nchars ostream overlen parasplit plass posn powf punct signum slen sstart tabwidth tlen underlen winfo wlen wordlen

use std::io::{BufWriter, Stdout, Write};
use std::mem;
use std::ops::Range;

use crate::FmtOptions;
use crate::parasplit::{CarriedWord, ParaWords, Paragraph, WordInfo};

/// Most words held back from one chunk of a paragraph to the next
const MAX_HELD_WORDS: usize = 8192;

struct BreakArgs<'a> {
    opts: &'a FmtOptions,
    indent: &'a [u8],
    indent_len: usize,
    uniform: bool,
    ostream: &'a mut BufWriter<Stdout>,
}

impl BreakArgs<'_> {
    /// Number of spaces to add before a word, based on mode, newline, sentence start.
    fn compute_slen(&self, newline: bool, start: bool, punct: bool) -> usize {
        if self.uniform || newline {
            if start || (newline && punct) { 2 } else { 1 }
        } else {
            0
        }
    }

    fn compute_width(&self, winfo: &WordInfo, posn: usize, fresh: bool) -> usize {
        if fresh {
            0
        } else {
            let post = winfo.after_tab;
            match winfo.before_tab {
                None => post,
                Some(pre) => {
                    post + ((pre + posn) / self.opts.tabwidth + 1) * self.opts.tabwidth - posn
                }
            }
        }
    }
}

/// What is carried from one chunk of a paragraph to the next, so that a long
/// paragraph can be formatted while only a bounded part of it is in memory.
#[derive(Default)]
pub struct ParaState {
    /// Words read but not written yet
    carry: Vec<CarriedWord>,
    /// Whether the first word of the paragraph was written
    started: bool,
    /// Whether the last word written ends a sentence
    prev_punct: bool,
    /// Whether a line was ended after the last word written
    fresh: bool,
    /// Whether the last word read ends a sentence
    read_punct: bool,
    /// Greedy mode: length of the current output line
    line_len: usize,
    /// Optimal mode: the candidate breaks still open
    kp: Option<KpState>,
    /// Optimal mode: length of the output line holding a word that is
    /// still being passed on
    word_line_len: Option<usize>,
}

/// Format a paragraph, or a chunk of one, and write it out.
///
/// For a chunk that is followed by more of the paragraph, only the lines
/// that cannot change any more are written; `state` keeps what is needed to
/// go on with the next chunk.
pub fn break_lines(
    para: &Paragraph,
    opts: &FmtOptions,
    ostream: &mut BufWriter<Stdout>,
    state: &mut ParaState,
) -> std::io::Result<()> {
    let carry = mem::take(&mut state.carry);
    let p_words = ParaWords::new(opts, para, &carry, state.read_punct);
    let mut words = p_words.words();
    if let Some(word) = words.last() {
        state.read_punct = word.ends_punct;
    }

    let mut args = BreakArgs {
        opts,
        indent: &para.indent_str,
        indent_len: para.indent_len,
        // does this paragraph require uniform spacing?
        uniform: para.mail_header || opts.uniform,
        ostream,
    };

    // the rest of a word too long to be held at once is passed on as it is
    if para.starts_in_word {
        if let Some((piece, rest)) = words.split_first() {
            args.ostream.write_all(piece.word)?;
            state.prev_punct = piece.ends_punct;
            state.fresh = false;
            state.line_len += piece.word_nchars;
            if let Some(len) = &mut state.word_line_len {
                *len += piece.word_nchars;
            }
            words = rest;
        }
        if !(para.ends_in_word && words.is_empty()) {
            // the word is complete; go on from the line it sits on
            if let Some(len) = state.word_line_len.take() {
                state.kp = Some(KpState::new(len, 0, Some(state.prev_punct)));
            }
        }
    }

    if opts.quick || para.mail_header {
        break_simple(words, para, &mut args, state)?;
    } else {
        break_knuth_plass(words, para, &mut args, state)?;
    }

    if !para.more {
        *state = ParaState::default();
    }
    Ok(())
}

/// Write the init or indent, then the first word of a paragraph, and
/// return the length of the line so far.
fn write_first_word(
    word: &WordInfo,
    para: &Paragraph,
    args: &mut BreakArgs,
) -> std::io::Result<usize> {
    let init_len = first_line_start(para, args);
    if args.opts.crown || args.opts.tagged {
        // handle "init" portion
        args.ostream.write_all(&para.init_str)?;
    } else if !para.mail_header {
        // for non-(crown, tagged) that's the same as a normal indent
        args.ostream.write_all(args.indent)?;
    }
    args.ostream.write_all(word.word)?;
    Ok(init_len + word.word_nchars)
}

/// Length of the init or indent that starts a paragraph
fn first_line_start(para: &Paragraph, args: &BreakArgs) -> usize {
    if args.opts.crown || args.opts.tagged {
        para.init_len
    } else if !para.mail_header {
        args.indent_len
    } else {
        // mail headers get no indent at all
        0
    }
}

/// `break_simple` implements a "greedy" breaking algorithm: print words until
/// maxlength would be exceeded, then print a linebreak and indent and continue.
fn break_simple<'a>(
    words: &'a [WordInfo<'a>],
    para: &Paragraph,
    args: &mut BreakArgs<'a>,
    state: &mut ParaState,
) -> std::io::Result<()> {
    let mut iter = words.iter();
    if !state.started {
        // the first word will *always* appear on the first line
        let Some(winfo) = iter.next() else {
            return args.ostream.write_all(b"\n");
        };
        state.line_len = write_first_word(winfo, para, args)?;
        state.prev_punct = false;
        state.started = true;
    }
    (state.line_len, state.prev_punct) = iter.try_fold(
        (state.line_len, state.prev_punct),
        |(l, prev_punct), winfo| accum_words_simple(args, l, prev_punct, winfo),
    )?;
    if para.more {
        Ok(())
    } else {
        args.ostream.write_all(b"\n")
    }
}

fn accum_words_simple<'a>(
    args: &mut BreakArgs<'a>,
    l: usize,
    prev_punct: bool,
    winfo: &'a WordInfo<'a>,
) -> std::io::Result<(usize, bool)> {
    // compute the length of this word, considering how tabs will expand at this position on the line
    let wlen = winfo.word_nchars + args.compute_width(winfo, l, false);

    let slen = args.compute_slen(winfo.new_line, winfo.sentence_start, prev_punct);

    if l + wlen + slen > args.opts.width {
        write_newline(args.indent, args.ostream)?;
        write_with_spaces(&winfo.word[winfo.word_start..], 0, args.ostream)?;
        Ok((args.indent_len + winfo.word_nchars, winfo.ends_punct))
    } else {
        write_with_spaces(winfo.word, slen, args.ostream)?;
        Ok((l + wlen + slen, winfo.ends_punct))
    }
}

/// `break_knuth_plass` implements an "optimal" breaking algorithm in the style of
/// Knuth, D.E., and Plass, M.F. "Breaking Paragraphs into Lines." in Software,
/// Practice and Experience. Vol. 11, No. 11, November 1981.
/// <http://onlinelibrary.wiley.com/doi/10.1002/spe.4380111102/pdf>
///
/// When more of the paragraph follows, the candidate breaks are kept for the
/// next chunk, and only the lines up to the last break that all of them
/// share are written: those lines are the same whatever follows.
fn break_knuth_plass<'a>(
    words: &'a [WordInfo<'a>],
    para: &Paragraph,
    args: &mut BreakArgs<'a>,
    state: &mut ParaState,
) -> std::io::Result<()> {
    let Some(first) = words.first() else {
        return if para.more {
            Ok(())
        } else {
            args.ostream.write_all(b"\n")
        };
    };
    // the first word will *always* appear on the first line
    let mut kp = state
        .kp
        .take()
        .unwrap_or_else(|| KpState::new(first_line_start(para, args) + first.word_nchars, 1, None));

    // Lay out everything if this is the end of the paragraph or of what can
    // be laid out before an overlong word. Otherwise leave out the last words,
    // as the words after them are needed to judge them.
    let to_end = !para.more || para.ends_in_word;
    let limit = if to_end {
        words.len()
    } else {
        words.len().saturating_sub(2)
    };
    if limit > kp.next {
        find_kp_breakpoints(words, kp.next..limit, &mut kp, args);
        kp.next = limit;
    }

    // choose the break up to which to write
    let last = if to_end {
        best_active(&kp.linebreaks, &kp.active)
    } else {
        // Write the lines that all candidate layouts share. If that would
        // hold too many words back, also write lines of the best layout so
        // far, which the words to come are unlikely to change.
        let held_after = |idx: usize| {
            kp.linebreaks[idx]
                .linebreak
                .map_or(words.len(), |k| words.len() - k - 1)
        };
        let decided = decided_break(&kp.linebreaks, &kp.active);
        let mut idx = best_active(&kp.linebreaks, &kp.active);
        while idx != decided && held_after(kp.linebreaks[idx].prev) <= MAX_HELD_WORDS {
            idx = kp.linebreaks[idx].prev;
        }
        idx
    };
    let stop = match kp.linebreaks[last].linebreak {
        Some(k) if !to_end => k + 1,
        None if !to_end => {
            // no line was finished yet
            state.carry = words.iter().map(CarriedWord::new).collect();
            state.kp = Some(kp);
            return Ok(());
        }
        _ => words.len(),
    };

    let begin = if state.started {
        0
    } else {
        write_first_word(first, para, args)?;
        state.started = true;
        state.prev_punct = false;
        state.fresh = false;
        1
    };
    let breakpoints = build_path(&kp.linebreaks, last);
    (state.prev_punct, state.fresh) = write_kp_lines(
        words,
        begin..stop,
        &breakpoints,
        state.prev_punct,
        state.fresh,
        args,
    )?;

    if !to_end {
        state.carry = words[stop..].iter().map(CarriedWord::new).collect();
        kp.retain_from(last, stop);
        state.kp = Some(kp);
        Ok(())
    } else if para.ends_in_word {
        // an overlong word follows; it sits on a line of its own
        state.word_line_len = Some(args.indent_len + words[words.len() - 1].word_nchars);
        Ok(())
    } else {
        args.ostream.write_all(b"\n")
    }
}

/// Write `words[range]` with line breaks at `breakpoints` (last first),
/// and return whether the last word written ends a sentence and whether a
/// line was ended after it.
fn write_kp_lines(
    words: &[WordInfo],
    range: Range<usize>,
    breakpoints: &[(usize, bool)],
    mut prev_punct: bool,
    mut fresh: bool,
    args: &mut BreakArgs,
) -> std::io::Result<(bool, bool)> {
    // note that breakpoints is in reverse break order, so we .rev() it
    let mut breakpoints = breakpoints.iter().rev().peekable();
    for k in range {
        let winfo = &words[k];
        let break_here = breakpoints.next_if(|&&(next_break, _)| next_break == k);
        if break_here.is_some_and(|&(_, break_before)| break_before) {
            write_newline(args.indent, args.ostream)?;
            write_with_spaces(&winfo.word[winfo.word_start..], 0, args.ostream)?;
            fresh = false;
        } else {
            if fresh {
                write_newline(args.indent, args.ostream)?;
            }
            let (slen, word) = slice_if_fresh(args, fresh, winfo, prev_punct);
            write_with_spaces(word, slen, args.ostream)?;
            // breaking after this word, so that means "fresh" is true for the next one
            fresh = break_here.is_some();
        }
        prev_punct = winfo.ends_punct;
    }
    Ok((prev_punct, fresh))
}

#[derive(Clone, Copy)]
struct LineBreak {
    prev: usize,
    /// index of the word at which the line is broken
    linebreak: Option<usize>,
    break_before: bool,
    demerits: i64,
    prev_rat: f32,
    length: usize,
    fresh: bool,
}

/// The candidate breaks of a paragraph that is being formatted
struct KpState {
    /// the breaks considered, the first being the start of the current line
    linebreaks: Vec<LineBreak>,
    /// the breaks whose line can still be extended
    active: Vec<usize>,
    /// index of the next word to look at
    next: usize,
    /// whether the next word starts a sentence
    is_sentence_start: bool,
    /// set if the next word follows a word passed on unheld, and whether
    /// that one ended a sentence
    pending_punct: Option<bool>,
}

impl KpState {
    /// Start with a line of `length`, with the first `next` words on it.
    fn new(length: usize, next: usize, pending_punct: Option<bool>) -> Self {
        Self {
            linebreaks: vec![LineBreak {
                prev: 0,
                linebreak: None,
                break_before: false,
                demerits: 0,
                prev_rat: f32::NAN,
                length,
                fresh: false,
            }],
            active: vec![0],
            next,
            is_sentence_start: false,
            pending_punct,
        }
    }

    /// Make break `start` the start of the layout, after the lines up to it
    /// were written along with the first `written` words. Breaks that do not
    /// follow from it are dropped.
    fn retain_from(&mut self, start: usize, written: usize) {
        let mut keep = vec![false; self.linebreaks.len()];
        keep[start] = true;
        let mut active = Vec::with_capacity(self.active.len());
        for &a in &self.active {
            let mut idx = a;
            while idx > start && !keep[idx] {
                idx = self.linebreaks[idx].prev;
            }
            if idx >= start {
                let mut idx = a;
                while !keep[idx] {
                    keep[idx] = true;
                    idx = self.linebreaks[idx].prev;
                }
                active.push(a);
            }
        }
        // renumber the kept breaks, keeping their order
        let mut new_index = vec![0; self.linebreaks.len()];
        let mut linebreaks = Vec::new();
        for (idx, lb) in self.linebreaks.iter().enumerate().skip(start) {
            if keep[idx] {
                new_index[idx] = linebreaks.len();
                linebreaks.push(if idx == start {
                    LineBreak {
                        prev: 0,
                        linebreak: None,
                        ..*lb
                    }
                } else {
                    LineBreak {
                        prev: new_index[lb.prev],
                        linebreak: lb.linebreak.map(|k| k - written),
                        ..*lb
                    }
                });
            }
        }
        self.active = active.into_iter().map(|a| new_index[a]).collect();
        self.linebreaks = linebreaks;
        self.next -= written;
    }
}

/// Run the algorithm over `words[range]`, adding to the breaks in `kp`.
/// Words after the range are only looked at to judge the words in it.
#[allow(clippy::cognitive_complexity)]
fn find_kp_breakpoints(
    words: &[WordInfo],
    range: Range<usize>,
    kp: &mut KpState,
    args: &BreakArgs,
) {
    let mut linebreaks = mem::take(&mut kp.linebreaks);
    // this vec holds the current active linebreaks; next_ holds the breaks that will be active for
    // the next word
    let mut active_breaks = mem::take(&mut kp.active);
    let mut next_active_breaks = vec![];
    let mut is_sentence_start = kp.is_sentence_start;

    let stretch = args.opts.width - args.opts.goal;
    let minlength = if args.opts.goal <= 10 {
        1
    } else {
        args.opts.goal.max(stretch + 1) - stretch
    };
    let mut new_linebreaks = vec![];
    for k in range {
        let w = &words[k];
        let mut iter = words[k + 1..].iter().peekable();
        if let Some(punct) = kp.pending_punct.take() {
            // the word before was passed on without being held
            is_sentence_start = w.sentence_start || (w.new_line && punct);
        }
        let next_word_sentence_final = is_next_word_sentence_final(iter.clone());
        // if this is the last word, we don't add additional demerits for this break
        let (is_last_word, is_sentence_end) = match iter.peek() {
            None => (true, true),
            Some(&&WordInfo {
                sentence_start: st,
                new_line: nl,
                ..
            }) => (false, st || (nl && w.ends_punct)),
        };

        // should we be adding extra space at the beginning of the next sentence?
        let slen = args.compute_slen(w.new_line, is_sentence_start, false);

        let mut best_active_demerits = i64::MAX;
        let mut ld_idx = 0;
        new_linebreaks.clear();
        let mut best_break_before: Option<LineBreak> = None;
        let mut best_break_after: Option<LineBreak> = None;
        next_active_breaks.clear();
        // go through each active break, extending it and possibly adding a new active
        // break if we are above the minimum required length
        #[allow(clippy::explicit_iter_loop)]
        for &i in active_breaks.iter() {
            let active = &mut linebreaks[i];
            if active.demerits < best_active_demerits {
                best_active_demerits = active.demerits;
                ld_idx = i;
            }

            // Also consider a break before this word, so the previous line can end at the
            // prior word when that yields a better global layout.
            if !active.fresh && active.length >= minlength {
                let (mut new_demerits, new_ratio) = compute_demerits(
                    args.opts.goal as isize - active.length as isize,
                    stretch,
                    w.word_nchars,
                    active.prev_rat,
                );
                if is_sentence_end {
                    new_demerits = new_demerits.saturating_add(ORPHAN_BREAK_PENALTY);
                }
                let total_demerits = active.demerits.saturating_add(new_demerits);
                if best_break_before
                    .as_ref()
                    .is_none_or(|best| total_demerits < best.demerits)
                {
                    best_break_before = Some(LineBreak {
                        prev: i,
                        linebreak: Some(k),
                        break_before: true,
                        demerits: total_demerits,
                        prev_rat: new_ratio,
                        length: args.indent_len + w.word_nchars,
                        fresh: false,
                    });
                }
            }

            // get the new length
            let tlen = w.word_nchars
                + args.compute_width(w, active.length, active.fresh)
                + slen
                + active.length;

            // if tlen is longer than args.opts.width, we drop this break from the active list
            // otherwise, we extend the break, and possibly add a new break at this point
            if tlen <= args.opts.width {
                // this break will still be active next time
                next_active_breaks.push(i);
                // we can put this word on this line
                active.fresh = false;
                active.length = tlen;

                // if we're above the minlength, we can also consider breaking here
                if tlen >= minlength {
                    let (mut new_demerits, new_ratio) = if is_last_word {
                        // there is no penalty for the final line's length
                        (0, 0.0)
                    } else {
                        compute_demerits(
                            args.opts.goal as isize - tlen as isize,
                            stretch,
                            w.word_nchars,
                            active.prev_rat,
                        )
                    };

                    if !is_last_word && next_word_sentence_final {
                        new_demerits = new_demerits.saturating_add(ORPHAN_BREAK_PENALTY);
                    }

                    let total_demerits = active.demerits.saturating_add(new_demerits);
                    if best_break_after
                        .as_ref()
                        .is_none_or(|best| total_demerits < best.demerits)
                    {
                        best_break_after = Some(LineBreak {
                            prev: i,
                            linebreak: Some(k),
                            break_before: false,
                            demerits: total_demerits,
                            prev_rat: new_ratio,
                            length: args.indent_len,
                            fresh: true,
                        });
                    }
                }
            }
        }

        if let Some(lb) = best_break_before {
            new_linebreaks.push(lb);
        }
        if let Some(lb) = best_break_after {
            new_linebreaks.push(lb);
        }

        for lb in new_linebreaks.drain(..) {
            next_active_breaks.push(linebreaks.len());
            linebreaks.push(lb);
        }

        if next_active_breaks.is_empty() {
            // every potential linebreak is too long! choose the linebreak with the least demerits, ld_idx
            let new_break =
                restart_active_breaks(args, &linebreaks[ld_idx], ld_idx, w, k, slen, minlength);
            next_active_breaks.push(linebreaks.len());
            linebreaks.push(new_break);
        }
        // swap in new list of active breaks
        mem::swap(&mut active_breaks, &mut next_active_breaks);
        // If this was the last word in a sentence, the next one must be the first in the next.
        is_sentence_start = is_sentence_end;
    }

    kp.linebreaks = linebreaks;
    kp.active = active_breaks;
    kp.is_sentence_start = is_sentence_start;
}

/// Of the active paths, select the one with the fewest demerits.
fn best_active(paths: &[LineBreak], active: &[usize]) -> usize {
    active
        .iter()
        .copied()
        .min_by_key(|&a| paths[a].demerits)
        .unwrap_or(0)
}

/// The latest break shared by all active paths: whatever words follow, the
/// lines up to it will not change any more.
fn decided_break(paths: &[LineBreak], active: &[usize]) -> usize {
    // a break always comes after the one it extends
    active
        .iter()
        .copied()
        .reduce(|mut a, mut b| {
            while a != b {
                if a > b {
                    a = paths[a].prev;
                } else {
                    b = paths[b].prev;
                }
            }
            a
        })
        .unwrap_or(0)
}

/// The breaks on the path ending at break `idx`, last first.
fn build_path(paths: &[LineBreak], mut idx: usize) -> Vec<(usize, bool)> {
    let mut breakwords = vec![];
    // chase the pointers back through the break list, recording
    // the words at which we should break
    while let Some(word) = paths[idx].linebreak {
        breakwords.push((word, paths[idx].break_before));
        idx = paths[idx].prev;
    }
    breakwords
}

// badness = BAD_MULT * abs(r) ^ 3
const BAD_MULT: f32 = 200.0;
// DR_MULT is multiplier for delta-R between lines
const DR_MULT: f32 = 600.0;
// DL_MULT is penalty multiplier for short words at end of line
const DL_MULT: f32 = 10.0;
// Penalize breaks that leave the first word on the next line as the sentence-final word.
const ORPHAN_BREAK_PENALTY: i64 = 250_000_000;

fn is_word_sentence_final(current: &WordInfo, next: Option<&WordInfo>) -> bool {
    match next {
        None => true,
        Some(next_word) => next_word.sentence_start || (next_word.new_line && current.ends_punct),
    }
}

fn is_next_word_sentence_final<'a, T: Iterator<Item = &'a WordInfo<'a>>>(mut iter: T) -> bool {
    let Some(next_word) = iter.next() else {
        return false;
    };
    is_word_sentence_final(next_word, iter.next())
}

fn compute_demerits(delta_len: isize, stretch: usize, wlen: usize, prev_rat: f32) -> (i64, f32) {
    // how much stretch are we using?
    let ratio = if delta_len == 0 {
        0.0f32
    } else {
        delta_len as f32 / stretch as f32
    };

    // compute badness given the stretch ratio
    let bad_linelen = (BAD_MULT * ratio.powi(3).abs()) as i64;

    // we penalize lines ending in really short words
    let bad_wordlen = if wlen >= stretch {
        0
    } else {
        (DL_MULT
            * ((stretch - wlen) as f32 / (stretch - 1) as f32)
                .powi(3)
                .abs()) as i64
    };

    // we penalize lines that have very different ratios from previous lines
    let bad_delta_r = if prev_rat.is_nan() {
        0
    } else {
        (DR_MULT * ((ratio - prev_rat) / 2.0).powi(3).abs()) as i64
    };

    let demerits_base = 1_i64
        .saturating_add(bad_linelen)
        .saturating_add(bad_wordlen)
        .saturating_add(bad_delta_r);
    let demerits = demerits_base.saturating_mul(demerits_base);

    (demerits, ratio)
}

fn restart_active_breaks(
    args: &BreakArgs,
    active: &LineBreak,
    act_idx: usize,
    w: &WordInfo,
    k: usize,
    slen: usize,
    min: usize,
) -> LineBreak {
    let (break_before, line_length) = if active.fresh {
        // never break before a word if that word would be the first on a line
        (false, args.indent_len)
    } else {
        // choose the lesser evil: breaking too early, or breaking too late
        let wlen = w.word_nchars + args.compute_width(w, active.length, active.fresh);
        let underlen = min as isize - active.length as isize;
        let overlen = (wlen + slen + active.length) as isize - args.opts.width as isize;
        if overlen > underlen {
            // break early, put this word on the next line
            (true, args.indent_len + w.word_nchars)
        } else {
            (false, args.indent_len)
        }
    };

    // restart the linebreak. This will be our only active path.
    LineBreak {
        prev: act_idx,
        linebreak: Some(k),
        break_before,
        demerits: 0, // this is the only active break, so we can reset the demerit count
        prev_rat: if break_before { 1.0 } else { -1.0 },
        length: line_length,
        fresh: !break_before,
    }
}

/// If we're on a fresh line, `slen=0` and we slice off leading whitespace.
/// Otherwise, compute `slen` and leave whitespace alone.
fn slice_if_fresh<'a>(
    args: &BreakArgs,
    fresh: bool,
    winfo: &WordInfo<'a>,
    prev_punct: bool,
) -> (usize, &'a [u8]) {
    if fresh {
        (0, &winfo.word[winfo.word_start..])
    } else {
        (
            args.compute_slen(winfo.new_line, winfo.sentence_start, prev_punct),
            winfo.word,
        )
    }
}

/// Write a newline and add the indent.
fn write_newline(indent: &[u8], ostream: &mut BufWriter<Stdout>) -> std::io::Result<()> {
    ostream.write_all(b"\n")?;
    ostream.write_all(indent)
}

/// Write the word, along with slen spaces.
fn write_with_spaces(
    word: &[u8],
    slen: usize,
    ostream: &mut BufWriter<Stdout>,
) -> std::io::Result<()> {
    if slen == 2 {
        ostream.write_all(b"  ")?;
    } else if slen == 1 {
        ostream.write_all(b" ")?;
    }
    ostream.write_all(word)
}
