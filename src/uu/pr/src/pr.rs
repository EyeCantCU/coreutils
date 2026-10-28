// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

// spell-checker:ignore (ToDO) adFfmprt Ffabm tabify nonprinting backspacing

use clap::{Arg, ArgAction, ArgMatches, Command};
use std::ffi::{OsStr, OsString};
use std::fs::{File, metadata};
use std::io::{self, BufRead, BufReader, Write, stdin, stdout};
use std::time::SystemTime;

use uucore::display::Quotable;
use uucore::error::{UResult, USimpleError, strip_errno};
use uucore::format_usage;
use uucore::time::{FormatSystemTimeFallback, format, format_system_time};
use uucore::{os_str_as_bytes_lossy, show_error, translate};

const TAB: u8 = b'\t';
const FF: u8 = 0x0C;
const NL: u8 = b'\n';
const BS: u8 = 0x08;
const LINES_PER_PAGE: usize = 66;
const HEADER_LINES_PER_PAGE: usize = 5;
const TRAILER_LINES_PER_PAGE: usize = 5;
const DEFAULT_PAGE_WIDTH: usize = 72;
const DEFAULT_TAB_WIDTH: usize = 8;
const DEFAULT_NUMBER_DIGITS: usize = 5;
const FILE_STDIN: &str = "-";
const MAX_INT_VALUE: i64 = i32::MAX as i64;

mod options {
    pub const HEADER: &str = "header";
    pub const DATE_FORMAT: &str = "date-format";
    pub const DOUBLE_SPACE: &str = "double-space";
    pub const NUMBER_LINES: &str = "number-lines";
    pub const FIRST_LINE_NUMBER: &str = "first-line-number";
    pub const PAGES: &str = "pages";
    /// A legacy `+FIRST[:LAST]` operand, passed on by the argument rewriting.
    pub const PAGES_OPERAND: &str = "pages-operand";
    pub const OMIT_HEADER: &str = "omit-header";
    pub const OMIT_PAGINATION: &str = "omit-pagination";
    pub const PAGE_LENGTH: &str = "length";
    pub const NO_FILE_WARNINGS: &str = "no-file-warnings";
    pub const FORM_FEED: &str = "form-feed";
    pub const COLUMN_WIDTH: &str = "width";
    pub const PAGE_WIDTH: &str = "page-width";
    pub const ACROSS: &str = "across";
    pub const COLUMN_DOWN: &str = "column-down";
    pub const COLUMNS: &str = "columns";
    pub const COLUMN_CHAR_SEPARATOR: &str = "separator";
    pub const COLUMN_STRING_SEPARATOR: &str = "sep-string";
    pub const MERGE: &str = "merge";
    pub const INDENT: &str = "indent";
    pub const JOIN_LINES: &str = "join-lines";
    pub const HELP: &str = "help";
    pub const FILES: &str = "files";
    pub const EXPAND_TABS: &str = "expand-tabs";
    pub const OUTPUT_TABS: &str = "output-tabs";
    pub const SHOW_CONTROL_CHARS: &str = "show-control-chars";
    pub const SHOW_NONPRINTING: &str = "show-nonprinting";
}

/// How a short option uses the rest of its argument.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OptKind {
    Flag,
    Required,
    Optional,
}

/// Short options, the long option they stand for, and how they take a value.
const SHORT_OPTIONS: &[(u8, &str, OptKind)] = &[
    (b'a', options::ACROSS, OptKind::Flag),
    (b'b', options::COLUMN_DOWN, OptKind::Flag),
    (b'c', options::SHOW_CONTROL_CHARS, OptKind::Flag),
    (b'd', options::DOUBLE_SPACE, OptKind::Flag),
    (b'D', options::DATE_FORMAT, OptKind::Required),
    (b'e', options::EXPAND_TABS, OptKind::Optional),
    (b'f', options::FORM_FEED, OptKind::Flag),
    (b'F', options::FORM_FEED, OptKind::Flag),
    (b'h', options::HEADER, OptKind::Required),
    (b'i', options::OUTPUT_TABS, OptKind::Optional),
    (b'J', options::JOIN_LINES, OptKind::Flag),
    (b'l', options::PAGE_LENGTH, OptKind::Required),
    (b'm', options::MERGE, OptKind::Flag),
    (b'n', options::NUMBER_LINES, OptKind::Optional),
    (b'N', options::FIRST_LINE_NUMBER, OptKind::Required),
    (b'o', options::INDENT, OptKind::Required),
    (b'r', options::NO_FILE_WARNINGS, OptKind::Flag),
    (b's', options::COLUMN_CHAR_SEPARATOR, OptKind::Optional),
    (b'S', options::COLUMN_STRING_SEPARATOR, OptKind::Optional),
    (b't', options::OMIT_HEADER, OptKind::Flag),
    (b'T', options::OMIT_PAGINATION, OptKind::Flag),
    (b'v', options::SHOW_NONPRINTING, OptKind::Flag),
    (b'w', options::COLUMN_WIDTH, OptKind::Required),
    (b'W', options::PAGE_WIDTH, OptKind::Required),
];

/// Long options that need a value, used to keep that value away from the
/// rewriting of short options and legacy operands.
const LONG_OPTIONS_WITH_VALUE: &[&str] = &[
    options::PAGES,
    options::HEADER,
    options::DATE_FORMAT,
    options::FIRST_LINE_NUMBER,
    options::PAGE_LENGTH,
    options::COLUMN_WIDTH,
    options::PAGE_WIDTH,
    options::COLUMNS,
    options::INDENT,
];

/// All long option names, used to resolve abbreviations.
const LONG_OPTIONS: &[&str] = &[
    options::PAGES,
    options::HEADER,
    options::DATE_FORMAT,
    options::DOUBLE_SPACE,
    options::NUMBER_LINES,
    options::FIRST_LINE_NUMBER,
    options::OMIT_HEADER,
    options::OMIT_PAGINATION,
    options::PAGE_LENGTH,
    options::NO_FILE_WARNINGS,
    options::FORM_FEED,
    options::COLUMN_WIDTH,
    options::PAGE_WIDTH,
    options::ACROSS,
    options::COLUMNS,
    options::COLUMN_CHAR_SEPARATOR,
    options::COLUMN_STRING_SEPARATOR,
    options::MERGE,
    options::INDENT,
    options::JOIN_LINES,
    options::HELP,
    options::EXPAND_TABS,
    options::OUTPUT_TABS,
    options::SHOW_CONTROL_CHARS,
    options::SHOW_NONPRINTING,
    "version",
];

#[derive(Debug, thiserror::Error)]
enum PrError {
    /// A message printed as `pr: <msg>`, exit status 1.
    #[error("{0}")]
    Message(String),

    #[error("{}", strip_errno(.0))]
    Write(io::Error),
}

impl PrError {
    fn msg(s: impl Into<String>) -> Self {
        Self::Message(s.into())
    }
}

