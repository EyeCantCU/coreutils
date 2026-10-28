// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

// spell-checker:ignore isdst fortnight fortnights wednes thur thurs sept twelfth timespec glued

//! Diagnostics printed by `date --debug`.
//!
//! The parsed value always comes from `parse_datetime`. This module only
//! classifies the pieces of the input string, so that it can explain step by
//! step how that value is reached. If that explanation does not arrive at the
//! instant the real parser produced, only the final result is reported.

use std::fmt::Write as _;

use jiff::civil::{Date, DateTime, Time};
use jiff::tz::{AmbiguousOffset, Offset, TimeZone};
use jiff::{SignedDuration, Timestamp, Zoned};
use parse_datetime::ParsedDateTime;

use super::FIXED_OFFSET_ABBREVIATIONS;

const WEEKDAY_ABBREVIATIONS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Print diagnostic lines to stderr, each prefixed with the utility name.
///
/// A failed write makes `date` exit with status 1, as for other diagnostics.
pub(crate) fn emit<S: AsRef<str>>(lines: &[S]) {
    let name = uucore::util_name();
    let mut text = String::new();
    for line in lines {
        let _ = writeln!(text, "{name}: {}", line.as_ref());
    }
    uucore::error::write_stderr(&text);
}

/// Explain how `input` was interpreted. `parsed` is the value the parser
/// produced, or `None` when parsing failed.
pub(crate) fn explain(input: &str, parsed: Option<&ParsedDateTime>, now: &Zoned, utc: bool) {
    let (tz_rule, rest) = split_tz_rule(input);
    let rule_zone =
        tz_rule.and_then(|name| TimeZone::get(name).or_else(|_| TimeZone::posix(name)).ok());
    let zone = rule_zone.clone().unwrap_or_else(|| now.time_zone().clone());
    let env = Environment {
        now,
        utc,
        tz_rule: tz_rule.filter(|_| rule_zone.is_some()),
        zone: &zone,
    };

    let outcome = tokenize(rest)
        .and_then(|tokens| Parser::new(&tokens, &env).run())
        .and_then(|items| env.evaluate(&items));

    match parsed {
        Some(ParsedDateTime::InRange(date)) => {
            if let Some(Outcome::Valid { lines, result }) = outcome
                && result == date.timestamp()
            {
                emit(&lines);
            }
            emit(&env.final_lines(date.timestamp()));
        }
        Some(ParsedDateTime::Extended(_)) => {}
        None => {
            if let Some(Outcome::Invalid { lines }) = outcome {
                emit(&lines);
            }
        }
    }
}

/// Split a leading `TZ="..."` rule from the rest of the input.
fn split_tz_rule(input: &str) -> (Option<&str>, &str) {
    let trimmed = input.trim_start();
    if let Some(rest) = trimmed.strip_prefix("TZ=\"")
        && let Some(end) = rest.find('"')
    {
        return (Some(&rest[..end]), &rest[end + 1..]);
    }
    (None, input)
}

/// `+HH`, or `+HH:MM` (and `:SS`) when the offset is not a whole hour.
fn format_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let abs = seconds.unsigned_abs();
    let (hours, minutes, secs) = (abs / 3600, abs / 60 % 60, abs % 60);
    match (minutes, secs) {
        (0, 0) => format!("{sign}{hours:02}"),
        (_, 0) => format!("{sign}{hours:02}:{minutes:02}"),
        _ => format!("{sign}{hours:02}:{minutes:02}:{secs:02}"),
    }
}

fn format_hms(time: Time) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        time.hour(),
        time.minute(),
        time.second()
    )
}

fn format_ymd(year: i64, month: i64, day: i64) -> String {
    format!("{year:04}-{month:02}-{day:02}")
}

fn format_datetime(dt: DateTime) -> String {
    format!(
        "{} {}",
        format_ymd(dt.year().into(), dt.month().into(), dt.day().into()),
        format_hms(dt.time())
    )
}

fn days(n: i64) -> Option<SignedDuration> {
    SignedDuration::try_from_hours(n.checked_mul(24)?)
}

/// Whole seconds (rounded down) and the non-negative nanosecond remainder.
fn split_timestamp(ts: Timestamp) -> (i64, i32) {
    let (secs, nanos) = (ts.as_second(), ts.subsec_nanosecond());
    if nanos < 0 {
        (secs - 1, nanos + 1_000_000_000)
    } else {
        (secs, nanos)
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// Value and number of digits
    Num(i64, usize),
    /// Lowercase letters, periods removed
    Word(String),
    Punct(char),
}

struct Token {
    tok: Tok,
    /// Whether whitespace precedes this token
    spaced: bool,
}

fn tokenize(input: &str) -> Option<Vec<Token>> {
    let bytes = input.as_bytes();
    let mut tokens = Vec::new();
    let mut spaced = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            spaced = true;
            i += 1;
            continue;
        }
        let start = i;
        let tok = if c.is_ascii_digit() {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            Tok::Num(input[start..i].parse().ok()?, i - start)
        } else if c.is_ascii_alphabetic() {
            while i < bytes.len() && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'.') {
                i += 1;
            }
            Tok::Word(input[start..i].replace('.', "").to_ascii_lowercase())
        } else if c.is_ascii() {
            i += 1;
            Tok::Punct(c as char)
        } else {
            return None;
        };
        tokens.push(Token { tok, spaced });
        spaced = false;
    }
    Some(tokens)
}

