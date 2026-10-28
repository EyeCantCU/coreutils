// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use std::env;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io::{self, IsTerminal, Write, stdout};

use clap::builder::PossibleValue;
use clap::{Arg, ArgAction, Command};

use uucore::display::{OsWrite, Quotable};
use uucore::error::UResult;
use uucore::i18n::{UEncoding, get_ctype_encoding};
use uucore::line_ending::LineEnding;
use uucore::parser::shortcut_value_parser::ShortcutValueParser;
use uucore::quoting_style::{QuotingStyle, escape_name};
use uucore::{format_usage, os_str_as_bytes_lossy, show_error, translate};

static OPT_NULL: &str = "null";
static OPT_QUOTING_STYLE: &str = "quoting-style";

static ARG_VARIABLES: &str = "variables";

#[uucore::main(no_signals)]
pub fn uumain(args: impl uucore::Args) -> UResult<()> {
    let matches = uucore::clap_localization::handle_clap_result_with_exit_code(uu_app(), args, 2)?;

    let variables: Vec<String> = matches
        .get_many::<String>(ARG_VARIABLES)
        .map(|v| v.map(ToString::to_string).collect())
        .unwrap_or_default();

    let separator = LineEnding::from_zero_flag(matches.get_flag(OPT_NULL));
    let style = quoting_style(&matches);

    if variables.is_empty() {
        let mut stdout = stdout().lock();
        for (name, value) in env::vars_os() {
            write_quoted(&mut stdout, &name, style)?;
            stdout.write_all(b"=")?;
            write_quoted(&mut stdout, &value, style)?;
            write!(stdout, "{separator}")?;
        }
        return Ok(());
    }

    let mut error_found = false;
    for env_var in variables {
        // we silently ignore a=b as variable but we trigger an error
        if env_var.contains('=') {
            error_found = true;
            continue;
        }
        if let Some(var) = env::var_os(env_var) {
            let mut stdout = stdout().lock();
            write_quoted(&mut stdout, &var, style)?;
            write!(stdout, "{separator}")?;
        } else {
            error_found = true;
        }
    }

    if error_found { Err(1.into()) } else { Ok(()) }
}

/// How printenv quotes its output.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    /// A style handled by uucore's quoting.
    Quoting(QuotingStyle),
    /// Backslash escapes, without quotes, leaving spaces alone.
    Escape,
    /// Locale quotation marks: `‘…’` in UTF-8, otherwise `'…'` (`"…"` for clocale).
    Locale { double: bool },
}

/// Parse a quoting style name, as given to `--quoting-style` or in `QUOTING_STYLE`.
fn parse_style(name: &str) -> Option<Style> {
    Some(Style::Quoting(match name {
        "literal" => QuotingStyle::Literal { show_control: true },
        "shell" => QuotingStyle::SHELL,
        "shell-always" => QuotingStyle::SHELL_QUOTE,
        "shell-escape" => QuotingStyle::SHELL_ESCAPE,
        "shell-escape-always" => QuotingStyle::SHELL_ESCAPE_QUOTE,
        "c" | "c-maybe" => QuotingStyle::C_DOUBLE,
        "escape" => return Some(Style::Escape),
        "locale" => return Some(Style::Locale { double: false }),
        "clocale" => return Some(Style::Locale { double: true }),
        _ => return None,
    }))
}

/// Output is quoted when `--quoting-style` is given, or when standard output
/// is a terminal, in which case `QUOTING_STYLE` (default `shell-escape`)
/// picks the style. Otherwise it is written as-is.
fn quoting_style(matches: &clap::ArgMatches) -> Option<Style> {
    if let Some(style) = matches.get_one::<String>(OPT_QUOTING_STYLE) {
        return parse_style(style);
    }
    if !stdout().is_terminal() {
        return None;
    }
    let from_env = env::var_os("QUOTING_STYLE").and_then(|value| {
        let style = value.to_str().and_then(parse_style);
        if style.is_none() {
            show_error!(
                "{}",
                translate!("printenv-invalid-quoting-style-env-var", "invalid" => value.quote())
            );
        }
        style
    });
    // Control characters are shown rather than replaced by '?'.
    Some(from_env.unwrap_or(Style::Quoting(
        QuotingStyle::SHELL_ESCAPE.show_control(true),
    )))
}