pub fn uu_app() -> Command {
    Command::new("pr")
        .version(uucore::crate_version!())
        .help_template(uucore::localized_help_template("pr"))
        .about(translate!("pr-about"))
        .after_help(translate!("pr-after-help"))
        .override_usage(format_usage(&translate!("pr-usage")))
        .infer_long_args(true)
        .args_override_self(true)
        .disable_help_flag(true)
        .arg(
            Arg::new(options::PAGES)
                .long(options::PAGES)
                .help(translate!("pr-help-pages"))
                .allow_hyphen_values(true)
                .value_name("FIRST_PAGE[:LAST_PAGE]"),
        )
        .arg(
            Arg::new(options::PAGES_OPERAND)
                .long(options::PAGES_OPERAND)
                .hide(true)
                .allow_hyphen_values(true),
        )
        .arg(
            Arg::new(options::HEADER)
                .short('h')
                .long(options::HEADER)
                .help(translate!("pr-help-header"))
                .allow_hyphen_values(true)
                .value_parser(clap::value_parser!(OsString))
                .value_name("STRING"),
        )
        .arg(
            Arg::new(options::DATE_FORMAT)
                .short('D')
                .long(options::DATE_FORMAT)
                .value_name("FORMAT")
                .allow_hyphen_values(true)
                .help(translate!("pr-help-date-format")),
        )
        .arg(
            Arg::new(options::DOUBLE_SPACE)
                .short('d')
                .long(options::DOUBLE_SPACE)
                .help(translate!("pr-help-double-space"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::NUMBER_LINES)
                .short('n')
                .long(options::NUMBER_LINES)
                .help(translate!("pr-help-number-lines"))
                .action(ArgAction::Append)
                .num_args(0..=1)
                .require_equals(true)
                .value_parser(clap::value_parser!(OsString))
                .value_name("[char][width]"),
        )
        .arg(
            Arg::new(options::FIRST_LINE_NUMBER)
                .short('N')
                .long(options::FIRST_LINE_NUMBER)
                .help(translate!("pr-help-first-line-number"))
                .allow_hyphen_values(true)
                .value_name("NUMBER"),
        )
        .arg(
            Arg::new(options::OMIT_HEADER)
                .short('t')
                .long(options::OMIT_HEADER)
                .help(translate!("pr-help-omit-header"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::OMIT_PAGINATION)
                .short('T')
                .long(options::OMIT_PAGINATION)
                .help(translate!("pr-help-omit-pagination"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::PAGE_LENGTH)
                .short('l')
                .long(options::PAGE_LENGTH)
                .help(translate!("pr-help-page-length"))
                .allow_hyphen_values(true)
                .value_name("PAGE_LENGTH"),
        )
        .arg(
            Arg::new(options::NO_FILE_WARNINGS)
                .short('r')
                .long(options::NO_FILE_WARNINGS)
                .help(translate!("pr-help-no-file-warnings"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::FORM_FEED)
                .short('F')
                .visible_short_alias('f')
                .long(options::FORM_FEED)
                .help(translate!("pr-help-form-feed"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::COLUMN_WIDTH)
                .short('w')
                .long(options::COLUMN_WIDTH)
                .help(translate!("pr-help-column-width"))
                .allow_hyphen_values(true)
                .value_name("width"),
        )
        .arg(
            Arg::new(options::PAGE_WIDTH)
                .short('W')
                .long(options::PAGE_WIDTH)
                .help(translate!("pr-help-page-width"))
                .allow_hyphen_values(true)
                .value_name("width"),
        )
        .arg(
            Arg::new(options::ACROSS)
                .short('a')
                .long(options::ACROSS)
                .help(translate!("pr-help-across"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            // -b is a no-op for backwards compatibility (column-down is now the default)
            Arg::new(options::COLUMN_DOWN)
                .short('b')
                .hide(true)
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::COLUMNS)
                .long(options::COLUMNS)
                .help(translate!("pr-help-columns"))
                .allow_hyphen_values(true)
                .value_name("columns"),
        )
        .arg(
            Arg::new(options::COLUMN_CHAR_SEPARATOR)
                .short('s')
                .long(options::COLUMN_CHAR_SEPARATOR)
                .help(translate!("pr-help-column-char-separator"))
                .value_name("char")
                .num_args(0..=1)
                .require_equals(true)
                .value_parser(clap::value_parser!(OsString)),
        )
        .arg(
            Arg::new(options::COLUMN_STRING_SEPARATOR)
                .short('S')
                .long(options::COLUMN_STRING_SEPARATOR)
                .help(translate!("pr-help-column-string-separator"))
                .value_name("string")
                .num_args(0..=1)
                .require_equals(true)
                .value_parser(clap::value_parser!(OsString)),
        )
        .arg(
            Arg::new(options::MERGE)
                .short('m')
                .long(options::MERGE)
                .help(translate!("pr-help-merge"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::INDENT)
                .short('o')
                .long(options::INDENT)
                .help(translate!("pr-help-indent"))
                .allow_hyphen_values(true)
                .value_name("margin"),
        )
        .arg(
            Arg::new(options::JOIN_LINES)
                .short('J')
                .long(options::JOIN_LINES)
                .help(translate!("pr-help-join-lines"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::SHOW_CONTROL_CHARS)
                .short('c')
                .long(options::SHOW_CONTROL_CHARS)
                .help(translate!("pr-help-show-control-chars"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::SHOW_NONPRINTING)
                .short('v')
                .long(options::SHOW_NONPRINTING)
                .help(translate!("pr-help-show-nonprinting"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::HELP)
                .long(options::HELP)
                .help(translate!("pr-help-help"))
                .action(ArgAction::Help),
        )
        .arg(
            Arg::new(options::FILES)
                .action(ArgAction::Append)
                .value_parser(clap::value_parser!(OsString))
                .value_hint(clap::ValueHint::FilePath),
        )
        .arg(
            Arg::new(options::EXPAND_TABS)
                .long(options::EXPAND_TABS)
                .short('e')
                .action(ArgAction::Append)
                .num_args(0..=1)
                .require_equals(true)
                .value_parser(clap::value_parser!(OsString))
                .value_name("[CHAR][WIDTH]")
                .help(translate!("pr-help-expand-tabs")),
        )
        .arg(
            Arg::new(options::OUTPUT_TABS)
                .long(options::OUTPUT_TABS)
                .short('i')
                .action(ArgAction::Append)
                .num_args(0..=1)
                .require_equals(true)
                .value_parser(clap::value_parser!(OsString))
                .value_name("[CHAR][WIDTH]")
                .help(translate!("pr-help-output-tabs")),
        )
}

#[uucore::main]
pub fn uumain(args: impl uucore::Args) -> UResult<()> {
    let args: Vec<OsString> = args.collect();
    let matches =
        uucore::clap_localization::handle_clap_result(uu_app(), rewrite_arguments(&args))?;

    let files: Vec<OsString> = matches.get_many::<OsString>(options::FILES).map_or_else(
        || vec![OsString::from(FILE_STDIN)],
        |v| v.cloned().collect(),
    );

    let settings = match Settings::from_matches(&matches, &files) {
        Ok(s) => s,
        Err(e) => return Err(USimpleError::new(1, e.to_string())),
    };

    // Under `stdbuf -oL` each line is written as soon as it is complete.
    let mut out = uucore::stdbuf::stdout_buf_writer(stdout().lock());
    let mut failed = false;
    let result = if settings.merge {
        print_merged(&files, &settings, &mut out, &mut failed)
    } else {
        files.iter().try_for_each(|file| {
            if let Some(input) = open_input(file, &settings) {
                print_file(input, file, &settings, &mut out).map(|ok| failed |= !ok)
            } else {
                failed = true;
                Ok(())
            }
        })
    };
    let result = result.and_then(|()| out.flush().map_err(PrError::Write));
    match result {
        Ok(()) if failed => Err(1.into()),
        Ok(()) => Ok(()),
        Err(e) => Err(USimpleError::new(1, e.to_string())),
    }
}

/// Build an `OsString` from bytes taken out of an argument.
fn os_from_bytes(bytes: &[u8]) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        OsStr::from_bytes(bytes).to_owned()
    }
    #[cfg(not(unix))]
    {
        OsString::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

fn long_with_value(name: &str, value: &[u8]) -> OsString {
    let mut s = OsString::from(format!("--{name}="));
    s.push(os_from_bytes(value));
    s
}

/// Rewrite the command line into a form clap can parse.
///
/// pr has options with optional values that must be attached (`-n3`, `-e`, `-S:`),
/// numeric options (`-3`), and legacy `+FIRST[:LAST]` operands. Short options are
/// turned into their long equivalents, with any value attached after `=`.
fn rewrite_arguments(args: &[OsString]) -> Vec<OsString> {
    let mut result = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    if let Some(name) = iter.next() {
        result.push(name.clone());
    }
    // Index in `result` of the `--columns=` built from the digits seen so far,
    // while the most recently seen option character was a digit.
    let mut digits: Option<(usize, Vec<u8>)> = None;
    let mut only_operands = false;
    // Only a `+FIRST[:LAST]` before any other page range selects pages.
    let mut pages_seen = false;
    while let Some(arg) = iter.next() {
        let bytes = os_str_as_bytes_lossy(arg);
        if only_operands || bytes.len() < 2 || bytes[0] != b'-' {
            // A `+` operand that is not a valid page range is a file name.
            if !only_operands
                && !pages_seen
                && bytes.first() == Some(&b'+')
                && !matches!(parse_page_range(&bytes[1..]), Err(PageRangeError::Range))
            {
                pages_seen = true;
                result.push(long_with_value(options::PAGES_OPERAND, &bytes[1..]));
            } else {
                result.push(arg.clone());
            }
            continue;
        }
        if bytes.as_ref() == b"--" {
            only_operands = true;
            result.push(arg.clone());
            continue;
        }
        if bytes[1] == b'-' {
            digits = None;
            result.push(arg.clone());
            let name = &bytes[2..];
            let bare_name = name.split(|&b| b == b'=').next().unwrap_or_default();
            pages_seen |= resolve_long_option(bare_name) == Some(options::PAGES);
            if !name.contains(&b'=')
                && long_option_needs_value(name)
                && let Some(value) = iter.next()
            {
                result.push(value.clone());
            }
            continue;
        }
        let mut i = 1;
        while i < bytes.len() {
            let c = bytes[i];
            if c.is_ascii_digit() {
                if let Some((index, number)) = &mut digits {
                    number.push(c);
                    result[*index] = long_with_value(options::COLUMNS, number);
                } else {
                    result.push(long_with_value(options::COLUMNS, &[c]));
                    digits = Some((result.len() - 1, vec![c]));
                }
                i += 1;
                continue;
            }
            digits = None;
            match SHORT_OPTIONS.iter().find(|(s, _, _)| *s == c) {
                Some((_, _, OptKind::Flag)) | None => {
                    result.push(os_from_bytes(&[b'-', c]));
                    i += 1;
                }
                Some((_, long, OptKind::Optional)) => {
                    if i + 1 < bytes.len() {
                        result.push(long_with_value(long, &bytes[i + 1..]));
                    } else {
                        result.push(OsString::from(format!("--{long}")));
                    }
                    break;
                }
                Some((_, long, OptKind::Required)) => {
                    if i + 1 < bytes.len() {
                        result.push(long_with_value(long, &bytes[i + 1..]));
                    } else if let Some(value) = iter.next() {
                        result.push(long_with_value(long, &os_str_as_bytes_lossy(value)));
                    } else {
                        // Let clap report the missing value.
                        result.push(os_from_bytes(&[b'-', c]));
                    }
                    break;
                }
            }
        }
    }
    result
}

/// The long option that the possibly abbreviated `name` stands for.
fn resolve_long_option(name: &[u8]) -> Option<&'static str> {
    if let Some(exact) = LONG_OPTIONS.iter().find(|o| o.as_bytes() == name) {
        return Some(exact);
    }
    let mut candidates = LONG_OPTIONS
        .iter()
        .filter(|o| o.as_bytes().starts_with(name));
    match (candidates.next(), candidates.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// Whether the (possibly abbreviated) long option `name` takes a separate value.
fn long_option_needs_value(name: &[u8]) -> bool {
    resolve_long_option(name).is_some_and(|o| LONG_OPTIONS_WITH_VALUE.contains(&o))
}

/// Outcome of parsing a decimal integer the way pr's numeric options accept it.
enum IntError {
    Invalid,
    TooLarge,
}

/// Parse an integer with optional leading blanks and sign. Values outside the
/// range of a 32-bit signed integer are reported as too large.
fn parse_int(s: &[u8]) -> Result<i64, IntError> {
    let s = s.trim_ascii_start();
    let (negative, digits) = match s.first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return Err(IntError::Invalid);
    }
    let mut value: i64 = 0;
    for d in digits {
        value = value * 10 + i64::from(d - b'0');
        if value > MAX_INT_VALUE + 1 {
            return Err(IntError::TooLarge);
        }
    }
    let value = if negative { -value } else { value };
    if !(-MAX_INT_VALUE - 1..=MAX_INT_VALUE).contains(&value) {
        return Err(IntError::TooLarge);
    }
    Ok(value)
}

fn errno_out_of_range() -> String {
    translate!("pr-error-out-of-range")
}

fn errno_too_large() -> String {
    translate!("pr-error-value-too-large")
}

/// Parse the value of a numeric option, which must lie in `min..=i32::MAX`.
/// Values below `min` are out of range, or too large when `negative_too_large`
/// is set (an unsigned option given a negative value).
fn parse_numeric_option(
    raw: &str,
    description: &str,
    min: i64,
    negative_too_large: bool,
) -> Result<i64, PrError> {
    let fail = |reason: Option<String>| {
        let mut msg = format!("{description}: {}", raw.quote());
        if let Some(reason) = reason {
            msg.push_str(": ");
            msg.push_str(&reason);
        }
        PrError::msg(msg)
    };
    match parse_int(raw.as_bytes()) {
        Ok(n) if n < 0 && negative_too_large => Err(fail(Some(errno_too_large()))),
        Ok(n) if n < min => Err(fail(Some(errno_out_of_range()))),
        Ok(n) => Ok(n),
        Err(IntError::Invalid) => Err(fail(None)),
        Err(IntError::TooLarge) => Err(fail(Some(errno_too_large()))),
    }
}

fn get_numeric_option(
    matches: &ArgMatches,
    id: &str,
    description: &str,
    min: i64,
) -> Result<Option<i64>, PrError> {
    matches
        .get_one::<String>(id)
        .map(|raw| parse_numeric_option(raw, description, min, false))
        .transpose()
}

#[derive(Debug)]
enum PageRangeError {
    Invalid,
    Range,
    Suffix,
    TooLarge,
}

/// Parse an unsigned page number at the start of `s`, returning it with the rest of `s`.
fn parse_page_number(s: &[u8]) -> Result<(u64, &[u8]), PageRangeError> {
    let s = s.trim_ascii_start();
    let s = s.strip_prefix(b"+").unwrap_or(s);
    let len = s.iter().take_while(|b| b.is_ascii_digit()).count();
    if len == 0 {
        return Err(PageRangeError::Invalid);
    }
    let (digits, rest) = s.split_at(len);
    let mut value: u64 = 0;
    for d in digits {
        match value
            .checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(d - b'0')))
        {
            Some(v) => value = v,
            None if rest.is_empty() => return Err(PageRangeError::TooLarge),
            None => return Err(PageRangeError::Suffix),
        }
    }
    Ok((value, rest))
}

/// Parse `FIRST[:LAST]`.
fn parse_page_range(s: &[u8]) -> Result<(u64, Option<u64>), PageRangeError> {
    let (first, rest) = parse_page_number(s)?;
    let last = match rest {
        [] => None,
        [b':', second @ ..] => {
            let (last, rest) = parse_page_number(second)?;
            if !rest.is_empty() {
                return Err(PageRangeError::Suffix);
            }
            Some(last)
        }
        _ => return Err(PageRangeError::Range),
    };
    if first == 0 || last.is_some_and(|last| last < first) {
        return Err(PageRangeError::Range);
    }
    Ok((first, last))
}

/// A character and a width, as given to `-e`, `-i` and `-n`.
#[derive(Clone, Copy)]
struct CharWidth {
    ch: u8,
    width: usize,
}

/// Parse the optional `[CHAR][WIDTH]` arguments of `-e`, `-i` or `-n`. Each
/// use of the option changes only what its argument gives.
fn parse_char_width(
    option: char,
    matches: &ArgMatches,
    id: &str,
    default: CharWidth,
) -> Result<Option<CharWidth>, PrError> {
    if !matches.contains_id(id) {
        return Ok(None);
    }
    let mut result = default;
    if let Some(occurrences) = matches.get_occurrences::<OsString>(id) {
        for mut values in occurrences {
            if let Some(value) = values.next() {
                result = parse_char_width_value(option, value, result)?;
            }
        }
    }
    Ok(Some(result))
}

fn parse_char_width_value(
    option: char,
    value: &OsString,
    current: CharWidth,
) -> Result<CharWidth, PrError> {
    let bytes = os_str_as_bytes_lossy(value);
    let try_help = translate!("pr-try-help-message");
    if bytes.is_empty() {
        return Err(PrError::msg(format!(
            "{}\n{try_help}",
            translate!("pr-error-empty-argument", "option" => option)
        )));
    }
    let mut result = current;
    let digits = if bytes[0].is_ascii_digit() {
        &bytes[..]
    } else {
        result.ch = bytes[0];
        &bytes[1..]
    };
    if !digits.is_empty() {
        let shown = os_from_bytes(digits);
        let invalid = |reason: Option<String>| {
            let mut msg = translate!(
                "pr-error-invalid-char-width-argument",
                "option" => option,
                "arg" => shown.quote()
            );
            if let Some(reason) = reason {
                msg.push_str(": ");
                msg.push_str(&reason);
            }
            PrError::msg(format!("{msg}\n{try_help}"))
        };
        result.width = match parse_int(digits) {
            Ok(n) if n > 0 => n as usize,
            Ok(_) | Err(IntError::Invalid) => return Err(invalid(None)),
            Err(IntError::TooLarge) => return Err(invalid(Some(errno_too_large()))),
        };
    }
    Ok(result)
}

/// How nonprinting characters are shown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ControlChars {
    /// Leave them alone.
    Raw,
    /// `-c`: hat notation for control characters, octal for the rest.
    Hat,
    /// `-v`: octal for all of them.
    Octal,
}

/// Line numbering settings.
#[derive(Clone, Copy)]
struct Numbering {
    separator: u8,
    digits: usize,
}

impl Numbering {
    /// Width taken by a number and its separator, a TAB counting up to the next tab stop.
    fn field_width(&self) -> usize {
        if self.separator == TAB {
            self.digits + DEFAULT_TAB_WIDTH - self.digits % DEFAULT_TAB_WIDTH
        } else {
            self.digits + 1
        }
    }

    /// The last `digits` characters of the number, right-aligned.
    fn format(&self, number: i64) -> Vec<u8> {
        let s = number.to_string();
        let s = s.as_bytes();
        let shown = &s[s.len().saturating_sub(self.digits)..];
        let mut out = vec![b' '; self.digits - shown.len()];
        out.extend_from_slice(shown);
        out
    }
}

#[derive(Clone)]
struct Settings {
    /// Print the five-line header and trailer on each page.
    header_enabled: bool,
    /// Number of text lines on a page.
    body_lines: usize,
    double_space: bool,
    /// End pages with a form feed instead of blank lines.
    form_feed: bool,
    /// Copy form feeds found in the input when there is no header (`-t` but not `-T`).
    keep_input_form_feeds: bool,
    first_page: u64,
    last_page: Option<u64>,
    /// Number of text columns for a single file.
    columns: usize,
    across: bool,
    merge: bool,
    /// Multi-column output without truncation and alignment.
    join: bool,
    /// Width of each text column in multi-column output.
    column_width: usize,
    separator: Vec<u8>,
    /// Width used to lay out the header line.
    page_width: usize,
    /// Truncation width of single-column output (`-W`).
    single_width: Option<usize>,
    /// Truncation width when merging a single file, where `-w` also applies.
    merged_single_width: Option<usize>,
    margin: usize,
    number: Option<Numbering>,
    first_line_number: Option<i64>,
    expand: Option<CharWidth>,
    tabify: Option<CharWidth>,
    control_chars: ControlChars,
    header: Option<Vec<u8>>,
    date_format: String,
    /// Do not report files that cannot be read (`-r`).
    quiet: bool,
}

impl Settings {
    #[allow(clippy::cognitive_complexity)]
    fn from_matches(matches: &ArgMatches, files: &[OsString]) -> Result<Self, PrError> {
        let merge = matches.get_flag(options::MERGE);

        // `--pages` comes after any `+FIRST[:LAST]` operand, so it wins.
        let pages = [(options::PAGES, "--pages"), (options::PAGES_OPERAND, "+")]
            .into_iter()
            .find_map(|(id, option)| matches.get_one::<String>(id).map(|raw| (raw, option)))
            .map(|(raw, option)| {
                parse_page_range(raw.as_bytes()).map_err(|e| {
                    let arg = raw.quote();
                    PrError::msg(match e {
                        PageRangeError::Invalid => {
                            translate!("pr-error-invalid-pages-argument", "option" => option, "arg" => arg)
                        }
                        PageRangeError::Range => {
                            translate!("pr-error-invalid-page-range", "arg" => arg)
                        }
                        PageRangeError::Suffix => {
                            translate!("pr-error-invalid-pages-suffix", "option" => option, "arg" => arg)
                        }
                        PageRangeError::TooLarge => {
                            translate!("pr-error-pages-too-large", "option" => option, "arg" => arg)
                        }
                    })
                })
            })
            .transpose()?;
        let (first_page, last_page) = pages.unwrap_or((1, None));

        let columns = match matches.get_one::<String>(options::COLUMNS) {
            Some(raw) => {
                parse_numeric_option(raw, &translate!("pr-error-invalid-columns"), 1, false)?
                    as usize
            }
            None => 1,
        };

        let page_length = get_numeric_option(
            matches,
            options::PAGE_LENGTH,
            &translate!("pr-error-invalid-page-length"),
            1,
        )?
        .map_or(LINES_PER_PAGE, |n| n as usize);
        let page_width = get_numeric_option(
            matches,
            options::PAGE_WIDTH,
            &translate!("pr-error-invalid-page-width", "option" => "W"),
            1,
        )?
        .map(|n| n as usize);
        let column_width = get_numeric_option(
            matches,
            options::COLUMN_WIDTH,
            &translate!("pr-error-invalid-page-width", "option" => "w"),
            1,
        )?
        .map(|n| n as usize);
        let first_line_number = get_numeric_option(
            matches,
            options::FIRST_LINE_NUMBER,
            &translate!("pr-error-invalid-first-line-number"),
            i64::from(i32::MIN),
        )?;
        let margin = matches
            .get_one::<String>(options::INDENT)
            .map(|raw| parse_numeric_option(raw, &translate!("pr-error-invalid-indent"), 0, true))
            .transpose()?
            .map_or(0, |n| n as usize);

        let default_tab = CharWidth {
            ch: TAB,
            width: DEFAULT_TAB_WIDTH,
        };
        let expand = parse_char_width('e', matches, options::EXPAND_TABS, default_tab)?;
        let tabify = parse_char_width('i', matches, options::OUTPUT_TABS, default_tab)?;
        let number = parse_char_width(
            'n',
            matches,
            options::NUMBER_LINES,
            CharWidth {
                ch: TAB,
                width: DEFAULT_NUMBER_DIGITS,
            },
        )?
        .map(|cw| Numbering {
            separator: cw.ch,
            digits: cw.width,
        });

        if merge && columns > 1 {
            return Err(PrError::msg(translate!("pr-error-column-merge-conflict")));
        }
        if merge && matches.get_flag(options::ACROSS) {
            return Err(PrError::msg(translate!("pr-error-across-merge-conflict")));
        }

        let join_lines = matches.get_flag(options::JOIN_LINES);
        let char_separator = matches
            .contains_id(options::COLUMN_CHAR_SEPARATOR)
            .then(|| {
                matches
                    .get_one::<OsString>(options::COLUMN_CHAR_SEPARATOR)
                    .map(|s| os_str_as_bytes_lossy(s).into_owned())
                    .unwrap_or_default()
            });
        let string_separator = matches
            .contains_id(options::COLUMN_STRING_SEPARATOR)
            .then(|| {
                matches
                    .get_one::<OsString>(options::COLUMN_STRING_SEPARATOR)
                    .map(|s| os_str_as_bytes_lossy(s).into_owned())
                    .unwrap_or_default()
            });
        let width_given = page_width.is_some() || column_width.is_some();
        let separator = match (&string_separator, &char_separator) {
            (Some(s), _) => s.clone(),
            (None, Some(s)) if !s.is_empty() => s.clone(),
            (None, Some(_)) if width_given => Vec::new(),
            (None, Some(_)) => vec![TAB],
            (None, None) if join_lines => vec![TAB],
            (None, None) => vec![b' '],
        };
        // `-s` without a width turns off truncation and alignment, like `-J`.
        let join =
            join_lines || (char_separator.is_some() && string_separator.is_none() && !width_given);

        let across = matches.get_flag(options::ACROSS) && columns > 1;
        let width = page_width.or(column_width).unwrap_or(DEFAULT_PAGE_WIDTH);
        let text_columns = if merge { files.len() } else { columns };
        let multi_column = text_columns > 1;

        let merged_single_width = page_width.or(column_width).filter(|_| !join_lines);
        let column_width = if multi_column && !join {
            let separators = (text_columns - 1) * separator.len();
            let numbers = if merge {
                number.map_or(0, |n| n.field_width())
            } else {
                0
            };
            let usable = width.saturating_sub(separators + numbers);
            let column_width = usable / text_columns;
            if column_width < 1 {
                return Err(PrError::msg(translate!("pr-error-page-width-too-narrow")));
            }
            column_width
        } else {
            width
        };

        let omit_pagination = matches.get_flag(options::OMIT_PAGINATION);
        let header_enabled = !matches.get_flag(options::OMIT_HEADER)
            && !omit_pagination
            && page_length > HEADER_LINES_PER_PAGE + TRAILER_LINES_PER_PAGE;
        let body_lines = if header_enabled {
            page_length - HEADER_LINES_PER_PAGE - TRAILER_LINES_PER_PAGE
        } else {
            page_length
        };

        let control_chars = if matches.get_flag(options::SHOW_NONPRINTING) {
            ControlChars::Octal
        } else if matches.get_flag(options::SHOW_CONTROL_CHARS) {
            ControlChars::Hat
        } else {
            ControlChars::Raw
        };

        // Multi-column output expands input tabs and puts tabs back on output.
        // When columns are joined with a TAB, input tabs are left as they are.
        let expand = expand
            .or_else(|| (multi_column && !(join && separator == [TAB])).then_some(default_tab));
        let tabify = tabify.or_else(|| multi_column.then_some(default_tab));

        let header = matches
            .get_one::<OsString>(options::HEADER)
            .map(|h| os_str_as_bytes_lossy(h).into_owned());

        Ok(Self {
            header_enabled,
            body_lines,
            double_space: matches.get_flag(options::DOUBLE_SPACE),
            form_feed: matches.get_flag(options::FORM_FEED),
            // A short page turns off the header like -t does, keeping form feeds even with -T.
            keep_input_form_feeds: !omit_pagination
                || page_length <= HEADER_LINES_PER_PAGE + TRAILER_LINES_PER_PAGE,
            first_page,
            last_page,
            columns,
            across,
            merge,
            join,
            column_width,
            separator,
            page_width: width,
            // A later -w turns truncation of single-column output back off.
            single_width: page_width.filter(|_| {
                !join_lines
                    && matches.index_of(options::COLUMN_WIDTH)
                        < matches.index_of(options::PAGE_WIDTH)
            }),
            merged_single_width,
            margin,
            number,
            first_line_number,
            expand,
            tabify,
            control_chars,
            header,
            date_format: get_date_format(matches),
            quiet: matches.get_flag(options::NO_FILE_WARNINGS),
        })
    }

    /// Number of input lines that make up one column of a page.
    fn rows_per_page(&self) -> usize {
        if self.double_space {
            (self.body_lines / 2).max(1)
        } else {
            self.body_lines
        }
    }
}

fn get_date_format(matches: &ArgMatches) -> String {
    match matches.get_one::<String>(options::DATE_FORMAT) {
        Some(format) => format,
        None => {
            // Replicate behavior from GNU manual.
            if std::env::var("POSIXLY_CORRECT").is_ok()
                // TODO: This needs to be moved to uucore and handled by icu?
                && (std::env::var_os("LC_TIME").as_deref() == Some(OsStr::new("POSIX"))
                    || std::env::var_os("LC_ALL").as_deref() == Some(OsStr::new("POSIX")))
            {
                "%b %e %H:%M %Y"
            } else {
                format::LONG_ISO
            }
        }
    }
    .to_string()
}

/// How a line of input ended.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LineEnd {
    Newline,
    FormFeed,
    Eof,
}

/// An input file, read line by line without holding more than a buffer in memory.
struct Input {
    reader: Box<dyn BufRead>,
    name: OsString,
    /// A newline right after a form feed that ended a line belongs to that form feed.
    skip_newline: bool,
    /// Set once reading failed; the error has been reported.
    failed: bool,
}

/// How a file is named in error messages.
fn display_name(name: &OsStr) -> String {
    if name == FILE_STDIN {
        translate!("pr-standard-input").quote().to_string()
    } else {
        name.maybe_quote().to_string()
    }
}

fn open_input(name: &OsStr, settings: &Settings) -> Option<Input> {
    let quiet = settings.quiet;
    let opened = if name == FILE_STDIN {
        // A stdin that was closed at startup must not read as empty input.
        uucore::signals::ensure_stdin_open()
            .map(|()| Box::new(BufReader::new(stdin())) as Box<dyn BufRead>)
    } else {
        File::open(name).map(|f| Box::new(BufReader::new(f)) as Box<dyn BufRead>)
    };
    let reader = match opened {
        Ok(reader) => reader,
        Err(e) => {
            // -r only silences files that cannot be opened; stdin is always open
            // as far as pr is concerned, so its failure is a read error.
            if !quiet || name == FILE_STDIN {
                show_error!("{}: {}", display_name(name), strip_errno(&e));
            }
            return None;
        }
    };
    Some(Input {
        reader,
        name: name.to_owned(),
        skip_newline: false,
        failed: false,
    })
}

impl Input {
    /// Report a read error, which `-r` does not silence.
    fn report(&mut self, e: &io::Error) {
        show_error!("{}: {}", display_name(&self.name), strip_errno(e));
        self.failed = true;
    }

    /// Look at the next byte without consuming it.
    fn peek(&mut self) -> Option<u8> {
        if self.failed {
            return None;
        }
        loop {
            let buf = match self.reader.fill_buf() {
                Ok(buf) => buf,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    self.report(&e);
                    return None;
                }
            };
            match buf.first() {
                None => return None,
                Some(&NL) if self.skip_newline => {
                    self.skip_newline = false;
                    self.reader.consume(1);
                }
                Some(&b) => {
                    self.skip_newline = false;
                    return Some(b);
                }
            }
        }
    }

    fn at_eof(&mut self) -> bool {
        self.peek().is_none()
    }

    /// Consume a form feed if it comes next.
    fn take_form_feed(&mut self) -> bool {
        if self.peek() == Some(FF) {
            self.reader.consume(1);
            self.skip_newline = true;
            true
        } else {
            false
        }
    }

    /// Pass the bytes of the next line to `f`, without its terminator.
    fn read_line(
        &mut self,
        mut f: impl FnMut(&[u8]) -> Result<(), PrError>,
    ) -> Result<LineEnd, PrError> {
        if self.peek().is_none() {
            return Ok(LineEnd::Eof);
        }
        loop {
            let buf = match self.reader.fill_buf() {
                Ok(buf) => buf,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    self.report(&e);
                    return Ok(LineEnd::Eof);
                }
            };
            if buf.is_empty() {
                return Ok(LineEnd::Eof);
            }
            if let Some(pos) = memchr::memchr2(NL, FF, buf) {
                let end = if buf[pos] == NL {
                    LineEnd::Newline
                } else {
                    LineEnd::FormFeed
                };
                f(&buf[..pos])?;
                self.reader.consume(pos + 1);
                if end == LineEnd::FormFeed {
                    self.skip_newline = true;
                }
                return Ok(end);
            }
            let len = buf.len();
            f(buf)?;
            self.reader.consume(len);
        }
    }
}

/// Destination of rendered text.
trait Sink {
    /// Text where spaces may become tabs, TAB moves to the next tab stop,
    /// backspace moves back one position, and every other byte takes one position.
    fn text(&mut self, bytes: &[u8]) -> Result<(), PrError>;
    fn spaces(&mut self, n: usize) -> Result<(), PrError>;
}

impl Sink for Vec<u8> {
    fn text(&mut self, bytes: &[u8]) -> Result<(), PrError> {
        self.extend_from_slice(bytes);
        Ok(())
    }

    fn spaces(&mut self, n: usize) -> Result<(), PrError> {
        self.resize(self.len() + n, b' ');
        Ok(())
    }
}

/// Writes one output line, keeping track of the output position so that runs
/// of spaces can be replaced by tabs.
struct LineWriter<'a, W: Write> {
    out: &'a mut W,
    position: usize,
    pending_spaces: usize,
    tabify: Option<CharWidth>,
}

impl<'a, W: Write> LineWriter<'a, W> {
    fn new(out: &'a mut W, tabify: Option<CharWidth>) -> Self {
        Self {
            out,
            position: 0,
            pending_spaces: 0,
            tabify,
        }
    }

    fn write_raw(&mut self, bytes: &[u8]) -> Result<(), PrError> {
        self.out.write_all(bytes).map_err(PrError::Write)
    }

    fn write_spaces(&mut self, mut n: usize) -> Result<(), PrError> {
        const SPACES: [u8; 256] = [b' '; 256];
        while n > 0 {
            let chunk = n.min(SPACES.len());
            self.write_raw(&SPACES[..chunk])?;
            n -= chunk;
        }
        Ok(())
    }

    /// Write out the pending run of spaces, using tabs where they reach a tab stop.
    fn flush_spaces(&mut self) -> Result<(), PrError> {
        let n = std::mem::take(&mut self.pending_spaces);
        if n == 0 {
            return Ok(());
        }
        let end = self.position;
        let mut current = end - n;
        if let Some(tab) = self.tabify.filter(|_| n > 1) {
            loop {
                let stop = (current / tab.width + 1) * tab.width;
                if stop > end {
                    break;
                }
                self.write_raw(&[tab.ch])?;
                current = stop;
            }
        }
        self.write_spaces(end - current)
    }

    /// Start a new run of spaces: spaces before this point are not merged with later ones.
    fn break_run(&mut self) -> Result<(), PrError> {
        self.flush_spaces()
    }

    fn pad_to(&mut self, column: usize) -> Result<(), PrError> {
        if column > self.position {
            self.spaces(column - self.position)?;
        }
        Ok(())
    }

    /// Write a column separator; each of its bytes takes one position.
    fn separator(&mut self, sep: &[u8]) -> Result<(), PrError> {
        for &b in sep {
            if b == b' ' {
                self.spaces(1)?;
            } else {
                self.flush_spaces()?;
                self.write_raw(&[b])?;
                self.position += 1;
            }
        }
        Ok(())
    }

    /// Write a TAB that moves to the next tab stop.
    fn tab(&mut self) -> Result<(), PrError> {
        self.flush_spaces()?;
        self.write_raw(&[TAB])?;
        let width = self.tabify.map_or(DEFAULT_TAB_WIDTH, |t| t.width);
        self.position = (self.position / width + 1) * width;
        Ok(())
    }

    fn end_line(&mut self, double_space: bool) -> Result<(), PrError> {
        // When spaces are turned into tabs, trailing spaces are left out.
        if self.tabify.is_some() {
            self.pending_spaces = 0;
        }
        self.flush_spaces()?;
        self.write_raw(if double_space { b"\n\n" } else { b"\n" })?;
        self.position = 0;
        Ok(())
    }
}

impl<W: Write> Sink for LineWriter<'_, W> {
    fn text(&mut self, bytes: &[u8]) -> Result<(), PrError> {
        let mut start = 0;
        for (i, &b) in bytes.iter().enumerate() {
            if b == b' ' {
                if start < i {
                    self.write_raw(&bytes[start..i])?;
                }
                start = i + 1;
                self.pending_spaces += 1;
                self.position += 1;
                continue;
            }
            if self.pending_spaces > 0 {
                if start < i {
                    self.write_raw(&bytes[start..i])?;
                }
                start = i;
                self.flush_spaces()?;
            }
            match b {
                // Tabs in the text are not counted when placing output tabs.
                TAB => {}
                BS => self.position = self.position.saturating_sub(1),
                // Other nonprinting bytes take no room.
                0x20..=0x7e => self.position += 1,
                _ => {}
            }
        }
        if start < bytes.len() {
            self.write_raw(&bytes[start..])?;
        }
        Ok(())
    }

    fn spaces(&mut self, n: usize) -> Result<(), PrError> {
        self.pending_spaces += n;
        self.position += n;
        Ok(())
    }
}

/// Turns the bytes of an input line into output text: expands input tabs,
/// shows nonprinting characters, and truncates the line.
struct Renderer {
    /// Position used for tab expansion and backspacing.
    input_position: usize,
    /// Position used for truncation.
    width_used: usize,
    limit: Option<usize>,
    stopped: bool,
    /// Some of the line was rendered.
    emitted: bool,
    /// Added to the input position once the first byte has been handled.
    deferred_start: usize,
    expand: Option<CharWidth>,
    control_chars: ControlChars,
}

impl Renderer {
    fn new(settings: &Settings, start: usize, width_used: usize, limit: Option<usize>) -> Self {
        Self {
            input_position: start,
            width_used,
            limit,
            stopped: false,
            emitted: false,
            deferred_start: 0,
            expand: settings.expand,
            control_chars: settings.control_chars,
        }
    }

    fn fits(&mut self, width: usize) -> bool {
        if self
            .limit
            .is_some_and(|limit| self.width_used + width > limit)
        {
            self.stopped = true;
            false
        } else {
            true
        }
    }

    fn advance(&mut self, width: usize) -> Result<(), PrError> {
        if self.input_position + width > MAX_INT_VALUE as usize {
            return Err(PrError::msg(translate!("pr-error-integer-overflow")));
        }
        self.input_position += width;
        self.width_used += width;
        self.emitted = true;
        Ok(())
    }

    /// The first character of the line did not fit, so nothing of it is shown.
    /// Such a line gets no line number.
    fn truncated_at_start(&self) -> bool {
        self.stopped && !self.emitted
    }

    fn render(&mut self, bytes: &[u8], sink: &mut impl Sink) -> Result<(), PrError> {
        for &b in bytes {
            if self.stopped {
                break;
            }
            self.render_byte(b, sink)?;
            self.input_position += std::mem::take(&mut self.deferred_start);
        }
        Ok(())
    }

    fn render_byte(&mut self, b: u8, sink: &mut impl Sink) -> Result<(), PrError> {
        if let Some(expand) = self.expand {
            let width = if b == expand.ch {
                expand.width
            } else if b == TAB {
                DEFAULT_TAB_WIDTH
            } else {
                0
            };
            if width > 0 {
                let n = width - self.input_position % width;
                if !self.fits(n) {
                    return Ok(());
                }
                self.advance(n)?;
                return sink.spaces(n);
            }
        }
        match b {
            TAB => {
                // An unexpanded tab is measured against the whole output line.
                let n = DEFAULT_TAB_WIDTH - self.width_used % DEFAULT_TAB_WIDTH;
                if self.fits(n) {
                    self.advance(n)?;
                    sink.text(&[TAB])?;
                }
            }
            BS if self.control_chars == ControlChars::Raw => {
                // A backspace cannot move before the start of the line.
                if self.input_position > 0 {
                    self.emitted = true;
                    self.input_position -= 1;
                    self.width_used = self.width_used.saturating_sub(1);
                    sink.text(&[BS])?;
                }
            }
            0x20..=0x7e => {
                if self.fits(1) {
                    self.advance(1)?;
                    sink.text(&[b])?;
                }
            }
            _ => {
                let mut repr = [0u8; 4];
                let shown: &[u8] = match self.control_chars {
                    ControlChars::Raw => {
                        // Shown as is, and taking no room.
                        return sink.text(&[b]);
                    }
                    ControlChars::Hat if b < 0x20 || b == 0x7f => {
                        repr[0] = b'^';
                        repr[1] = b ^ 0x40;
                        &repr[..2]
                    }
                    ControlChars::Hat | ControlChars::Octal => {
                        repr = [
                            b'\\',
                            b'0' + (b >> 6),
                            b'0' + ((b >> 3) & 7),
                            b'0' + (b & 7),
                        ];
                        &repr[..]
                    }
                };
                if self.fits(shown.len()) {
                    self.advance(shown.len())?;
                    sink.text(shown)?;
                }
            }
        }
        Ok(())
    }
}

/// Header line: date on the left, the title centered, the page number on the right.
fn write_header(
    out: &mut impl Write,
    settings: &Settings,
    date: &[u8],
    title: &[u8],
    page: u64,
) -> Result<(), PrError> {
    let page_text = format!("{} {page}", translate!("pr-page"));
    let blank = settings
        .page_width
        .saturating_sub(date.len() + title.len() + page_text.len());
    let before = (blank / 2).max(1);
    let after = (blank - blank / 2).max(1);
    let mut line = Vec::with_capacity(settings.margin + settings.page_width + 2);
    // The margin also shows up on the first, otherwise blank, line.
    line.resize(settings.margin, b' ');
    line.extend_from_slice(b"\n\n");
    line.resize(line.len() + settings.margin, b' ');
    line.extend_from_slice(date);
    line.resize(line.len() + before, b' ');
    line.extend_from_slice(title);
    line.resize(line.len() + after, b' ');
    line.extend_from_slice(page_text.as_bytes());
    line.extend_from_slice(b"\n\n\n");
    out.write_all(&line).map_err(PrError::Write)
}

/// What is printed around the text of every page.
struct PageFrame {
    date: Vec<u8>,
    title: Vec<u8>,
}

impl PageFrame {
    fn new(settings: &Settings, file: Option<&OsStr>) -> Self {
        let time = match file {
            Some(name) if name != FILE_STDIN => metadata(name).and_then(|m| m.modified()).ok(),
            _ => Some(SystemTime::now()),
        };
        let date = time
            .and_then(|time| {
                let mut v = Vec::new();
                format_system_time(
                    &mut v,
                    time,
                    &settings.date_format,
                    FormatSystemTimeFallback::Integer,
                )
                .ok()
                .map(|()| v)
            })
            .unwrap_or_default();
        let title = match (&settings.header, file) {
            (Some(header), _) => header.clone(),
            (None, Some(name)) if name != FILE_STDIN => os_str_as_bytes_lossy(name).into_owned(),
            _ => Vec::new(),
        };
        Self { date, title }
    }

    fn header(&self, out: &mut impl Write, settings: &Settings, page: u64) -> Result<(), PrError> {
        if settings.header_enabled {
            write_header(out, settings, &self.date, &self.title, page)?;
        }
        Ok(())
    }

    /// Finish a page.
    fn footer(out: &mut impl Write, settings: &Settings, end: PageEnd) -> Result<(), PrError> {
        let rows = end.rows;
        let text = if settings.header_enabled {
            if settings.form_feed {
                // An empty page still gets a blank line, and with double spacing
                // a form feed where a row would start counts as half a row.
                match (rows, settings.double_space) {
                    (0, false) => &b"\n\x0c"[..],
                    (0, true) => &b"\n\n\x0c"[..],
                    (_, true) if end.at_row_start => &b"\n\x0c"[..],
                    _ => &b"\x0c"[..],
                }
            } else {
                // With double spacing, an odd last text line of the page stays unused.
                let missing = settings.rows_per_page().saturating_sub(rows);
                let blank =
                    missing * if settings.double_space { 2 } else { 1 } + TRAILER_LINES_PER_PAGE;
                let mut w = LineWriter::new(out, None);
                for _ in 0..blank {
                    w.write_raw(b"\n")?;
                }
                return Ok(());
            }
        } else if end.form_feed && settings.keep_input_form_feeds {
            &b"\x0c"[..]
        } else {
            &b""[..]
        };
        out.write_all(text).map_err(PrError::Write)
    }
}

/// How a page of input ended.
#[derive(Default, Clone, Copy)]
struct PageEnd {
    /// Rows of text on the page.
    rows: usize,
    /// A form feed in the input ended the page.
    form_feed: bool,
    /// The page ended, by a form feed or the end of the input, where the next
    /// row would have started.
    at_row_start: bool,
}

impl PageEnd {
    fn form_feed(rows: usize, at_row_start: bool) -> Self {
        Self {
            rows,
            form_feed: true,
            at_row_start,
        }
    }

    fn full(rows: usize) -> Self {
        Self {
            rows,
            ..Self::default()
        }
    }
}

/// Tracks page and line numbers while reading a file.
struct Counter {
    page: u64,
    line: i64,
    started: bool,
    /// The last page read was ended by a form feed, which starts another (empty) page.
    form_feed_at_end: bool,
}

impl Counter {
    fn new() -> Self {
        Self {
            page: 0,
            line: 1,
            started: false,
            form_feed_at_end: false,
        }
    }

    /// Move to the next page; returns whether it is printed, or `None` past the last page.
    fn next_page(&mut self, settings: &Settings) -> Option<bool> {
        self.page += 1;
        if settings.last_page.is_some_and(|last| self.page > last) {
            return None;
        }
        let selected = self.page >= settings.first_page;
        if selected && !self.started {
            self.started = true;
            if let Some(n) = settings.first_line_number {
                self.line = n;
            }
        }
        Some(selected)
    }

    fn report_short_input(&self, settings: &Settings) {
        let count = (self.page + u64::from(self.form_feed_at_end)).max(1);
        if !self.started && settings.first_page > count {
            show_error!(
                "{}",
                translate!(
                    "pr-error-starting-page-exceeds-page-count",
                    "start" => settings.first_page,
                    "count" => count
                )
            );
        }
    }
}

/// Print one file on its own. Returns false if reading it failed.
fn print_file(
    mut input: Input,
    name: &OsStr,
    settings: &Settings,
    out: &mut impl Write,
) -> Result<bool, PrError> {
    // Output for earlier files goes out before any error about this one.
    out.flush().map_err(PrError::Write)?;
    let frame = PageFrame::new(settings, (!settings.merge).then_some(name));
    let mut counter = Counter::new();
    let mut reached_last = false;
    while !input.at_eof() {
        let Some(selected) = counter.next_page(settings) else {
            reached_last = true;
            break;
        };
        if selected {
            frame.header(out, settings, counter.page)?;
        }
        let end = if settings.columns > 1 {
            print_columns_page(&mut input, settings, out, selected, &mut counter.line)?
        } else {
            print_single_page(&mut input, settings, out, selected, &mut counter.line)?
        };
        if selected {
            PageFrame::footer(out, settings, end)?;
        }
        counter.form_feed_at_end = end.form_feed;
    }
    if !reached_last && !input.failed {
        counter.report_short_input(settings);
    }
    Ok(!input.failed)
}

/// Print one page of single-column output, streaming each line.
fn print_single_page(
    input: &mut Input,
    settings: &Settings,
    out: &mut impl Write,
    selected: bool,
    line_number: &mut i64,
) -> Result<PageEnd, PrError> {
    let rows = settings.rows_per_page();
    let number_width = settings.number.map_or(0, |n| n.field_width());
    let limit = settings.single_width.map(|w| w.max(number_width + 1));
    let mut sink = Vec::new();
    let mut lines = 0;
    while lines < rows {
        match input.peek() {
            None => {
                return Ok(PageEnd {
                    rows: lines,
                    form_feed: false,
                    at_row_start: true,
                });
            }
            Some(FF) => {
                input.take_form_feed();
                return Ok(PageEnd::form_feed(lines, true));
            }
            Some(_) => {}
        }
        let mut renderer = Renderer::new(settings, 0, number_width, limit);
        let end = if selected {
            let mut line = PrefixedLine {
                writer: LineWriter::new(out, settings.tabify),
                prefix: Some((settings.margin, settings.number, *line_number)),
            };
            let end = input.read_line(|bytes| renderer.render(bytes, &mut line))?;
            if !renderer.truncated_at_start() {
                line.write_prefix()?;
                // Without a header, a line ended by a form feed or by the end
                // of the input is not double spaced.
                let double =
                    settings.double_space && (settings.header_enabled || end == LineEnd::Newline);
                line.writer.end_line(double)?;
            }
            end
        } else {
            input.read_line(|bytes| {
                // Rendering is still needed to detect position overflows.
                sink.clear();
                renderer.render(bytes, &mut sink)
            })?
        };
        if renderer.truncated_at_start() {
            // Nothing of the line fits: it is left out altogether.
            if end == LineEnd::FormFeed {
                return Ok(PageEnd::form_feed(lines, true));
            }
            continue;
        }
        *line_number += 1;
        lines += 1;
        match end {
            LineEnd::FormFeed => return Ok(PageEnd::form_feed(lines, false)),
            LineEnd::Eof => return Ok(PageEnd::full(lines)),
            LineEnd::Newline => {}
        }
    }
    input.take_form_feed();
    Ok(PageEnd::full(lines))
}

/// A single-column output line whose margin and line number are written
/// once there is something to print.
struct PrefixedLine<'a, W: Write> {
    writer: LineWriter<'a, W>,
    prefix: Option<(usize, Option<Numbering>, i64)>,
}

impl<W: Write> PrefixedLine<'_, W> {
    fn write_prefix(&mut self) -> Result<(), PrError> {
        if let Some((margin, number, n)) = self.prefix.take() {
            self.writer.spaces(margin)?;
            if let Some(numbering) = number {
                self.writer.text(&numbering.format(n))?;
                if numbering.separator == TAB {
                    self.writer.tab()?;
                } else {
                    self.writer.text(&[numbering.separator])?;
                }
            }
        }
        Ok(())
    }
}