fn month_number(word: &str) -> Option<i8> {
    const MONTHS: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    if word == "sept" {
        return Some(9);
    }
    MONTHS
        .iter()
        .position(|m| *m == word || (word.len() == 3 && m.starts_with(word)))
        .map(|i| i as i8 + 1)
}

/// Day of the week, counted from Sunday = 0.
fn weekday_number(word: &str) -> Option<i8> {
    const DAYS: [&str; 7] = [
        "sunday",
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
    ];
    if matches!(word, "tues" | "wednes" | "thur" | "thurs") {
        return Some(&word[..3]).and_then(weekday_number);
    }
    DAYS.iter()
        .position(|d| *d == word || (word.len() == 3 && d.starts_with(word)))
        .map(|i| i as i8)
}

fn ordinal_number(word: &str) -> Option<i64> {
    Some(match word {
        "last" => -1,
        "this" => 0,
        "next" | "first" => 1,
        "third" => 3,
        "fourth" => 4,
        "fifth" => 5,
        "sixth" => 6,
        "seventh" => 7,
        "eighth" => 8,
        "ninth" => 9,
        "tenth" => 10,
        "eleventh" => 11,
        "twelfth" => 12,
        _ => return None,
    })
}

fn ordinal_label(ordinal: i64) -> Option<&'static str> {
    Some(match ordinal {
        -1 => "last",
        0 => "this",
        1 => "next/first",
        2 => "(SECOND)",
        3 => "third",
        4 => "fourth",
        5 => "fifth",
        6 => "sixth",
        7 => "seventh",
        8 => "eighth",
        9 => "ninth",
        10 => "tenth",
        11 => "eleventh",
        12 => "twelfth",
        _ => return None,
    })
}

#[derive(Clone, Copy, Default, PartialEq)]
struct Relative {
    years: i64,
    months: i64,
    days: i64,
    hours: i64,
    minutes: i64,
    seconds: i64,
}

impl Relative {
    fn scaled(self, n: i64) -> Option<Self> {
        Some(Self {
            years: self.years.checked_mul(n)?,
            months: self.months.checked_mul(n)?,
            days: self.days.checked_mul(n)?,
            hours: self.hours.checked_mul(n)?,
            minutes: self.minutes.checked_mul(n)?,
            seconds: self.seconds.checked_mul(n)?,
        })
    }

    fn plus(self, other: Self) -> Option<Self> {
        Some(Self {
            years: self.years.checked_add(other.years)?,
            months: self.months.checked_add(other.months)?,
            days: self.days.checked_add(other.days)?,
            hours: self.hours.checked_add(other.hours)?,
            minutes: self.minutes.checked_add(other.minutes)?,
            seconds: self.seconds.checked_add(other.seconds)?,
        })
    }

    fn unit(word: &str) -> Option<Self> {
        let one = Self::default();
        Some(match word {
            "year" | "years" => Self { years: 1, ..one },
            "month" | "months" => Self { months: 1, ..one },
            "fortnight" | "fortnights" => Self { days: 14, ..one },
            "week" | "weeks" => Self { days: 7, ..one },
            "day" | "days" => Self { days: 1, ..one },
            "hour" | "hours" => Self { hours: 1, ..one },
            "minute" | "minutes" | "min" | "mins" => Self { minutes: 1, ..one },
            "second" | "seconds" | "sec" | "secs" => Self { seconds: 1, ..one },
            _ => return None,
        })
    }

    fn describe(&self) -> String {
        let parts: Vec<String> = [
            (self.years, " year(s)"),
            (self.months, " month(s)"),
            (self.days, " day(s)"),
            (self.hours, " hour(s)"),
            (self.minutes, " minutes"),
            (self.seconds, " seconds"),
        ]
        .iter()
        .filter(|(n, _)| *n != 0)
        .map(|(n, unit)| format!("{n:+}{unit}"))
        .collect();
        if parts.is_empty() {
            "today/this/now".to_string()
        } else {
            parts.join(" ")
        }
    }
}

struct DayItem {
    ordinal: i64,
    /// Whether the ordinal was written out ("next", "2", ...)
    explicit: bool,
    weekday: i8,
}

