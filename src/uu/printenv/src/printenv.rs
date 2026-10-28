// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use std::env;
use std::ffi::OsStr;
use std::io::{self, IsTerminal, Write, stdout};

use clap::builder::PossibleValue;
use clap::{Arg, ArgAction, Command};

use uucore::display::OsWrite;
use uucore::error::UResult;
use uucore::i18n::get_ctype_encoding;
use uucore::line_ending::LineEnding;
use uucore::parser::shortcut_value_parser::ShortcutValueParser;
use uucore::quoting_style::{QuotingStyle, escape_bytes, quoting_style_from_env};
use uucore::{format_usage, os_str_as_bytes_lossy, translate};

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

/// Output is quoted when `--quoting-style` is given, or when standard output
/// is a terminal, in which case `QUOTING_STYLE` (default `shell-escape`)
/// picks the style. Otherwise it is written as-is.
fn quoting_style(matches: &clap::ArgMatches) -> Option<QuotingStyle> {
    let style = if let Some(style) = matches.get_one::<String>(OPT_QUOTING_STYLE) {
        QuotingStyle::parse(style)?
    } else if stdout().is_terminal() {
        quoting_style_from_env().unwrap_or(QuotingStyle::SHELL_ESCAPE)
    } else {
        return None;
    };
    // Control characters are shown rather than replaced by '?'.
    Some(style.show_control(true))
}

fn write_quoted(out: &mut impl OsWrite, s: &OsStr, style: Option<QuotingStyle>) -> io::Result<()> {
    let Some(style) = style else {
        return out.write_all_os(s);
    };
    let encoding = get_ctype_encoding();
    let bytes = os_str_as_bytes_lossy(s);
    if style == QuotingStyle::Escape {
        // Unlike ls, printenv does not backslash-escape spaces in this style.
        for (i, piece) in bytes.split(|&b| b == b' ').enumerate() {
            if i > 0 {
                out.write_all(b" ")?;
            }
            out.write_all(&escape_bytes(piece, style, encoding))?;
        }
        Ok(())
    } else {
        out.write_all(&escape_bytes(&bytes, style, encoding))
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