impl<W: Write> Sink for PrefixedLine<'_, W> {
    fn text(&mut self, bytes: &[u8]) -> Result<(), PrError> {
        self.write_prefix()?;
        self.writer.text(bytes)
    }

    fn spaces(&mut self, n: usize) -> Result<(), PrError> {
        self.write_prefix()?;
        self.writer.spaces(n)
    }
}

/// A rendered line of input, to be printed as one text column.
#[derive(Default)]
struct Cell {
    bytes: Vec<u8>,
    /// Width of the text, not counting the line number.
    text_width: usize,
    /// Nothing of the line fit in the column.
    truncated_at_start: bool,
}

impl Numbering {
    /// The number as it starts a text column: a TAB separator becomes spaces.
    fn column_field(&self, number: i64) -> Vec<u8> {
        let mut field = self.format(number);
        if self.separator == TAB {
            field.resize(self.field_width(), b' ');
        } else {
            field.push(self.separator);
        }
        field
    }
}

/// Render the next line of `input` as a text column, preceded by its number if
/// `number` is given. In aligned output the cell, number included, is truncated
/// to `width`. Also returns whether the line was numbered: a line of which
/// nothing fits is shown empty and without a number.
fn read_cell(
    input: &mut Input,
    settings: &Settings,
    number: Option<(Numbering, i64)>,
    width: usize,
) -> Result<(Cell, LineEnd, bool), PrError> {
    let (mut bytes, start) = match number {
        Some((numbering, n)) => (numbering.column_field(n), numbering.field_width()),
        None => (Vec::new(), 0),
    };
    let limit = (!settings.join).then(|| width.max(start + 1));
    // Tabs are expanded from the start of the text, after the line number.
    // In columns of a single file the number counts too, but only from the
    // second byte of the line on.
    let mut renderer = Renderer::new(settings, 0, start, limit);
    if !settings.merge {
        renderer.deferred_start = start;
    }
    let end = input.read_line(|line| renderer.render(line, &mut bytes))?;
    if renderer.truncated_at_start() {
        let cell = Cell {
            truncated_at_start: true,
            ..Cell::default()
        };
        return Ok((cell, end, false));
    }
    let text_width = renderer.width_used.saturating_sub(start);
    let cell = Cell {
        bytes,
        text_width,
        truncated_at_start: false,
    };
    Ok((cell, end, true))
}