impl DayItem {
    fn label(&self) -> String {
        let name = WEEKDAY_ABBREVIATIONS[self.weekday as usize];
        match ordinal_label(self.ordinal).filter(|_| self.explicit) {
            Some(prefix) => format!("{prefix} {name}"),
            None => name.to_string(),
        }
    }
}

struct DateItem {
    /// Year value and the number of digits it was written with
    year: Option<(i64, usize)>,
    month: i64,
    day: i64,
}

/// Everything recognized in the input string.
#[derive(Default)]
struct Items {
    lines: Vec<String>,
    day: Option<DayItem>,
    date: Option<DateItem>,
    time: Option<Time>,
    zone: Option<i32>,
    local_isdst: Option<bool>,
    relative: Relative,
    relative_seen: bool,
    epoch: Option<Timestamp>,
}

struct Environment<'a> {
    now: &'a Zoned,
    utc: bool,
    tz_rule: Option<&'a str>,
    /// Zone of the `TZ="..."` rule if any, otherwise the zone of `now`
    zone: &'a TimeZone,
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    env: &'a Environment<'a>,
    items: Items,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token], env: &'a Environment<'a>) -> Self {
        Self {
            tokens,
            pos: 0,
            env,
            items: Items::default(),
        }
    }

    fn tok(&self, offset: usize) -> Option<&Tok> {
        self.tokens.get(self.pos + offset).map(|t| &t.tok)
    }

    /// The token at `offset`, if it is not separated from the previous one
    fn glued(&self, offset: usize) -> Option<&Tok> {
        self.tokens
            .get(self.pos + offset)
            .filter(|t| !t.spaced)
            .map(|t| &t.tok)
    }

    fn word(&self, offset: usize) -> Option<&str> {
        match self.tok(offset) {
            Some(Tok::Word(w)) => Some(w),
            _ => None,
        }
    }

    fn num(&self, offset: usize) -> Option<(i64, usize)> {
        match self.tok(offset) {
            Some(Tok::Num(n, len)) => Some((*n, *len)),
            _ => None,
        }
    }

    fn is_punct(&self, offset: usize, c: char) -> bool {
        self.tok(offset) == Some(&Tok::Punct(c))
    }

    fn glued_punct(&self, offset: usize, c: char) -> bool {
        self.glued(offset) == Some(&Tok::Punct(c))
    }

    fn sign(&self, offset: usize) -> Option<i64> {
        match self.tok(offset) {
            Some(Tok::Punct('+')) => Some(1),
            Some(Tok::Punct('-')) => Some(-1),
            _ => None,
        }
    }

    fn push(&mut self, line: String) {
        self.items.lines.push(line);
    }

    fn run(mut self) -> Option<Items> {
        if self.tokens.is_empty() {
            // An empty string stands for midnight
            self.items.time = Some(Time::midnight());
            self.push("parsed number part: 00:00:00".to_string());
            return Some(self.items);
        }
        if self.is_punct(0, '@') {
            self.epoch()?;
        }
        while self.pos < self.tokens.len() {
            match self.tok(0)? {
                Tok::Punct(',') => self.pos += 1,
                Tok::Punct('+' | '-') => self.signed_relative()?,
                Tok::Num(..) => self.number()?,
                Tok::Word(_) => self.word_item()?,
                Tok::Punct(_) => return None,
            }
        }
        Some(self.items)
    }

    fn epoch(&mut self) -> Option<()> {
        self.pos += 1;
        let sign = self.sign(0).inspect(|_| self.pos += 1).unwrap_or(1);
        let (secs, _) = self.num(0)?;
        self.pos += 1;
        let mut nanos = 0;
        if self.is_punct(0, '.') || self.is_punct(0, ',') {
            nanos = self.fraction(1)?;
            self.pos += 2;
        }
        if self.pos != self.tokens.len() {
            return None;
        }
        let ts = Timestamp::new(secs.checked_mul(sign)?, nanos * sign as i32).ok()?;
        self.push(format!(
            "parsed number of seconds part: number of seconds: {}",
            split_timestamp(ts).0
        ));
        self.items.epoch = Some(ts);
        Some(())
    }

    /// Nanoseconds from the digits of the number token at `offset`.
    fn fraction(&self, offset: usize) -> Option<i32> {
        let Some(Tok::Num(n, len)) = self.glued(offset) else {
            return None;
        };
        let digits = format!("{n:0len$}");
        let nanos = format!("{digits:0<9}");
        nanos[..9].parse().ok()
    }

    /// Record a relative item of `count` times `unit`, applying a following "ago".
    fn add_relative(&mut self, count: i64, unit: Relative) -> Option<()> {
        let mut delta = unit.scaled(count)?;
        if self.word(0) == Some("ago") {
            self.pos += 1;
            delta = delta.scaled(-1)?;
        }
        self.items.relative = self.items.relative.plus(delta)?;
        self.items.relative_seen = true;
        let line = format!("parsed relative part: {}", self.items.relative.describe());
        self.push(line);
        Some(())
    }

    /// `+N unit` or `-N unit`
    fn signed_relative(&mut self) -> Option<()> {
        let sign = self.sign(0)?;
        let (n, _) = self.num(1)?;
        let unit = Relative::unit(self.word(2)?)?;
        self.pos += 3;
        self.add_relative(n.checked_mul(sign)?, unit)
    }

    fn add_day(&mut self, ordinal: i64, explicit: bool, weekday: i8) -> Option<()> {
        if self.items.day.is_some() {
            return None;
        }
        let day = DayItem {
            ordinal,
            explicit,
            weekday,
        };
        if explicit {
            ordinal_label(ordinal)?;
        }
        self.push(format!(
            "parsed day part: {} (day ordinal={ordinal} number={weekday})",
            day.label()
        ));
        self.items.day = Some(day);
        Some(())
    }

    fn add_date(
        &mut self,
        kind: &str,
        year: Option<(i64, usize)>,
        month: i64,
        day: i64,
    ) -> Option<()> {
        if self.items.date.is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        let shown_year = year.map_or_else(|| self.env.now.year().into(), |(y, _)| y);
        self.push(format!(
            "parsed {kind} part: (Y-M-D) {}",
            format_ymd(shown_year, month, day)
        ));
        self.items.date = Some(DateItem { year, month, day });
        Some(())
    }

    fn set_time(&mut self, time: Time) -> Option<()> {
        if self.items.time.is_some() {
            return None;
        }
        self.items.time = Some(time);
        Some(())
    }

    fn set_zone(&mut self, offset: i32) -> Option<()> {
        if self.items.zone.is_some() || self.items.local_isdst.is_some() {
            return None;
        }
        self.items.zone = Some(offset);
        Some(())
    }

    fn word_item(&mut self) -> Option<()> {
        let word = self.word(0)?.to_string();
        if let Some(month) = month_number(&word) {
            return self.month_first_date(month.into());
        }
        if let Some(weekday) = weekday_number(&word) {
            self.pos += 1;
            return self.add_day(0, false, weekday);
        }
        if let Some(unit) = Relative::unit(&word) {
            self.pos += 1;
            return self.add_relative(1, unit);
        }
        if let Some(ordinal) = ordinal_number(&word) {
            let next = self.word(1)?.to_string();
            self.pos += 2;
            if let Some(weekday) = weekday_number(&next) {
                return self.add_day(ordinal, true, weekday);
            }
            return self.add_relative(ordinal, Relative::unit(&next)?);
        }
        let day = Relative {
            days: 1,
            ..Relative::default()
        };
        match word.as_str() {
            "tomorrow" => {
                self.pos += 1;
                self.add_relative(1, day)
            }
            "yesterday" => {
                self.pos += 1;
                self.add_relative(-1, day)
            }
            "today" | "now" => {
                self.pos += 1;
                self.add_relative(0, day)
            }
            _ => self.zone_word(&word),
        }
    }

    fn zone_word(&mut self, word: &str) -> Option<()> {
        self.pos += 1;
        let Some(offset) = self.zone_offset(word) else {
            let isdst = self.local_zone_dst(word)?;
            if self.items.zone.is_some() || self.items.local_isdst.is_some() {
                return None;
            }
            self.items.local_isdst = Some(isdst);
            self.push(format!("parsed local_zone part: isdst={}", u8::from(isdst)));
            return Some(());
        };
        self.set_zone(offset)?;
        // A signed relative item right after a zone name is reported first
        if self.sign(0).is_some()
            && self.num(1).is_some()
            && self.word(2).and_then(Relative::unit).is_some()
        {
            self.signed_relative()?;
        }
        self.push(format!("parsed zone part: UTC{}", format_offset(offset)));
        Some(())
    }

    fn zone_offset(&self, word: &str) -> Option<i32> {
        if matches!(word, "utc" | "gmt" | "ut" | "z") {
            return Some(0);
        }
        if self.local_zone_dst(word).is_some() {
            return None;
        }
        if let Some((_, offset)) = FIXED_OFFSET_ABBREVIATIONS
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(word))
        {
            return Some(*offset);
        }
        // Military zones
        let &[letter] = word.as_bytes() else {
            return None;
        };
        let hours = match letter {
            b'a'..=b'i' => i32::from(letter - b'a') + 1,
            b'k'..=b'm' => i32::from(letter - b'k') + 10,
            b'n'..=b'y' => -(i32::from(letter - b'n') + 1),
            _ => return None,
        };
        Some(hours * 3600)
    }

    /// Whether `word` is the standard or daylight abbreviation of the local zone.
    fn local_zone_dst(&self, word: &str) -> Option<bool> {
        let year = self.env.now.year();
        [1, 7].into_iter().find_map(|month| {
            let ts = Date::new(year, month, 1)
                .ok()?
                .to_zoned(TimeZone::UTC)
                .ok()?
                .timestamp();
            let info = self.env.zone.to_offset_info(ts);
            info.abbreviation()
                .eq_ignore_ascii_case(word)
                .then(|| info.dst().is_dst())
        })
    }

    /// `Mon D`, `Mon D, YYYY`
    fn month_first_date(&mut self, month: i64) -> Option<()> {
        let (day, _) = self.num(1)?;
        self.pos += 2;
        let mut year = None;
        if self.is_punct(0, ',')
            && let Some(y) = self.num(1)
        {
            year = Some(y);
            self.pos += 2;
        }
        self.add_date("date", year, month, day)
    }

    fn number(&mut self) -> Option<()> {
        let (n, len) = self.num(0)?;
        // ISO 8601 date, possibly followed by 'T' and a time
        if self.glued_punct(1, '-')
            && let Some(Tok::Num(month, _)) = self.glued(2).cloned()
            && self.glued_punct(3, '-')
            && let Some(Tok::Num(day, _)) = self.glued(4).cloned()
        {
            self.pos += 5;
            if self.glued(0) == Some(&Tok::Word("t".to_string()))
                && matches!(self.glued(1), Some(Tok::Num(..)))
            {
                self.pos += 1;
                return self.time(Some(DateItem {
                    year: Some((n, len)),
                    month,
                    day,
                }));
            }
            return self.add_date("date", Some((n, len)), month, day);
        }
        // Slash-separated dates
        if self.glued_punct(1, '/')
            && let Some(Tok::Num(second, _)) = self.glued(2).cloned()
        {
            self.pos += 3;
            let third = if self.glued_punct(0, '/')
                && let Some(Tok::Num(v, l)) = self.glued(1).cloned()
            {
                self.pos += 2;
                Some((v, l))
            } else {
                None
            };
            return match third {
                Some((day, _)) if len >= 4 => {
                    self.push(format!(
                        "warning: value {n} has {len} digits. Assuming YYYY/MM/DD"
                    ));
                    self.add_date("date", Some((n, len)), second, day)
                }
                Some(year) => {
                    self.push(format!(
                        "warning: value {n} has less than 4 digits. Assuming MM/DD/YY[YY]"
                    ));
                    self.add_date("date", Some(year), n, second)
                }
                None => self.add_date("date", None, n, second),
            };
        }
        if self.glued_punct(1, ':') {
            return self.time(None);
        }
        if let Some(word) = self.word(1).map(str::to_string) {
            if matches!(word.as_str(), "am" | "pm") {
                return self.time(None);
            }
            if let Some(month) = month_number(&word) {
                self.pos += 2;
                let year = self.num(0).inspect(|_| self.pos += 1);
                return self.add_date("date", year, month.into(), n);
            }
            if let Some(unit) = Relative::unit(&word) {
                self.pos += 2;
                return self.add_relative(n, unit);
            }
            if let Some(weekday) = weekday_number(&word) {
                self.pos += 2;
                return self.add_day(n, true, weekday);
            }
        }
        self.pos += 1;
        self.pure_number(n, len)
    }

    fn pure_number(&mut self, n: i64, len: usize) -> Option<()> {
        if let Some(date) = &mut self.items.date
            && date.year.is_none()
        {
            date.year = Some((n, len));
            self.push(format!("parsed number part: year: {n}"));
            return Some(());
        }
        if len > 4 && self.items.date.is_none() {
            let (year, month, day) = (n / 10000, n / 100 % 100, n % 100);
            self.add_date("number", Some((year, len - 4)), month, day)?;
            return Some(());
        }
        if self.items.time.is_none() {
            let (hour, minute) = if len <= 2 { (n, 0) } else { (n / 100, n % 100) };
            let time =
                Time::new(i8::try_from(hour).ok()?, i8::try_from(minute).ok()?, 0, 0).ok()?;
            self.set_time(time)?;
            self.push(format!("parsed number part: {}", format_hms(time)));
            return Some(());
        }
        None
    }

    /// `H:MM[:SS[.frac]] [am|pm] [+-HH[[:]MM]]`, or `H am|pm`
    fn time(&mut self, date: Option<DateItem>) -> Option<()> {
        let (hour, _) = self.num(0)?;
        self.pos += 1;
        let (mut minute, mut second, mut nanos) = (0, 0, 0);
        if self.glued_punct(0, ':') {
            let Some(Tok::Num(m, _)) = self.glued(1).cloned() else {
                return None;
            };
            minute = m;
            self.pos += 2;
            if self.glued_punct(0, ':')
                && let Some(Tok::Num(s, _)) = self.glued(1).cloned()
            {
                second = s;
                self.pos += 2;
                if (self.glued_punct(0, '.') || self.glued_punct(0, ','))
                    && let Some(frac) = self.fraction(1)
                {
                    nanos = frac;
                    self.pos += 2;
                }
            }
        }
        let mut shown = format!("{hour:02}:{minute:02}:{second:02}");
        if nanos != 0 {
            let _ = write!(shown, ".{nanos:09}");
        }
        let mut hour24 = hour;
        if let Some(meridian) = self.word(0).filter(|w| matches!(*w, "am" | "pm")) {
            if !(1..=12).contains(&hour) {
                return None;
            }
            hour24 = hour % 12 + if meridian == "pm" { 12 } else { 0 };
            shown.push_str(meridian);
            self.pos += 1;
        }
        if let Some(offset) = self.numeric_offset() {
            self.set_zone(offset)?;
            let _ = write!(shown, " UTC{}", format_offset(offset));
        }
        let time = Time::new(
            i8::try_from(hour24).ok()?,
            i8::try_from(minute).ok()?,
            i8::try_from(second).ok()?,
            nanos,
        )
        .ok()?;
        self.set_time(time)?;
        match date {
            Some(DateItem { year, month, day }) => {
                self.add_date("datetime", year, month, day)?;
                let line = self.items.lines.pop()?;
                self.push(format!("{line} {shown}"));
            }
            None => self.push(format!("parsed time part: {shown}")),
        }
        Some(())
    }

    /// A numeric UTC offset following a time, unless it starts a relative item.
    fn numeric_offset(&mut self) -> Option<i32> {
        let sign = self.sign(0)?;
        let Some(Tok::Num(n, len)) = self.glued(1).cloned() else {
            return None;
        };
        let mut used = 2;
        let (hours, minutes) = if self.glued_punct(2, ':')
            && let Some(Tok::Num(m, _)) = self.glued(3).cloned()
        {
            used = 4;
            (n, m)
        } else if len <= 2 {
            (n, 0)
        } else {
            (n / 100, n % 100)
        };
        if self.word(used).and_then(Relative::unit).is_some() || hours > 24 || minutes > 59 {
            return None;
        }
        self.pos += used;
        i32::try_from(sign * (hours * 3600 + minutes * 60)).ok()
    }
}

