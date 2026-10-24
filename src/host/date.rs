//! `java.util.Date`, `java.util.TimeZone` and `groovy.time.TimeCategory`.
//!
//! A `Date` is its millisecond instant. The local zone is the C library's
//! (`localtime_r` / `mktime`), which is the zone the JVM defaults to as well; an
//! explicit `TimeZone` is `UTC`/`GMT` or a fixed `GMT±hh:mm` offset (a named
//! region falls back to `GMT`, as the JDK does for an id it does not know —
//! the region database is not modeled).
//!
//! Formatting follows `SimpleDateFormat` for the letters scripts use (`y M d
//! H h m s S a E z Z` and quoted text); `TimeCategory` durations render as
//! `BaseDuration.toString` does (`2 days, 3 hours, 5.007 seconds`).

use super::*;

const DAY: i64 = 86_400_000;

/// A `TimeZone`: `None` for the JVM default (the local zone), else a fixed
/// offset in seconds and its id.
#[derive(Clone)]
pub(super) struct Zone {
    pub(super) fixed: Option<(i32, String)>,
}

/// A `TimeCategory` duration's fields, and which class it is.
#[derive(Clone, Default)]
pub(super) struct Dur {
    years: i64,
    months: i64,
    days: i64,
    hours: i64,
    minutes: i64,
    seconds: i64,
    millis: i64,
    class: &'static str,
}

/// Broken-down local or fixed-zone time of an instant.
struct Fields {
    year: i64,
    month: i64, // 1-12
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    millis: i64,
    weekday: i64, // 0 = Sunday
    offset_secs: i64,
    zone: String,
}

pub(super) fn date_millis(v: &Value) -> Option<i64> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::Date(ms)) => Some(*ms),
            _ => None,
        }),
        _ => None,
    }
}

pub(super) fn new_date(ms: i64) -> Value {
    heap_push(HeapObj::Date(ms))
}

fn zone_of(v: &Value) -> Option<Zone> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::TimeZone(z)) => Some(z.clone()),
            _ => None,
        }),
        _ => None,
    }
}

pub(super) fn dur_of(v: &Value) -> Option<Dur> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::Duration(d)) => Some((**d).clone()),
            _ => None,
        }),
        _ => None,
    }
}

fn new_dur(d: Dur) -> Value {
    heap_push(HeapObj::Duration(Box::new(d)))
}

/// The class a date-family value reports.
pub(super) fn class_name(v: &Value) -> Option<String> {
    if date_millis(v).is_some() {
        return Some("java.util.Date".to_string());
    }
    if zone_of(v).is_some() {
        return Some("sun.util.calendar.ZoneInfo".to_string());
    }
    dur_of(v).map(|d| format!("groovy.time.{}", d.class))
}

/// Civil date of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
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