/// Write one row of text columns. `columns` holds the cells in order; a `None`
/// cell is an empty column that is still padded and separated.
fn write_row(
    out: &mut impl Write,
    settings: &Settings,
    columns: &[Option<&Cell>],
    first_column_extra: usize,
    double_space: bool,
    trailing_separator: bool,
) -> Result<(), PrError> {
    let mut w = LineWriter::new(out, settings.tabify);
    w.spaces(settings.margin)?;
    let separator_width = settings.separator.len();
    let step = settings.column_width + separator_width;
    // Joined columns printed down the page count tab stops after a column
    // separator as if the column before it had started at the left edge
    // (after the margin, for the first column), unless it ended in spaces.
    let restart_positions = settings.join && !settings.merge && !settings.across;
    for (i, cell) in columns.iter().enumerate() {
        // Across the page, a column that shows nothing of its line does not
        // start a new run of spaces.
        if !(settings.across && cell.is_some_and(|c| c.truncated_at_start)) {
            w.break_run()?;
        }
        if let Some(cell) = cell {
            w.text(&cell.bytes)?;
        }
        if i + 1 < columns.len() {
            if !settings.join {
                let end = settings.margin + first_column_extra + i * step + settings.column_width;
                w.pad_to(end)?;
            }
            let restart = restart_positions && w.pending_spaces == 0;
            w.separator(&settings.separator)?;
            if restart {
                w.break_run()?;
                let margin = if i == 0 { settings.margin } else { 0 };
                w.position = margin + cell.map_or(0, |c| c.text_width) + separator_width;
            }
        }
    }
    if trailing_separator && !columns.is_empty() {
        // A column that fills its width is followed by its separator even when
        // the columns after it are left out.
        let i = columns.len() - 1;
        let end = settings.margin + first_column_extra + i * step + settings.column_width;
        if w.position >= end {
            w.break_run()?;
            w.write_raw(&settings.separator)?;
            w.position += separator_width;
        }
    }
    w.end_line(double_space)
}