enum Outcome {
    Valid {
        lines: Vec<String>,
        result: Timestamp,
    },
    Invalid {
        lines: Vec<String>,
    },
}

impl Environment<'_> {
    fn env_tz_description(&self) -> String {
        match std::env::var("TZ") {
            Ok(tz) => format!("TZ=\"{tz}\" environment value"),
            Err(_) => "system default".to_string(),
        }
    }

    fn input_timezone(&self, items: &Items) -> String {
        if items.epoch.is_some() {
            return "'@timespec' - always UTC".to_string();
        }
        if let Some(offset) = items.zone {
            return format!("parsed date/time string ({})", format_offset(offset));
        }
        if let Some(rule) = self.tz_rule {
            return format!("TZ=\"{rule}\" in date string");
        }
        if self.utc {
            return "TZ=\"UTC0\" environment value or -u".to_string();
        }
        let mut text = self.env_tz_description();
        if items.local_isdst == Some(true) {
            text.push_str(", dst");
        }
        text
    }

    fn final_lines(&self, ts: Timestamp) -> Vec<String> {
        let zone = if let Some(rule) = self.tz_rule {
            format!("TZ=\"{rule}\" environment value")
        } else if self.utc {
            "Universal Time".to_string()
        } else {
            self.env_tz_description()
        };
        let (secs, nanos) = split_timestamp(ts);
        let local = ts.to_zoned(self.zone.clone());
        vec![
            format!("timezone: {zone}"),
            format!("final: {secs}.{nanos:09} (epoch-seconds)"),
            format!(
                "final: (Y-M-D) {} (UTC)",
                format_datetime(ts.to_zoned(TimeZone::UTC).datetime())
            ),
            format!(
                "final: (Y-M-D) {} (UTC{})",
                format_datetime(local.datetime()),
                format_offset(local.offset().seconds())
            ),
        ]
    }

    /// The instant of `dt` in the input zone, or `Err` with the instant it
    /// normalizes to when that wall-clock time does not exist.
    fn resolve(&self, items: &Items, dt: DateTime) -> Option<Result<Timestamp, Timestamp>> {
        if let Some(offset) = items.zone {
            return Some(Ok(Offset::from_seconds(offset)
                .ok()?
                .to_timestamp(dt)
                .ok()?));
        }
        let ambiguous = self.zone.to_ambiguous_timestamp(dt);
        let compatible = ambiguous.compatible().ok()?;
        let is_dst = |ts: Timestamp| self.zone.to_offset_info(ts).dst().is_dst();
        let Some(want_dst) = items.local_isdst else {
            return Some(match ambiguous.offset() {
                AmbiguousOffset::Gap { .. } => Err(compatible),
                _ => Ok(compatible),
            });
        };
        let candidates = match ambiguous.offset() {
            AmbiguousOffset::Unambiguous { offset } => vec![offset],
            AmbiguousOffset::Fold { before, after } => vec![before, after],
            AmbiguousOffset::Gap { .. } => vec![],
        };
        if let Some(ts) = candidates
            .into_iter()
            .filter_map(|offset| offset.to_timestamp(dt).ok())
            .find(|ts| is_dst(*ts) == want_dst)
        {
            return Some(Ok(ts));
        }
        // Read the wall clock as if it were in the requested DST state
        let shift = SignedDuration::from_hours(if want_dst { -1 } else { 1 });
        Some(Ok(compatible.checked_add(shift).ok()?))
    }

    fn wall_clock(&self, items: &Items, ts: Timestamp) -> Option<DateTime> {
        Some(match items.zone {
            Some(offset) => Offset::from_seconds(offset).ok()?.to_datetime(ts),
            None => ts.to_zoned(self.zone.clone()).datetime(),
        })
    }

    fn evaluate(&self, items: &Items) -> Option<Outcome> {
        let mut lines = items.lines.clone();
        lines.push(format!("input timezone: {}", self.input_timezone(items)));
        if let Some(ts) = items.epoch {
            return Some(Outcome::Valid { lines, result: ts });
        }

        let now = self.now.with_time_zone(self.zone.clone());
        let tz_suffix = items
            .zone
            .map(|offset| format!(" TZ={}", format_offset(offset)))
            .unwrap_or_default();

        let mut year = i64::from(now.year());
        if let Some((y, digits)) = items.date.as_ref().and_then(|d| d.year) {
            year = y;
            if digits <= 2 {
                year += if y < 69 { 2000 } else { 1900 };
                lines.push(format!("warning: adjusting year value {y} to {year}"));
            }
        }

        let time = if let Some(time) = items.time {
            lines.push(format!(
                "using specified time as starting value: '{}'",
                format_hms(time)
            ));
            time
        } else if items.date.is_some() || items.day.is_some() || !items.relative_seen {
            lines.push("warning: using midnight as starting time: 00:00:00".to_string());
            Time::midnight()
        } else {
            lines.push(format!(
                "using current time as starting value: '{}'",
                format_hms(now.time())
            ));
            now.time()
        };

        if items.date.is_none() && items.day.is_none() {
            lines.push(format!(
                "using current date as starting value: '(Y-M-D) {}'",
                format_ymd(now.year().into(), now.month().into(), now.day().into())
            ));
        }
        if let (Some(day), Some(_)) = (&items.day, &items.date) {
            lines.push(format!(
                "warning: day ({}) ignored when explicit dates are given",
                WEEKDAY_ABBREVIATIONS[day.weekday as usize]
            ));
        }

        let mut date = now.date();
        if let Some(d) = &items.date {
            let valid = i16::try_from(year)
                .ok()
                .and_then(|y| Date::new(y, d.month as i8, d.day as i8).ok());
            let Some(valid) = valid else {
                let first = Date::new(i16::try_from(year).ok()?, d.month as i8, 1).ok()?;
                let normalized = first.checked_add(days(d.day - 1)?).ok()?.to_datetime(time);
                let shown = format!("{} {}", format_ymd(year, d.month, d.day), format_hms(time));
                let ts = self.resolve(items, normalized)?.unwrap_or_else(|ts| ts);
                let normalized = self.wall_clock(items, ts)?;
                lines.extend(invalid_lines(&shown, &format_datetime(normalized)));
                return Some(Outcome::Invalid { lines });
            };
            date = valid;
        } else if let Some(day) = &items.day {
            let current = i64::from(date.weekday().to_sunday_zero_offset());
            let ahead = (i64::from(day.weekday) - current).rem_euclid(7);
            let shift = if day.ordinal > 0 {
                if ahead == 0 {
                    7 * day.ordinal
                } else {
                    ahead + 7 * (day.ordinal - 1)
                }
            } else {
                ahead + 7 * day.ordinal
            };
            date = date.checked_add(days(shift)?).ok()?;
            lines.push(format!(
                "new start date: '{}' is '(Y-M-D) {}'",
                day.label(),
                format_datetime(date.to_datetime(time))
            ));
        }

        let start = date.to_datetime(time);
        let mut ts = match self.resolve(items, start)? {
            Ok(ts) => ts,
            Err(normalized) => {
                let normalized = self.wall_clock(items, normalized)?;
                lines.extend(invalid_lines(
                    &format_datetime(start),
                    &format_datetime(normalized),
                ));
                return Some(Outcome::Invalid { lines });
            }
        };
        lines.push(format!(
            "starting date/time: '(Y-M-D) {}{tz_suffix}'",
            format_datetime(start)
        ));
        let mut wall = start;

        let rel = items.relative;
        if rel.years != 0 || rel.months != 0 || rel.days != 0 {
            if (rel.years != 0 || rel.months != 0) && start.day() != 15 {
                lines.push(
                    "warning: when adding relative months/years, it is recommended to specify the 15th of the months"
                        .to_string(),
                );
            }
            if rel.days != 0 && start.hour() != 12 {
                lines.push(
                    "warning: when adding relative days, it is recommended to specify noon"
                        .to_string(),
                );
            }
            let raw_year = i64::from(start.year()).checked_add(rel.years)?;
            let raw_month = i64::from(start.month()).checked_add(rel.months)?;
            let raw_day = i64::from(start.day()).checked_add(rel.days)?;
            let months = raw_year.checked_mul(12)?.checked_add(raw_month - 1)?;
            let first = Date::new(
                i16::try_from(months.div_euclid(12)).ok()?,
                (months.rem_euclid(12) + 1) as i8,
                1,
            )
            .ok()?;
            let adjusted = first
                .checked_add(days(raw_day - 1)?)
                .ok()?
                .to_datetime(start.time());
            ts = self.resolve(items, adjusted)?.unwrap_or_else(|ts| ts);
            wall = self.wall_clock(items, ts)?;
            lines.push(format!(
                "after date adjustment ({:+} years, {:+} months, {:+} days),",
                rel.years, rel.months, rel.days
            ));
            lines.push(format!(
                "    new date/time = '(Y-M-D) {}{tz_suffix}'",
                format_datetime(wall)
            ));
            if let Some(want_dst) = items.local_isdst
                && self.zone.to_offset_info(ts).dst().is_dst() != want_dst
            {
                lines.push(
                    "warning: daylight saving time changed after date adjustment".to_string(),
                );
            }
            let normalized = (
                i64::from(wall.year()),
                i64::from(wall.month()),
                i64::from(wall.day()),
            );
            if rel.days == 0 && normalized != (raw_year, raw_month, raw_day) {
                lines.push("warning: month/year adjustment resulted in shifted dates:".to_string());
                lines.push(format!(
                    "     adjusted Y M D: {raw_year:04} {raw_month:02} {raw_day:02}"
                ));
                lines.push(format!(
                    "   normalized Y M D: {:04} {:02} {:02}",
                    normalized.0, normalized.1, normalized.2
                ));
            }
        }

        lines.push(format!(
            "'(Y-M-D) {}{tz_suffix}' = {} epoch-seconds",
            format_datetime(wall),
            split_timestamp(ts).0
        ));

        if rel.hours != 0 || rel.minutes != 0 || rel.seconds != 0 {
            let seconds = rel
                .hours
                .checked_mul(3600)?
                .checked_add(rel.minutes.checked_mul(60)?)?
                .checked_add(rel.seconds)?;
            ts = ts.checked_add(SignedDuration::from_secs(seconds)).ok()?;
            lines.push(format!(
                "after time adjustment ({:+} hours, {:+} minutes, {:+} seconds, +0 ns),",
                rel.hours, rel.minutes, rel.seconds
            ));
            lines.push(format!(
                "    new time = {} epoch-seconds",
                split_timestamp(ts).0
            ));
        }

        Some(Outcome::Valid { lines, result: ts })
    }
}

/// Lines describing a date/time that does not exist; both arguments are
/// `YYYY-MM-DD HH:MM:SS` strings.
fn invalid_lines(provided: &str, normalized: &str) -> Vec<String> {
    const PREFIX: &str = "    user provided time: '(Y-M-D) ";
    // Underline each field that changed during normalization
    let mut marker = " ".repeat(PREFIX.len());
    for (start, len) in [(0, 4), (5, 2), (8, 2), (11, 2), (14, 2), (17, 2)] {
        let changed = provided.get(start..start + len) != normalized.get(start..start + len);
        let fill = if changed { "-" } else { " " };
        marker.push_str(&fill.repeat(len));
        marker.push(' ');
    }
    vec![
        "error: invalid date/time value:".to_string(),
        format!("{PREFIX}{provided}'"),
        format!("       normalized time: '(Y-M-D) {normalized}'"),
        marker.trim_end().to_string(),
        "     possible reasons:".to_string(),
        "       nonexistent due to daylight-saving time;".to_string(),
        "       invalid day/month combination;".to_string(),
        "       missing timezone".to_string(),
    ]
}