/// The local zone's offset (seconds east of UTC) and abbreviation at `ms`.
fn local_offset(ms: i64) -> (i64, String) {
    let t = ms.div_euclid(1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if !ok {
        return (0, "UTC".to_string());
    }
    let zone = if tm.tm_zone.is_null() {
        "UTC".to_string()
    } else {
        unsafe { std::ffi::CStr::from_ptr(tm.tm_zone) }
            .to_string_lossy()
            .into_owned()
    };
    (tm.tm_gmtoff as i64, zone)
}

fn fields(ms: i64, zone: &Zone) -> Fields {
    let (offset, name) = match &zone.fixed {
        Some((off, id)) => (i64::from(*off), id.clone()),
        None => local_offset(ms),
    };
    let local = ms + offset * 1000;
    let days = local.div_euclid(DAY);
    let rem = local.rem_euclid(DAY);
    let (year, month, day) = civil(days);
    Fields {
        year,
        month,
        day,
        hour: rem / 3_600_000,
        minute: rem / 60_000 % 60,
        second: rem / 1000 % 60,
        millis: rem % 1000,
        weekday: (days + 4).rem_euclid(7),
        offset_secs: offset,
        zone: name,
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// `SimpleDateFormat(pattern).format(date)` in `zone`.
fn format(ms: i64, pattern: &str, zone: &Zone) -> String {
    let f = fields(ms, zone);
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            if chars.get(i + 1) == Some(&'\'') {
                out.push('\'');
                i += 2;
                continue;
            }
            i += 1;
            while i < chars.len() && chars[i] != '\'' {
                out.push(chars[i]);
                i += 1;
            }
            i += 1;
            continue;
        }
        if !c.is_ascii_alphabetic() {
            out.push(c);
            i += 1;
            continue;
        }
        let mut n = 1;
        while chars.get(i + n) == Some(&c) {
            n += 1;
        }
        i += n;
        let num = |v: i64| format!("{v:0n$}");
        match c {
            'y' => out.push_str(&if n == 2 {
                format!("{:02}", f.year.rem_euclid(100))
            } else {
                num(f.year)
            }),
            'M' => out.push_str(&match n {
                1 | 2 => num(f.month),
                3 => MONTHS[(f.month - 1) as usize][..3].to_string(),
                _ => MONTHS[(f.month - 1) as usize].to_string(),
            }),
            'd' => out.push_str(&num(f.day)),
            'H' => out.push_str(&num(f.hour)),
            'h' => out.push_str(&num(if f.hour % 12 == 0 { 12 } else { f.hour % 12 })),
            'm' => out.push_str(&num(f.minute)),
            's' => out.push_str(&num(f.second)),
            'S' => out.push_str(&num(f.millis)),
            'a' => out.push_str(if f.hour < 12 { "AM" } else { "PM" }),
            'E' => out.push_str(if n >= 4 {
                DAYS[f.weekday as usize]
            } else {
                &DAYS[f.weekday as usize][..3]
            }),
            'z' => out.push_str(&f.zone),
            'Z' => {
                let o = f.offset_secs / 60;
                let sign = if o < 0 { '-' } else { '+' };
                out.push_str(&format!("{sign}{:02}{:02}", o.abs() / 60, o.abs() % 60));
            }
            other => out.push_str(&other.to_string().repeat(n)),
        }
    }
    out
}

/// `Date.toString()`: `EEE MMM dd HH:mm:ss zzz yyyy` in the local zone.
pub(super) fn to_string(ms: i64) -> String {
    format(ms, "EEE MMM dd HH:mm:ss zzz yyyy", &Zone { fixed: None })
}

/// `Date.parse(pattern, text)` for the numeric fields, in the local zone.
fn parse(pattern: &str, text: &str) -> Option<i64> {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut i, mut j) = (0, 0);
    let (mut y, mut mo, mut d, mut h, mut mi, mut s, mut ms) = (1970, 1, 1, 0, 0, 0, 0);
    let mut pm = false;
    while i < p.len() {
        let c = p[i];
        if !c.is_ascii_alphabetic() {
            if t.get(j) != Some(&c) {
                return None;
            }
            i += 1;
            j += 1;
            continue;
        }
        let mut n = 1;
        while p.get(i + n) == Some(&c) {
            n += 1;
        }
        i += n;
        if c == 'a' {
            let word: String = t.get(j..j + 2)?.iter().collect();
            pm = word.eq_ignore_ascii_case("PM");
            j += 2;
            continue;
        }
        let start = j;
        while j < t.len() && t[j].is_ascii_digit() {
            j += 1;
        }
        let v: i64 = t[start..j].iter().collect::<String>().parse().ok()?;
        match c {
            'y' => y = if n == 2 { 2000 + v } else { v },
            'M' => mo = v,
            'd' => d = v,
            'H' | 'h' => h = v,
            'm' => mi = v,
            's' => s = v,
            'S' => ms = v,
            _ => {}
        }
    }
    if pm && h < 12 {
        h += 12;
    }
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = (y - 1900) as i32;
    tm.tm_mon = (mo - 1) as i32;
    tm.tm_mday = d as i32;
    tm.tm_hour = h as i32;
    tm.tm_min = mi as i32;
    tm.tm_sec = s as i32;
    tm.tm_isdst = -1;
    let secs = unsafe { libc::mktime(&mut tm) };
    Some(secs as i64 * 1000 + ms)
}

/// `TimeZone.getTimeZone(id)`.
pub(super) fn time_zone(id: &str) -> Value {
    let fixed = match id {
        "UTC" | "GMT" | "Z" => (0, id.to_string()),
        other => match other.strip_prefix("GMT").filter(|r| r.len() > 1) {
            Some(rest) => {
                let sign = if rest.starts_with('-') { -1 } else { 1 };
                let body = rest.trim_start_matches(['+', '-']);
                let (hh, mm) = body.split_once(':').unwrap_or((body, "0"));
                let secs = sign
                    * (hh.parse::<i32>().unwrap_or(0) * 3600 + mm.parse::<i32>().unwrap_or(0) * 60);
                (secs, other.to_string())
            }
            None => (0, "GMT".to_string()),
        },
    };
    heap_push(HeapObj::TimeZone(Zone { fixed: Some(fixed) }))
}

thread_local! {
    /// How many `use(TimeCategory)` blocks are running.
    static TIME_CATEGORY: Cell<u32> = const { Cell::new(0) };
}

/// Enter / leave a `use(groovy.time.TimeCategory)` block.
pub(super) fn time_category(on: bool) {
    TIME_CATEGORY.with(|c| {
        c.set(if on {
            c.get() + 1
        } else {
            c.get().saturating_sub(1)
        })
    });
}

fn in_time_category() -> bool {
    TIME_CATEGORY.with(|c| c.get() > 0)
}