/// Print one page of multi-column output for a single file.
fn print_columns_page(
    input: &mut Input,
    settings: &Settings,
    out: &mut impl Write,
    selected: bool,
    line_number: &mut i64,
) -> Result<PageEnd, PrError> {
    let rows = settings.rows_per_page();
    let columns = settings.columns;
    let capacity = rows * columns;
    let mut cells: Vec<Cell> = Vec::new();
    let mut ended_by_form_feed = false;
    let mut at_row_start = false;
    let mut last_line_form_feed = false;
    let mut last_line_unterminated = false;
    while cells.len() < capacity {
        match input.peek() {
            None => {
                at_row_start = settings.across
                    && cells.len().is_multiple_of(columns)
                    && !last_line_unterminated;
                break;
            }
            Some(FF) => {
                input.take_form_feed();
                ended_by_form_feed = true;
                // Only output across the page is printed as it is read.
                at_row_start = settings.across && cells.len().is_multiple_of(columns);
                break;
            }
            Some(_) => {}
        }
        let number = settings.number.map(|n| (n, *line_number));
        let (cell, end, numbered) = read_cell(input, settings, number, settings.column_width)?;
        if !numbered && end == LineEnd::FormFeed {
            // Nothing of this line shows, so the form feed acts as if it began the line.
            ended_by_form_feed = true;
            at_row_start = settings.across && cells.len().is_multiple_of(columns);
            break;
        }
        if numbered {
            *line_number += 1;
        }
        cells.push(if selected { cell } else { Cell::default() });
        last_line_unterminated = end == LineEnd::Eof;
        if end == LineEnd::FormFeed {
            last_line_form_feed = true;
            ended_by_form_feed = true;
            break;
        }
    }
    if cells.len() == capacity && !ended_by_form_feed {
        input.take_form_feed();
    }
    let end = |rows| PageEnd {
        rows,
        form_feed: ended_by_form_feed,
        at_row_start,
    };
    if !selected {
        return Ok(end(0));
    }

    let mut table: Vec<Vec<Option<&Cell>>> = Vec::new();
    if settings.across {
        for chunk in cells.chunks(columns) {
            table.push(chunk.iter().map(Some).collect());
        }
    } else {
        // Columns are filled top to bottom. On a page that is not full, each
        // column takes its share of what is left, so the columns are balanced.
        let mut bounds = vec![0];
        let mut placed = 0;
        for column in 0..columns {
            let len = if cells.len() == capacity {
                rows
            } else {
                (cells.len() - placed).div_ceil(columns - column)
            };
            placed += len;
            bounds.push(placed);
        }
        let height = bounds[1];
        for row in 0..height {
            let cells_in_row: Vec<_> = (0..columns)
                .take_while(|&c| bounds[c] + row < bounds[c + 1])
                .map(|c| Some(&cells[bounds[c] + row]))
                .collect();
            table.push(cells_in_row);
        }
    }
    // Without a header, the last row is not double spaced when its last line
    // ended with a form feed, or when the page (down) or the row (across)
    // is not full.
    let short_end = last_line_form_feed
        || if settings.across {
            !cells.len().is_multiple_of(columns)
        } else {
            cells.len() < capacity
        };
    for (i, row) in table.iter().enumerate() {
        let last = i + 1 == table.len();
        let double = settings.double_space && (settings.header_enabled || !(last && short_end));
        write_row(out, settings, row, 0, double, false)?;
    }
    Ok(end(table.len()))
}

