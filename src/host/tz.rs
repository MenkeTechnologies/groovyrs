//! Region time zones (`America/New_York`) from the system tz database: the
//! TZif files under `$TZDIR` or `/usr/share/zoneinfo`, read the way `zic`
//! writes them — the transition table, then the POSIX `TZ` rule in the footer
//! for instants past its last transition.

use std::collections::HashMap;
use std::rc::Rc;

/// One local-time type: its UTC offset in seconds, whether it is daylight
/// time, and its abbreviation.
#[derive(Clone, Debug)]
pub(super) struct LocalType {
    pub(super) offset: i64,
    pub(super) dst: bool,
    pub(super) abbr: String,
}

/// A parsed zone.
#[derive(Debug)]
pub(super) struct TzData {
    transitions: Vec<i64>,
    type_of: Vec<usize>,
    types: Vec<LocalType>,
    rule: Option<Rule>,
}

/// The POSIX `TZ` footer: standard time, and daylight time with the rules
/// that start and end it.
#[derive(Debug)]
struct Rule {
    std: LocalType,
    dst: Option<(LocalType, When, When)>,
}

/// When a daylight-time change happens: a day of the year and the local
/// time of day (seconds) it happens at.
#[derive(Debug, Clone, Copy)]
struct When {
    day: Day,
    secs: i64,
}

#[derive(Debug, Clone, Copy)]
enum Day {
    /// `Mm.w.d`: day `d` (0 = Sunday) of week `w` (5 = last) of month `m`.
    Month(i64, i64, i64),
    /// `Jn`: day `n` (1-365), February 29 never counted.
    Julian1(i64),
    /// `n`: zero-based day of the year, leap days counted.
    Julian0(i64),
}