/// `n.days`, `n.hours`, … inside `TimeCategory`.
pub(super) fn number_property(v: &Value, name: &str) -> Option<Value> {
    if !in_time_category() {
        return None;
    }
    let Value::Int(n) = v else { return None };
    let n = *n;
    let mut d = Dur {
        class: "TimeDuration",
        ..Dur::default()
    };
    match name {
        "day" | "days" => {
            d.days = n;
            d.class = "Duration";
        }
        "week" | "weeks" => {
            d.days = n * 7;
            d.class = "Duration";
        }
        "hour" | "hours" => d.hours = n,
        "minute" | "minutes" => d.minutes = n,
        "second" | "seconds" => d.seconds = n,
        "millisecond" | "milliseconds" => d.millis = n,
        "month" | "months" => {
            d.months = n;
            d.class = "DatumDependentDuration";
        }
        "year" | "years" => {
            d.years = n;
            d.class = "DatumDependentDuration";
        }
        _ => return None,
    }
    Some(new_dur(d))
}

fn dur_millis(d: &Dur) -> i64 {
    ((((d.years * 365 + d.months * 30 + d.days) * 24 + d.hours) * 60 + d.minutes) * 60 + d.seconds)
        * 1000
        + d.millis
}

/// `BaseDuration.toString`.
fn dur_string(d: &Dur) -> String {
    let mut parts = Vec::new();
    for (n, unit) in [
        (d.years, "years"),
        (d.months, "months"),
        (d.days, "days"),
        (d.hours, "hours"),
        (d.minutes, "minutes"),
    ] {
        if n != 0 {
            parts.push(format!("{n} {unit}"));
        }
    }
    if d.seconds != 0 || d.millis != 0 {
        let norm_millis = d.millis % 1000;
        let norm_seconds = d.seconds + (d.millis - norm_millis) / 1000;
        let head = if norm_seconds == 0 {
            (if norm_millis < 0 { "-0" } else { "0" }).to_string()
        } else {
            norm_seconds.to_string()
        };
        parts.push(format!("{head}.{:03} seconds", norm_millis.abs()));
    }
    if parts.is_empty() {
        "0".to_string()
    } else {
        parts.join(", ")
    }
}

/// `date + months/years` on the calendar, the rest as elapsed time.
fn add_dur(ms: i64, d: &Dur, sign: i64) -> i64 {
    let mut ms = ms;
    if d.months != 0 || d.years != 0 {
        let zone = Zone { fixed: None };
        let f = fields(ms, &zone);
        let total = f.year * 12 + (f.month - 1) + sign * (d.years * 12 + d.months);
        let (y, m) = (total.div_euclid(12), total.rem_euclid(12) + 1);
        let day = days_from_civil(y, m, f.day);
        let local = day * DAY + ((f.hour * 60 + f.minute) * 60 + f.second) * 1000 + f.millis;
        ms = local - f.offset_secs * 1000;
    }
    let rest = Dur {
        years: 0,
        months: 0,
        ..d.clone()
    };
    ms + sign * dur_millis(&rest)
}

/// The elapsed time between two instants as a `TimeDuration`.
fn diff(a: i64, b: i64) -> Dur {
    let delta = a - b;
    Dur {
        days: delta / DAY,
        hours: delta % DAY / 3_600_000,
        minutes: delta % 3_600_000 / 60_000,
        seconds: delta % 60_000 / 1000,
        millis: delta % 1000,
        class: "TimeDuration",
        ..Dur::default()
    }
}

fn dur_combine(a: &Dur, b: &Dur, sign: i64) -> Dur {
    let class = if a.class == b.class {
        a.class
    } else {
        "TimeDuration"
    };
    Dur {
        years: a.years + sign * b.years,
        months: a.months + sign * b.months,
        days: a.days + sign * b.days,
        hours: a.hours + sign * b.hours,
        minutes: a.minutes + sign * b.minutes,
        seconds: a.seconds + sign * b.seconds,
        millis: a.millis + sign * b.millis,
        class,
    }
}

/// The calendar day number of an instant in the local zone.
fn local_day(ms: i64) -> i64 {
    let (off, _) = local_offset(ms);
    (ms + off * 1000).div_euclid(DAY)
}

/// `a + b` / `a - b` with a `Date` or a duration on the left.
pub(super) fn arithmetic(a: &Value, b: &Value, add: bool) -> Option<Value> {
    let sign = if add { 1 } else { -1 };
    if let Some(ms) = date_millis(a) {
        if let Value::Int(n) = b {
            return Some(new_date(ms + sign * n * DAY));
        }
        if let Some(d) = dur_of(b) {
            return Some(new_date(add_dur(ms, &d, sign)));
        }
        if let (false, Some(other)) = (add, date_millis(b)) {
            return Some(if in_time_category() {
                new_dur(diff(ms, other))
            } else {
                Value::int(local_day(ms) - local_day(other))
            });
        }
        return None;
    }
    let da = dur_of(a)?;
    if let Some(db) = dur_of(b) {
        return Some(new_dur(dur_combine(&da, &db, sign)));
    }
    if let (true, Some(ms)) = (add, date_millis(b)) {
        return Some(new_date(add_dur(ms, &da, 1)));
    }
    None
}