/// State of one file while merging.
struct MergeInput {
    input: Input,
    /// The file has no more lines for the current page.
    done: bool,
    ended_by_form_feed: bool,
}

/// Print all files side by side, one per column.
fn print_merged(
    files: &[OsString],
    settings: &Settings,
    out: &mut impl Write,
    failed: &mut bool,
) -> Result<(), PrError> {
    let mut inputs: Vec<MergeInput> = Vec::new();
    for file in files {
        match open_input(file, settings) {
            Some(input) => inputs.push(MergeInput {
                input,
                done: false,
                ended_by_form_feed: false,
            }),
            None => *failed = true,
        }
    }
    if inputs.len() < 2 {
        // Merging a single file is printing it, but truncated to any width given.
        let settings = Settings {
            single_width: settings.merged_single_width,
            ..settings.clone()
        };
        for m in inputs {
            let name = m.input.name.clone();
            *failed |= !print_file(m.input, &name, &settings, out)?;
        }
        return Ok(());
    }

    let frame = PageFrame::new(settings, None);
    let rows = settings.rows_per_page();
    let number_width = settings.number.map_or(0, |n| n.field_width());
    let mut counter = Counter::new();
    let mut reached_last = false;
    while inputs.iter_mut().any(|m| !m.input.at_eof()) {
        let Some(selected) = counter.next_page(settings) else {
            reached_last = true;
            break;
        };
        if selected {
            frame.header(out, settings, counter.page)?;
        }
        for m in &mut inputs {
            m.done = false;
            m.ended_by_form_feed = false;
        }
        let mut lines = 0;
        let mut at_row_start = false;
        // None of the lines on the previous row had a newline.
        let mut row_unterminated = false;
        while lines < rows {
            for m in &mut inputs {
                if m.done {
                    continue;
                }
                match m.input.peek() {
                    None => m.done = true,
                    Some(FF) => {
                        m.input.take_form_feed();
                        m.done = true;
                        m.ended_by_form_feed = true;
                    }
                    Some(_) => {}
                }
            }
            if inputs.iter().all(|m| m.done) {
                at_row_start = !row_unterminated;
                break;
            }
            row_unterminated = true;
            let mut cells: Vec<Option<Cell>> = Vec::with_capacity(inputs.len());
            for (i, m) in inputs.iter_mut().enumerate() {
                if m.done {
                    cells.push(None);
                    continue;
                }
                let number = if i == 0 {
                    settings.number.map(|n| (n, counter.line))
                } else {
                    None
                };
                let width = settings.column_width + if i == 0 { number_width } else { 0 };
                let (cell, end, _) = read_cell(&mut m.input, settings, number, width)?;
                match end {
                    LineEnd::FormFeed => {
                        m.done = true;
                        m.ended_by_form_feed = true;
                    }
                    LineEnd::Eof => m.done = true,
                    LineEnd::Newline => {}
                }
                row_unterminated &= end == LineEnd::Eof;
                cells.push(Some(cell));
            }
            let first_has_line = cells[0].is_some();
            if selected {
                if let Some(numbering) = settings.number.filter(|_| !first_has_line) {
                    // The number goes in front of the first column even when that file has
                    // nothing more on this page.
                    cells[0] = Some(Cell {
                        bytes: numbering.column_field(counter.line),
                        ..Cell::default()
                    });
                }
                let mut row: Vec<Option<&Cell>> = cells.iter().map(Option::as_ref).collect();
                // On the last row of a page, empty columns at the end get no
                // separators; on the first row only the very last one is left out.
                let mut dropped = 0;
                if inputs.iter().all(|m| m.done) {
                    while matches!(row.last(), Some(None)) {
                        row.pop();
                        dropped += 1;
                        if lines == 0 {
                            break;
                        }
                    }
                }
                let trailing_separator = dropped > 1 && !settings.join;
                write_row(
                    out,
                    settings,
                    &row,
                    number_width,
                    settings.double_space,
                    trailing_separator,
                )?;
            }
            // While pages are skipped, only the lines of the first file are counted.
            if selected || first_has_line {
                counter.line += 1;
            }
            lines += 1;
        }
        if lines == rows {
            for m in &mut inputs {
                if !m.done {
                    m.input.take_form_feed();
                }
            }
        }
        let end = PageEnd {
            rows: lines,
            form_feed: inputs.iter().any(|m| m.ended_by_form_feed),
            at_row_start,
        };
        if selected {
            PageFrame::footer(out, settings, end)?;
        }
        counter.form_feed_at_end = end.form_feed;
    }
    if !reached_last {
        counter.report_short_input(settings);
    }
    *failed |= inputs.iter().any(|m| m.input.failed);
    Ok(())
}