thread_local! {
    static CACHE: std::cell::RefCell<HashMap<String, Option<Rc<TzData>>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// The zone named `id`, if the tz database has it.
pub(super) fn load(id: &str) -> Option<Rc<TzData>> {
    if id.is_empty() || id.contains("..") || id.starts_with('/') {
        return None;
    }
    CACHE.with(|c| {
        c.borrow_mut()
            .entry(id.to_string())
            .or_insert_with(|| {
                let dir = std::env::var("TZDIR").unwrap_or_else(|_| "/usr/share/zoneinfo".into());
                let bytes = std::fs::read(std::path::Path::new(&dir).join(id)).ok()?;
                parse(&bytes).map(Rc::new)
            })
            .clone()
    })
}

fn be32(b: &[u8], at: usize) -> Option<i64> {
    Some(i64::from(i32::from_be_bytes(
        b.get(at..at + 4)?.try_into().ok()?,
    )))
}

fn be64(b: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Parse a TZif file, preferring its 64-bit (version 2+) data block.
fn parse(b: &[u8]) -> Option<TzData> {
    if b.get(0..4)? != b"TZif" {
        return None;
    }
    let version = *b.get(4)?;
    let counts = |at: usize| -> Option<[usize; 6]> {
        let mut c = [0usize; 6];
        for (i, slot) in c.iter_mut().enumerate() {
            *slot = be32(b, at + 20 + 4 * i)? as usize;
        }
        Some(c)
    };
    // [isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt]
    let v1 = counts(0)?;
    let v1_len = v1[3] * 5 + v1[4] * 6 + v1[5] + v1[2] * 8 + v1[1] + v1[0];
    let (start, c, wide) = if version >= b'2' {
        let at = 44 + v1_len;
        (at, counts(at)?, true)
    } else {
        (0, v1, false)
    };
    let tsize = if wide { 8 } else { 4 };
    let mut at = start + 44;
    let mut transitions = Vec::with_capacity(c[3]);
    for i in 0..c[3] {
        transitions.push(if wide {
            be64(b, at + i * 8)?
        } else {
            be32(b, at + i * 4)?
        });
    }
    at += c[3] * tsize;
    let type_of: Vec<usize> = b.get(at..at + c[3])?.iter().map(|&t| t as usize).collect();
    at += c[3];
    let raw_types: Vec<(i64, bool, usize)> = (0..c[4])
        .map(|i| {
            let p = at + i * 6;
            Some((be32(b, p)?, *b.get(p + 4)? != 0, *b.get(p + 5)? as usize))
        })
        .collect::<Option<_>>()?;
    at += c[4] * 6;
    let chars = b.get(at..at + c[5])?;
    let abbr = |i: usize| {
        let s = chars.get(i..).unwrap_or(&[]);
        let end = s.iter().position(|&x| x == 0).unwrap_or(s.len());
        String::from_utf8_lossy(&s[..end]).into_owned()
    };
    let types = raw_types
        .iter()
        .map(|&(offset, dst, ai)| LocalType {
            offset,
            dst,
            abbr: abbr(ai),
        })
        .collect();
    at += c[5] + c[2] * (tsize + 4) + c[1] + c[0];
    let rule = if wide {
        b.get(at..)
            .and_then(|f| std::str::from_utf8(f).ok())
            .and_then(|f| f.trim_matches('\n').lines().next().map(str::to_string))
            .and_then(|s| parse_rule(&s))
    } else {
        None
    };
    Some(TzData {
        transitions,
        type_of,
        types,
        rule,
    })
}

/// Parse a POSIX `TZ` string such as `EST5EDT,M3.2.0,M11.1.0`.
fn parse_rule(s: &str) -> Option<Rule> {
    let mut p = s;
    let name = |p: &mut &str| -> Option<String> {
        if let Some(rest) = p.strip_prefix('<') {
            let end = rest.find('>')?;
            let n = rest[..end].to_string();
            *p = &rest[end + 1..];
            Some(n)
        } else {
            let end = p
                .find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(p.len());
            if end < 3 {
                return None;
            }
            let n = p[..end].to_string();
            *p = &p[end..];
            Some(n)
        }
    };
    // `[+-]hh[:mm[:ss]]`, in seconds.
    let time = |p: &mut &str| -> Option<i64> {
        let neg = p.starts_with('-');
        let body = p.trim_start_matches(['+', '-']);
        let end = body
            .find(|c: char| !(c.is_ascii_digit() || c == ':'))
            .unwrap_or(body.len());
        if end == 0 {
            return None;
        }
        let mut secs = 0;
        for (i, part) in body[..end].split(':').enumerate() {
            let v: i64 = part.parse().ok()?;
            secs += v * [3600, 60, 1].get(i)?;
        }
        *p = &body[end..];
        Some(if neg { -secs } else { secs })
    };
    let std_name = name(&mut p)?;
    // POSIX offsets are west of UTC.
    let std_off = -time(&mut p)?;
    let std = LocalType {
        offset: std_off,
        dst: false,
        abbr: std_name,
    };
    if p.is_empty() {
        return Some(Rule { std, dst: None });
    }
    let dst_name = name(&mut p)?;
    let dst_off = if p.starts_with(',') {
        std_off + 3600
    } else {
        -time(&mut p)?
    };
    let when = |p: &mut &str| -> Option<When> {
        *p = p.strip_prefix(',')?;
        let day = if let Some(rest) = p.strip_prefix('M') {
            let end = rest.find(['/', ',']).unwrap_or(rest.len());
            let parts: Vec<i64> = rest[..end]
                .split('.')
                .map(|x| x.parse().ok())
                .collect::<Option<_>>()?;
            *p = &rest[end..];
            Day::Month(*parts.first()?, *parts.get(1)?, *parts.get(2)?)
        } else if let Some(rest) = p.strip_prefix('J') {
            let end = rest.find(['/', ',']).unwrap_or(rest.len());
            let n = rest[..end].parse().ok()?;
            *p = &rest[end..];
            Day::Julian1(n)
        } else {
            let end = p.find(['/', ',']).unwrap_or(p.len());
            let n = p[..end].parse().ok()?;
            *p = &p[end..];
            Day::Julian0(n)
        };
        let secs = match p.strip_prefix('/') {
            Some(rest) => {
                *p = rest;
                time(p)?
            }
            None => 7200,
        };
        Some(When { day, secs })
    };
    let start = when(&mut p)?;
    let end = when(&mut p)?;
    Some(Rule {
        std,
        dst: Some((
            LocalType {
                offset: dst_off,
                dst: true,
                abbr: dst_name,
            },
            start,
            end,
        )),
    })
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Days since 1970-01-01 of a civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

const MONTH_DAYS: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// The day (days since the epoch) a rule names in `year`.
fn rule_day(day: Day, year: i64) -> i64 {
    match day {
        Day::Month(m, w, d) => {
            let first = days_from_civil(year, m, 1);
            let first_dow = (first + 4).rem_euclid(7);
            let mut day = first + (d - first_dow).rem_euclid(7) + (w - 1) * 7;
            let len = MONTH_DAYS[(m - 1) as usize] + i64::from(m == 2 && is_leap(year));
            while day >= first + len {
                day -= 7;
            }
            day
        }
        Day::Julian1(n) => {
            let base = days_from_civil(year, 1, 1) + n - 1;
            base + i64::from(is_leap(year) && n >= 60)
        }
        Day::Julian0(n) => days_from_civil(year, 1, 1) + n,
    }
}

impl Rule {
    fn at(&self, t: i64) -> &LocalType {
        let Some((dst, start, end)) = &self.dst else {
            return &self.std;
        };
        let year = year_of_local(t + self.std.offset);
        // The changes happen at local wall time: daylight time starts at
        // standard time and ends at daylight time.
        let begin = rule_day(start.day, year) * 86_400 + start.secs - self.std.offset;
        let finish = rule_day(end.day, year) * 86_400 + end.secs - dst.offset;
        let in_dst = if begin < finish {
            t >= begin && t < finish
        } else {
            t >= begin || t < finish
        };
        if in_dst {
            dst
        } else {
            &self.std
        }
    }
}

/// The calendar year of a local-time second count.
fn year_of_local(local: i64) -> i64 {
    let days = local.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let y = yoe + era * 400;
    if mp >= 10 {
        y + 1
    } else {
        y
    }
}

impl TzData {
    /// The local-time type in effect at UTC second `t`.
    pub(super) fn at(&self, t: i64) -> LocalType {
        let i = self.transitions.partition_point(|&x| x <= t);
        if i == self.transitions.len() {
            if let Some(rule) = &self.rule {
                return rule.at(t).clone();
            }
        }
        if i == 0 {
            // Before the first transition: the first standard-time type.
            return self
                .types
                .iter()
                .find(|lt| !lt.dst)
                .or(self.types.first())
                .cloned()
                .unwrap_or(LocalType {
                    offset: 0,
                    dst: false,
                    abbr: "GMT".into(),
                });
        }
        self.types[self.type_of[i - 1]].clone()
    }

    /// The zone's raw (standard) offset now, in seconds: the footer rule's,
    /// or the last standard type's.
    pub(super) fn raw_offset(&self) -> i64 {
        if let Some(rule) = &self.rule {
            return rule.std.offset;
        }
        self.types
            .iter()
            .rev()
            .find(|lt| !lt.dst)
            .map_or(0, |lt| lt.offset)
    }

    /// Whether the zone still observes daylight time.
    pub(super) fn uses_dst(&self) -> bool {
        self.rule.as_ref().is_some_and(|r| r.dst.is_some())
    }
}