/// Order two dates or two durations.
pub(super) fn compare(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    if let (Some(x), Some(y)) = (date_millis(a), date_millis(b)) {
        return Some(x.cmp(&y));
    }
    if let (Some(x), Some(y)) = (dur_of(a), dur_of(b)) {
        return Some(dur_millis(&x).cmp(&dur_millis(&y)));
    }
    None
}

/// Text of a date-family value.
pub(super) fn text(v: &Value) -> Option<String> {
    if let Some(ms) = date_millis(v) {
        return Some(to_string(ms));
    }
    if let Some(d) = dur_of(v) {
        return Some(dur_string(&d));
    }
    zone_of(v).map(|z| match z.fixed {
        Some((_, id)) => id,
        None => "local".to_string(),
    })
}

/// The methods of `Date`, `TimeZone` and the durations.
pub(super) fn dispatch(recv: &Value, method: &str, args: &[Value]) -> Option<Value> {
    if let Some(ms) = date_millis(recv) {
        return Some(match (method, args) {
            ("getTime", []) => Value::int(ms),
            ("format", [p]) => Value::str(format(ms, &groovy_str(p), &Zone { fixed: None })),
            ("format", [p, z]) => {
                let zone = zone_of(z).unwrap_or(Zone { fixed: None });
                Value::str(format(ms, &groovy_str(p), &zone))
            }
            ("plus", [b]) => arithmetic(recv, b, true)?,
            ("minus", [b]) => arithmetic(recv, b, false)?,
            ("next", []) => new_date(ms + DAY),
            ("previous", []) => new_date(ms - DAY),
            ("before", [b]) => Value::bool(ms < date_millis(b)?),
            ("after", [b]) => Value::bool(ms > date_millis(b)?),
            ("compareTo", [b]) => Value::int(ms.cmp(&date_millis(b)?) as i64),
            ("equals", [b]) => Value::bool(date_millis(b) == Some(ms)),
            ("hashCode", []) => Value::int(i64::from((ms ^ (ms >> 32)) as i32)),
            ("toString", []) => Value::str(to_string(ms)),
            ("clone", []) => new_date(ms),
            _ => return None,
        });
    }
    if let Some(z) = zone_of(recv) {
        return Some(match (method, args) {
            ("getID", []) => Value::str(z.fixed.map(|(_, id)| id).unwrap_or_default()),
            ("getRawOffset", []) => {
                Value::int(i64::from(z.fixed.map(|(o, _)| o).unwrap_or(0)) * 1000)
            }
            _ => return None,
        });
    }
    let d = dur_of(recv)?;
    Some(match (method, args) {
        ("toMilliseconds", []) => Value::int(dur_millis(&d)),
        ("getYears", []) => Value::int(d.years),
        ("getMonths", []) => Value::int(d.months),
        ("getDays", []) => Value::int(d.days),
        ("getHours", []) => Value::int(d.hours),
        ("getMinutes", []) => Value::int(d.minutes),
        ("getSeconds", []) => Value::int(d.seconds),
        ("getMillis", []) => Value::int(d.millis),
        ("toString", []) => Value::str(dur_string(&d)),
        ("plus", [b]) => arithmetic(recv, b, true)?,
        ("minus", [b]) => arithmetic(recv, b, false)?,
        ("compareTo", [b]) => Value::int(compare(recv, b)? as i64),
        _ => return None,
    })
}

/// `Date`'s statics: `Date.parse(pattern, text)`.
pub(super) fn static_call(vm: &mut VM, class: &str, method: &str, args: &[Value]) -> Option<Value> {
    match (class, method, args) {
        ("Date", "parse", [p, t]) => match parse(&groovy_str(p), &groovy_str(t)) {
            Some(ms) => Some(new_date(ms)),
            None => {
                raise(
                    vm,
                    "ParseException",
                    &format!("Unparseable date: \"{}\"", groovy_str(t)),
                );
                Some(Value::Undef)
            }
        },
        ("TimeZone", "getTimeZone", [id]) => Some(time_zone(&groovy_str(id))),
        ("TimeZone", "getDefault", []) => Some(heap_push(HeapObj::TimeZone(Zone { fixed: None }))),
        _ => None,
    }
}

/// `new Date()` / `new Date(millis)`.
pub(super) fn construct(args: &[Value]) -> Option<Value> {
    match args {
        [] => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            Some(new_date(now))
        }
        [Value::Int(ms)] => Some(new_date(*ms)),
        _ => None,
    }
}