fn write_quoted(out: &mut impl OsWrite, s: &OsStr, style: Option<Style>) -> io::Result<()> {
    let Some(style) = style else {
        return out.write_all_os(s);
    };
    let encoding = get_ctype_encoding();
    match style {
        Style::Quoting(style) => {
            out.write_all_os(&escape_name(s, style.show_control(true), encoding))
        }
        Style::Escape => {
            // Unlike ls, printenv does not backslash-escape spaces in this style.
            let bytes = os_str_as_bytes_lossy(s);
            for (i, piece) in bytes.split(|&b| b == b' ').enumerate() {
                if i > 0 {
                    out.write_all(b" ")?;
                }
                let mut escaped = String::new();
                push_escaped(&mut escaped, piece, encoding, None);
                out.write_all(escaped.as_bytes())?;
            }
            Ok(())
        }
        Style::Locale { double } => {
            let bytes = os_str_as_bytes_lossy(s);
            let (open, close, quote) = match (encoding, double) {
                (UEncoding::Utf8, _) => ('\u{2018}', '\u{2019}', None),
                (_, false) => ('\'', '\'', Some('\'')),
                (_, true) => ('"', '"', Some('"')),
            };
            let mut quoted = String::from(open);
            push_escaped(&mut quoted, &bytes, encoding, quote);
            quoted.push(close);
            out.write_all(quoted.as_bytes())
        }
    }
}

/// Append `bytes` with backslash escapes for backslashes, control characters,
/// bytes that are not valid in `encoding`, and `quote`, if given.
fn push_escaped(buf: &mut String, bytes: &[u8], encoding: UEncoding, quote: Option<char>) {
    let push_byte = |buf: &mut String, byte: u8| {
        let _ = write!(buf, "\\{byte:03o}");
    };
    let push_char = |buf: &mut String, c: char| match c {
        '\\' => buf.push_str("\\\\"),
        '\x07' => buf.push_str("\\a"),
        '\x08' => buf.push_str("\\b"),
        '\t' => buf.push_str("\\t"),
        '\n' => buf.push_str("\\n"),
        '\x0b' => buf.push_str("\\v"),
        '\x0c' => buf.push_str("\\f"),
        '\r' => buf.push_str("\\r"),
        c if Some(c) == quote => {
            buf.push('\\');
            buf.push(c);
        }
        c if c.is_control() => {
            let mut utf8 = [0u8; 4];
            for &byte in c.encode_utf8(&mut utf8).as_bytes() {
                push_byte(buf, byte);
            }
        }
        c => buf.push(c),
    };
    if encoding == UEncoding::Utf8 {
        for chunk in bytes.utf8_chunks() {
            chunk.valid().chars().for_each(|c| push_char(buf, c));
            chunk.invalid().iter().for_each(|&b| push_byte(buf, b));
        }
    } else {
        for &byte in bytes {
            if byte.is_ascii() {
                push_char(buf, byte as char);
            } else {
                push_byte(buf, byte);
            }
        }
    }
}

pub fn uu_app() -> Command {
    let cmd = Command::new("printenv")
        .version(uucore::crate_version!())
        .about(translate!("printenv-about"))
        .override_usage(format_usage(&translate!("printenv-usage")))
        .infer_long_args(true);
    uucore::clap_localization::configure_localized_command(cmd)
        .arg(
            Arg::new(OPT_NULL)
                .short('0')
                .long(OPT_NULL)
                .help(translate!("printenv-help-null"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(OPT_QUOTING_STYLE)
                .long(OPT_QUOTING_STYLE)
                .value_name("WORD")
                .help(translate!("printenv-help-quoting-style"))
                .value_parser(ShortcutValueParser::new([
                    PossibleValue::new("literal"),
                    PossibleValue::new("locale"),
                    PossibleValue::new("shell"),
                    PossibleValue::new("shell-escape"),
                    PossibleValue::new("shell-always"),
                    PossibleValue::new("shell-escape-always"),
                    PossibleValue::new("clocale"),
                    PossibleValue::new("c"),
                    PossibleValue::new("c-maybe"),
                    PossibleValue::new("escape"),
                ])),
        )
        .arg(
            Arg::new(ARG_VARIABLES)
                .action(ArgAction::Append)
                .num_args(1..),
        )
}
