//! `java.util.Date`, `java.util.TimeZone` and `groovy.time.TimeCategory`.
//!
//! A `Date` is its millisecond instant. The local zone is the region `$TZ` or
//! `/etc/localtime` names — the zone the JVM defaults to as well — or the C
//! library's (`localtime_r`) when no tz-database file backs it; an
//! explicit `TimeZone` is a fixed `GMT±hh:mm` offset, a region from the
//! system tz database (see [`tz`]), or `GMT` for an id neither knows, as the
//! JDK answers. `java.sql.Timestamp` and `GregorianCalendar` ride the same
//! instants.
//!
//! Formatting follows `SimpleDateFormat` for the letters scripts use (`y M d
//! H h m s S a E z Z` and quoted text); `TimeCategory` durations render as
//! `BaseDuration.toString` does (`2 days, 3 hours, 5.007 seconds`).

use super::*;

const DAY: i64 = 86_400_000;

/// A `TimeZone`: its id and how it maps an instant to local time.
#[derive(Clone)]
pub(super) struct Zone {
    id: String,
    kind: ZoneKind,
}

#[derive(Clone)]
enum ZoneKind {
    /// The JVM default when no tz-database region backs it: the C library's
    /// local zone.
    Local,
    /// A fixed offset east of UTC, in seconds.
    Fixed(i64),
    /// A tz-database region.
    Region(std::rc::Rc<tz::TzData>),
}

/// What a zone answers at one instant.
struct ZoneAt {
    /// The UTC offset, seconds east.
    offset: i64,
    /// The standard offset in effect, seconds east.
    std: i64,
    /// `inDaylightTime`: the offset is not the standard one.
    dst: bool,
    /// The short name `SimpleDateFormat`'s `z` prints.
    format_name: String,
    /// The short name `Date.toString` prints.
    name: String,
}

/// `ZoneInfoFile.UTC1900`: `ZoneInfo`'s transition table starts here, and an
/// earlier instant reads the zone's raw offset, as standard time.
const UTC1900_MS: i64 = -2_208_988_800_000;
/// `ZoneInfoFile.LDT2100`: a zone without a recurring rule ends its table with
/// an entry at this local standard time, back to the raw offset.
const LDT2100_SECS: i64 = 4_102_444_800;

impl Zone {
    fn local() -> Zone {
        let id = default_zone_id();
        let kind = match tz::load(legacy_region(&id).unwrap_or(&id)) {
            Some(data) if jdk_zone(&id).is_some() => ZoneKind::Region(data),
            _ => ZoneKind::Local,
        };
        Zone { id, kind }
    }

    /// The offset, standard offset, daylight flag and short names at the
    /// instant `ms`.
    fn at(&self, ms: i64) -> ZoneAt {
        let plain = |offset: i64, name: String, dst: bool| ZoneAt {
            offset,
            std: if dst { offset - 3600 } else { offset },
            dst,
            format_name: name.clone(),
            name,
        };
        match &self.kind {
            ZoneKind::Local => {
                let (off, abbr, dst) = local_offset(ms);
                plain(off, jdk_short_name(&self.id, dst).unwrap_or(abbr), dst)
            }
            ZoneKind::Fixed(off) => plain(*off, self.id.clone(), false),
            ZoneKind::Region(data) => match jdk_zone(&self.id) {
                Some(jz) => jz.at(data, ms),
                None => {
                    let lt = data.at(ms.div_euclid(1000));
                    plain(lt.offset, java_short_name(&lt.abbr, lt.offset), lt.dst)
                }
            },
        }
    }

    /// The instant whose local time in this zone is `local` (milliseconds
    /// of local wall time since the epoch).
    fn instant_of_local(&self, local: i64) -> i64 {
        let mut guess = local;
        for _ in 0..3 {
            guess = local - self.at(guess).offset * 1000;
        }
        guess
    }

    /// `getRawOffset()`, in seconds.
    fn raw_offset(&self) -> i64 {
        match &self.kind {
            ZoneKind::Fixed(off) => *off,
            ZoneKind::Region(data) => match jdk_zone(&self.id) {
                Some(jz) => jz.raw_offset(),
                None => data.raw_offset(),
            },
            ZoneKind::Local => {
                let now = now_millis();
                let (off, _, dst) = local_offset(now);
                if dst {
                    off - 3600
                } else {
                    off
                }
            }
        }
    }

    /// `getDSTSavings()`, in seconds: what the recurring rule's daylight time
    /// adds, or nothing for a zone without one.
    fn dst_savings(&self) -> i64 {
        match &self.kind {
            ZoneKind::Region(data) => data.rule_savings().unwrap_or(0),
            ZoneKind::Fixed(_) => 0,
            ZoneKind::Local => i64::from(self.use_daylight_time()) * 3600,
        }
    }

    /// `useDaylightTime()`: the zone has a recurring daylight-time rule.
    fn use_daylight_time(&self) -> bool {
        match &self.kind {
            ZoneKind::Region(data) => data.uses_dst(),
            ZoneKind::Fixed(_) => false,
            ZoneKind::Local => {
                local_offset(now_millis()).2 || local_offset(now_millis() + 182 * DAY).2
            }
        }
    }

    /// `observesDaylightTime()`: a recurring rule, or daylight time now or at
    /// any later transition of the table.
    fn observes_daylight_time(&self) -> bool {
        if self.use_daylight_time() {
            return true;
        }
        match &self.kind {
            ZoneKind::Region(data) => {
                let now = now_millis();
                data.periods_from(now.div_euclid(1000))
                    .into_iter()
                    .any(|start| self.at(start * 1000).dst)
            }
            _ => false,
        }
    }
}

/// The JDK's own facts about each zone id, dumped from the oracle by
/// `meta/gen_zones.groovy`: its short names, the explicit CLDR daylight
/// offset `SimpleDateFormat` tests against, and its standard-offset history.
const ZONE_NAMES: &str = include_str!("meta/zones.tsv");

/// One `zones.tsv` row.
struct JdkZone {
    std_name: &'static str,
    dst_name: &'static str,
    /// The metazone's explicit daylight offset, seconds east.
    explicit_dst: Option<i64>,
    /// The first standard offset, then `(epoch second, offset)` per change.
    std0: i64,
    std_changes: Vec<(i64, i64)>,
}

impl JdkZone {
    fn parse(fields: &[&'static str]) -> Option<JdkZone> {
        let [_, std_name, dst_name, explicit, std0, changes @ ..] = fields else {
            return None;
        };
        let explicit_dst = match *explicit {
            "-" => None,
            text => Some(parse_zone_offset(text)?),
        };
        let std_changes = changes
            .iter()
            .map(|c| {
                let (t, off) = c.split_once(':')?;
                Some((t.parse().ok()?, off.parse().ok()?))
            })
            .collect::<Option<_>>()?;
        Some(JdkZone {
            std_name,
            dst_name,
            explicit_dst,
            std0: std0.parse().ok()?,
            std_changes,
        })
    }

    /// `ZoneInfoFile.getStandardOffset`: the standard offset at UTC second `t`.
    fn std_at(&self, t: i64) -> i64 {
        let i = self.std_changes.partition_point(|&(at, _)| at <= t);
        match i {
            0 => self.std0,
            _ => self.std_changes[i - 1].1,
        }
    }

    /// The last standard offset: `ZoneInfo`'s `rawOffset` field.
    fn last_std(&self) -> i64 {
        self.std_changes.last().map_or(self.std0, |&(_, off)| off)
    }

    /// `ZoneInfo.getRawOffset`: the last standard offset, or the one in
    /// effect now while a standard-offset change is still to come
    /// (`willGMTOffsetChange`).
    fn raw_offset(&self) -> i64 {
        let now = now_millis().div_euclid(1000);
        match self.std_changes.last() {
            Some(&(at, _)) if at > now => self.std_at(now),
            _ => self.last_std(),
        }
    }

    /// The zone at `ms` the way `ZoneInfo` answers: the tz database's offset,
    /// daylight time wherever that offset is not the JDK's standard offset,
    /// the raw offset before 1900, and the raw offset again after the 2100
    /// closing entry of a zone with no recurring rule.
    fn at(&self, data: &tz::TzData, ms: i64) -> ZoneAt {
        let raw = self.last_std();
        let past_table = !data.uses_dst() && ms.div_euclid(1000) >= LDT2100_SECS - raw;
        let (offset, std) = if ms < UTC1900_MS || past_table {
            (raw, raw)
        } else {
            let t = ms.div_euclid(1000);
            (data.at(t).offset, self.std_at(t))
        };
        let dst = offset != std;
        // `SimpleDateFormat` names daylight time by the metazone's explicit
        // daylight offset when it has one, whatever the transition says.
        let daylight = self.explicit_dst.map_or(dst, |d| d == offset);
        let name = |daylight: bool| {
            if daylight {
                self.dst_name
            } else {
                self.std_name
            }
            .to_string()
        };
        ZoneAt {
            offset,
            std,
            dst,
            format_name: name(daylight),
            name: name(dst),
        }
    }
}

/// `ZoneOffset.of` for the `+hh[:mm]` / `-hh[:mm]` forms, in seconds.
fn parse_zone_offset(text: &str) -> Option<i64> {
    let (sign, body) = match text.split_at_checked(1)? {
        ("+", b) => (1, b),
        ("-", b) => (-1, b),
        _ => return None,
    };
    let mut parts = body.split(':');
    let h: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next().map_or(Some(0), |m| m.parse().ok())?;
    Some(sign * (h * 3600 + m * 60))
}

thread_local! {
    static ZONE_INDEX: RefCell<Option<HashMap<&'static str, std::rc::Rc<JdkZone>>>> =
        const { RefCell::new(None) };
}

/// The JDK's row for zone `id`.
fn jdk_zone(id: &str) -> Option<std::rc::Rc<JdkZone>> {
    ZONE_INDEX.with(|z| {
        let mut slot = z.borrow_mut();
        let index = slot.get_or_insert_with(|| {
            ZONE_NAMES
                .lines()
                .filter(|l| !l.starts_with('#'))
                .filter_map(|l| {
                    let fields: Vec<&'static str> = l.split('\t').collect();
                    Some((*fields.first()?, std::rc::Rc::new(JdkZone::parse(&fields)?)))
                })
                .collect()
        });
        index.get(id).cloned()
    })
}

/// The short name (`z`) the JDK gives zone `id` in standard or daylight
/// time. It does not change with history, so 1906 New York is `EST` too.
fn jdk_short_name(id: &str, dst: bool) -> Option<String> {
    jdk_zone(id).map(|z| if dst { z.dst_name } else { z.std_name }.to_string())
}

/// A tz abbreviation as Java's short zone name: a numeric one (`+04`) has no
/// name and prints as `GMT+04:00`.
fn java_short_name(abbr: &str, offset: i64) -> String {
    if abbr.chars().any(|c| c.is_ascii_alphabetic()) {
        return abbr.to_string();
    }
    gmt_id(offset)
}

/// `GMT±hh:mm` for an offset in seconds.
fn gmt_id(offset: i64) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    let m = offset.abs() / 60;
    format!("GMT{sign}{:02}:{:02}", m / 60, m % 60)
}

/// The JVM default zone's id: `$TZ`, else the region `/etc/localtime` links
/// to.
fn default_zone_id() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        if !tz.is_empty() && !tz.starts_with('/') {
            return tz.to_string();
        }
    }
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|p| {
            let s = p.to_string_lossy().into_owned();
            s.split_once("zoneinfo/").map(|(_, id)| id.to_string())
        })
        .unwrap_or_else(|| "GMT".to_string())
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A `GregorianCalendar`: an instant and the zone its fields are read in.
#[derive(Clone)]
pub(super) struct Cal {
    ms: i64,
    zone: Zone,
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
    /// The name `Date.toString` prints, which reads the daylight flag alone.
    date_zone: String,
}

pub(super) fn date_millis(v: &Value) -> Option<i64> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::Date(ms) | HeapObj::Timestamp(ms)) => Some(*ms),
            _ => None,
        }),
        _ => None,
    }
}

fn is_timestamp(v: &Value) -> bool {
    matches!(v, Value::Obj(id) if HEAP.with(|h| matches!(h.borrow().get(*id as usize), Some(HeapObj::Timestamp(_)))))
}

fn cal_of(v: &Value) -> Option<Cal> {
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::Calendar(c)) => Some((**c).clone()),
            _ => None,
        }),
        _ => None,
    }
}

fn set_cal_millis(v: &Value, ms: i64) {
    if let Value::Obj(id) = v {
        HEAP.with(|h| {
            if let Some(HeapObj::Calendar(c)) = h.borrow_mut().get_mut(*id as usize) {
                c.ms = ms;
            }
        });
    }
}

fn new_cal(ms: i64, zone: Zone) -> Value {
    heap_push(HeapObj::Calendar(Box::new(Cal { ms, zone })))
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
    if is_timestamp(v) {
        return Some("java.sql.Timestamp".to_string());
    }
    if date_millis(v).is_some() {
        return Some("java.util.Date".to_string());
    }
    if zone_of(v).is_some() {
        return Some("sun.util.calendar.ZoneInfo".to_string());
    }
    if cal_of(v).is_some() {
        return Some("java.util.GregorianCalendar".to_string());
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

/// The local zone's offset (seconds east of UTC), abbreviation and daylight
/// flag at `ms`.
fn local_offset(ms: i64) -> (i64, String, bool) {
    let t = ms.div_euclid(1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if !ok {
        return (0, "UTC".to_string(), false);
    }
    let zone = if tm.tm_zone.is_null() {
        "UTC".to_string()
    } else {
        unsafe { std::ffi::CStr::from_ptr(tm.tm_zone) }
            .to_string_lossy()
            .into_owned()
    };
    let offset = tm.tm_gmtoff as i64;
    (offset, java_short_name(&zone, offset), tm.tm_isdst > 0)
}

fn fields(ms: i64, zone: &Zone) -> Fields {
    let at = zone.at(ms);
    let offset = at.offset;
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
        zone: at.format_name,
        date_zone: at.name,
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
    format_fields(&fields(ms, zone), pattern)
}

/// [`format`] over broken-down fields.
fn format_fields(f: &Fields, pattern: &str) -> String {
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
    // `Date.toString` names the zone by its daylight flag, where a `z` in a
    // pattern also tests the metazone's explicit daylight offset.
    let mut f = fields(ms, &Zone::local());
    f.zone = std::mem::take(&mut f.date_zone);
    format_fields(&f, "EEE MMM dd HH:mm:ss zzz yyyy")
}

/// `Timestamp.toString()`: `yyyy-mm-dd hh:mm:ss.f`, the fraction's trailing
/// zeros dropped (one digit kept), in the local zone.
fn timestamp_string(ms: i64) -> String {
    let base = format(ms, "yyyy-MM-dd HH:mm:ss", &Zone::local());
    let frac = format!("{:09}", ms.rem_euclid(1000) * 1_000_000);
    let trimmed = frac.trim_end_matches('0');
    format!("{base}.{}", if trimmed.is_empty() { "0" } else { trimmed })
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

/// A custom `GMT±h[h][[:]mm]` id's offset in seconds, as `ZoneInfoFile`
/// accepts it; `None` for anything else.
fn custom_offset(id: &str) -> Option<i64> {
    let rest = id.strip_prefix("GMT")?;
    let neg = match rest.chars().next()? {
        '+' => false,
        '-' => true,
        _ => return None,
    };
    let body = &rest[1..];
    let (h, m) = match body.split_once(':') {
        Some((h, m)) if (1..=2).contains(&h.len()) && m.len() == 2 => (h, m),
        None if (1..=2).contains(&body.len()) => (body, "0"),
        None if body.len() == 4 => (&body[..2], &body[2..]),
        _ => return None,
    };
    if !h.chars().chain(m.chars()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (h, m): (i64, i64) = (h.parse().ok()?, m.parse().ok()?);
    if h > 23 || m > 59 {
        return None;
    }
    let secs = h * 3600 + m * 60;
    Some(if neg { -secs } else { secs })
}

/// The JDK's three-letter legacy ids (`ZoneInfoFile`'s old mappings) that
/// name a region; `EST`, `MST` and `HST` are fixed offsets there.
fn legacy_region(id: &str) -> Option<&'static str> {
    Some(match id {
        "ACT" => "Australia/Darwin",
        "AET" => "Australia/Sydney",
        "AGT" => "America/Argentina/Buenos_Aires",
        "ART" => "Africa/Cairo",
        "AST" => "America/Anchorage",
        "BET" => "America/Sao_Paulo",
        "BST" => "Asia/Dhaka",
        "CAT" => "Africa/Harare",
        "CNT" => "America/St_Johns",
        "CST" => "America/Chicago",
        "CTT" => "Asia/Shanghai",
        "EAT" => "Africa/Addis_Ababa",
        "ECT" => "Europe/Paris",
        "IET" => "America/Indiana/Indianapolis",
        "IST" => "Asia/Kolkata",
        "JST" => "Asia/Tokyo",
        "MIT" => "Pacific/Apia",
        "NET" => "Asia/Yerevan",
        "NST" => "Pacific/Auckland",
        "PLT" => "Asia/Karachi",
        "PNT" => "America/Phoenix",
        "PRT" => "America/Puerto_Rico",
        "PST" => "America/Los_Angeles",
        "SST" => "Pacific/Guadalcanal",
        "VST" => "Asia/Ho_Chi_Minh",
        _ => return None,
    })
}

/// `TimeZone.getTimeZone(id)`.
pub(super) fn time_zone(id: &str) -> Value {
    heap_push(HeapObj::TimeZone(zone_named(id)))
}

fn zone_named(id: &str) -> Zone {
    let fixed = |id: String, off: i64| Zone {
        id,
        kind: ZoneKind::Fixed(off),
    };
    match id {
        "UTC" | "GMT" => return fixed(id.to_string(), 0),
        "EST" => return fixed(id.to_string(), -5 * 3600),
        "MST" => return fixed(id.to_string(), -7 * 3600),
        "HST" => return fixed(id.to_string(), -10 * 3600),
        _ => {}
    }
    if let Some(off) = custom_offset(id) {
        return fixed(gmt_id(off), off);
    }
    let region = legacy_region(id).unwrap_or(id);
    match tz::load(region) {
        Some(data) => Zone {
            id: id.to_string(),
            kind: ZoneKind::Region(data),
        },
        None => fixed("GMT".to_string(), 0),
    }
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
        let zone = Zone::local();
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
    let (off, _, _) = local_offset(ms);
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
    if let Some(ms) = cal_of(v).map(|c| c.ms) {
        return Some(format!("java.util.GregorianCalendar[time={ms}]"));
    }
    zone_of(v).map(|z| {
        format!(
            "sun.util.calendar.ZoneInfo[id=\"{}\",offset={}]",
            z.id,
            z.raw_offset() * 1000
        )
    })
}

/// The methods of `Date`, `TimeZone` and the durations.
pub(super) fn dispatch(recv: &Value, method: &str, args: &[Value]) -> Option<Value> {
    if let Some(ms) = date_millis(recv) {
        return Some(match (method, args) {
            ("getTime", []) => Value::int(ms),
            ("format", [p]) => Value::str(format(ms, &groovy_str(p), &Zone::local())),
            ("format", [p, z]) => {
                let zone = zone_of(z).unwrap_or_else(Zone::local);
                Value::str(format(ms, &groovy_str(p), &zone))
            }
            ("toTimestamp", []) => heap_push(HeapObj::Timestamp(ms)),
            ("toCalendar", []) => new_cal(ms, Zone::local()),
            ("toString", []) if is_timestamp(recv) => Value::str(timestamp_string(ms)),
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
            ("getID", []) => Value::str(z.id.clone()),
            ("getRawOffset", []) => Value::int(z.raw_offset() * 1000),
            ("getOffset", [t]) => Value::int(z.at(as_i64(t)?).offset * 1000),
            ("inDaylightTime", [d]) => Value::bool(z.at(date_millis(d)?).dst),
            ("useDaylightTime", []) => Value::bool(z.use_daylight_time()),
            ("observesDaylightTime", []) => Value::bool(z.observes_daylight_time()),
            ("getDSTSavings", []) => Value::int(z.dst_savings() * 1000),
            ("toString", []) => Value::str(text(recv)?),
            _ => return None,
        });
    }
    if let Some(cal) = cal_of(recv) {
        return calendar_method(recv, cal, method, args);
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
        ("TimeZone", "getDefault", []) => Some(heap_push(HeapObj::TimeZone(Zone::local()))),
        ("Calendar" | "GregorianCalendar", "getInstance", zone) if zone.len() <= 1 => {
            let zone = match zone.first() {
                Some(z) => zone_of(z)?,
                None => Zone::local(),
            };
            Some(new_cal(now_millis(), zone))
        }
        ("Calendar" | "GregorianCalendar", field, []) => calendar_constant(field).map(Value::int),
        _ => None,
    }
}

/// The `java.util.Calendar` field constants.
pub(super) fn calendar_constant(name: &str) -> Option<i64> {
    Some(match name {
        "ERA" => 0,
        "YEAR" => 1,
        "MONTH" => 2,
        "WEEK_OF_YEAR" => 3,
        "WEEK_OF_MONTH" => 4,
        "DATE" | "DAY_OF_MONTH" => 5,
        "DAY_OF_YEAR" => 6,
        "DAY_OF_WEEK" => 7,
        "DAY_OF_WEEK_IN_MONTH" => 8,
        "AM_PM" => 9,
        "HOUR" => 10,
        "HOUR_OF_DAY" => 11,
        "MINUTE" => 12,
        "SECOND" => 13,
        "MILLISECOND" => 14,
        "ZONE_OFFSET" => 15,
        "DST_OFFSET" => 16,
        "JANUARY" => 0,
        "FEBRUARY" => 1,
        "MARCH" => 2,
        "APRIL" => 3,
        "MAY" => 4,
        "JUNE" => 5,
        "JULY" => 6,
        "AUGUST" => 7,
        "SEPTEMBER" => 8,
        "OCTOBER" => 9,
        "NOVEMBER" => 10,
        "DECEMBER" => 11,
        "SUNDAY" => 1,
        "MONDAY" => 2,
        "TUESDAY" => 3,
        "WEDNESDAY" => 4,
        "THURSDAY" => 5,
        "FRIDAY" => 6,
        "SATURDAY" => 7,
        "AM" => 0,
        "PM" => 1,
        _ => return None,
    })
}

const MONTH_LEN: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

fn month_len(year: i64, month: i64) -> i64 {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    MONTH_LEN[(month - 1) as usize] + i64::from(month == 2 && leap)
}

/// One `Calendar.get(field)` value.
fn cal_field(f: &Fields, field: i64, offset_ms: (i64, i64)) -> Option<i64> {
    let doy = days_from_civil(f.year, f.month, f.day) - days_from_civil(f.year, 1, 1) + 1;
    Some(match field {
        0 => 1,
        1 => f.year,
        2 => f.month - 1,
        5 => f.day,
        6 => doy,
        7 => f.weekday + 1,
        8 => (f.day - 1) / 7 + 1,
        9 => i64::from(f.hour >= 12),
        10 => f.hour % 12,
        11 => f.hour,
        12 => f.minute,
        13 => f.second,
        14 => f.millis,
        15 => offset_ms.0,
        16 => offset_ms.1,
        _ => return None,
    })
}

/// The local wall time (ms since the epoch) of broken-down fields, which may
/// overflow their ranges the way a lenient calendar allows.
fn local_millis(
    year: i64,
    month0: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    millis: i64,
) -> i64 {
    let y = year + month0.div_euclid(12);
    let m = month0.rem_euclid(12) + 1;
    let days = days_from_civil(y, m, 1) + day - 1;
    days * DAY + ((hour * 60 + minute) * 60 + second) * 1000 + millis
}

/// The methods of a `GregorianCalendar`.
fn calendar_method(recv: &Value, cal: Cal, method: &str, args: &[Value]) -> Option<Value> {
    let f = fields(cal.ms, &cal.zone);
    // `ZONE_OFFSET` and `DST_OFFSET`: the standard offset in effect then, and
    // what daylight time adds to it.
    let at = cal.zone.at(cal.ms);
    let offsets = (at.std * 1000, (at.offset - at.std) * 1000);
    let rebuild = |y: i64, mo0: i64, d: i64, h: i64, mi: i64, s: i64, ms: i64| {
        cal.zone
            .instant_of_local(local_millis(y, mo0, d, h, mi, s, ms))
    };
    let parts = |f: &Fields| {
        (
            f.year,
            f.month - 1,
            f.day,
            f.hour,
            f.minute,
            f.second,
            f.millis,
        )
    };
    Some(match (method, args) {
        ("get" | "getAt", [field]) => Value::int(cal_field(&f, as_i64(field)?, offsets)?),
        ("getTime", []) => new_date(cal.ms),
        ("getTimeInMillis", []) => Value::int(cal.ms),
        ("setTimeInMillis", [t]) => {
            set_cal_millis(recv, as_i64(t)?);
            Value::Undef
        }
        ("setTime", [d]) => {
            set_cal_millis(recv, date_millis(d)?);
            Value::Undef
        }
        ("getTimeZone", []) => heap_push(HeapObj::TimeZone(cal.zone.clone())),
        ("clear", []) => {
            set_cal_millis(recv, rebuild(1970, 0, 1, 0, 0, 0, 0));
            Value::Undef
        }
        ("set" | "putAt", [field, v]) => {
            let (mut y, mut mo, mut d, mut h, mut mi, mut s, mut ms) = parts(&f);
            let v = as_i64(v)?;
            match as_i64(field)? {
                1 => y = v,
                2 => mo = v,
                5 => d = v,
                6 => {
                    mo = 0;
                    d = v;
                }
                10 => h = (h / 12) * 12 + v,
                11 => h = v,
                12 => mi = v,
                13 => s = v,
                14 => ms = v,
                _ => return None,
            }
            set_cal_millis(recv, rebuild(y, mo, d, h, mi, s, ms));
            Value::Undef
        }
        ("set", [y, mo, d, rest @ ..]) if rest.len() <= 3 => {
            let (_, _, _, mut h, mut mi, mut s, ms) = parts(&f);
            if let [hh, mm, tail @ ..] = rest {
                h = as_i64(hh)?;
                mi = as_i64(mm)?;
                if let [ss] = tail {
                    s = as_i64(ss)?;
                }
            }
            set_cal_millis(
                recv,
                rebuild(as_i64(y)?, as_i64(mo)?, as_i64(d)?, h, mi, s, ms),
            );
            Value::Undef
        }
        ("add", [field, n]) => {
            let n = as_i64(n)?;
            let (y, mo, d, h, mi, s, ms) = parts(&f);
            let t = match as_i64(field)? {
                // A year or month change keeps the day, pinned to the new
                // month's length.
                1 | 2 => {
                    let total = y * 12 + mo + if as_i64(field)? == 1 { n * 12 } else { n };
                    let (ny, nm0) = (total.div_euclid(12), total.rem_euclid(12));
                    let nd = d.min(month_len(ny, nm0 + 1));
                    rebuild(ny, nm0, nd, h, mi, s, ms)
                }
                5..=7 => rebuild(y, mo, d + n, h, mi, s, ms),
                3 => rebuild(y, mo, d + 7 * n, h, mi, s, ms),
                10 | 11 => cal.ms + n * 3_600_000,
                12 => cal.ms + n * 60_000,
                13 => cal.ms + n * 1000,
                14 => cal.ms + n,
                _ => return None,
            };
            set_cal_millis(recv, t);
            Value::Undef
        }
        ("getActualMaximum", [field]) => Value::int(match as_i64(field)? {
            5 => month_len(f.year, f.month),
            6 => 365 + i64::from(month_len(f.year, 2) == 29),
            2 => 11,
            11 => 23,
            12 | 13 => 59,
            14 => 999,
            _ => return None,
        }),
        ("getActualMinimum", [field]) => Value::int(match as_i64(field)? {
            5 | 6 => 1,
            _ => 0,
        }),
        ("format", [p]) => Value::str(format(cal.ms, &groovy_str(p), &cal.zone)),
        ("clone", []) => new_cal(cal.ms, cal.zone.clone()),
        ("before", [o]) => Value::bool(cal.ms < cal_of(o)?.ms),
        ("after", [o]) => Value::bool(cal.ms > cal_of(o)?.ms),
        ("compareTo", [o]) => Value::int(cal.ms.cmp(&cal_of(o)?.ms) as i64),
        ("equals", [o]) => Value::bool(cal_of(o).is_some_and(|c| c.ms == cal.ms)),
        ("toString", []) => Value::str(text(recv)?),
        _ => return None,
    })
}

/// `new Date()` / `new Date(millis)`.
pub(super) fn construct(args: &[Value]) -> Option<Value> {
    match args {
        [] => Some(new_date(now_millis())),
        [Value::Int(ms)] => Some(new_date(*ms)),
        _ => None,
    }
}

/// `new java.sql.Timestamp(millis)` / `new GregorianCalendar([zone])`.
pub(super) fn construct_named(class: &str, args: &[Value]) -> Option<Value> {
    match (class, args) {
        ("Timestamp", [Value::Int(ms)]) => Some(heap_push(HeapObj::Timestamp(*ms))),
        ("GregorianCalendar", []) => Some(new_cal(now_millis(), Zone::local())),
        ("GregorianCalendar", [z]) => Some(new_cal(now_millis(), zone_of(z)?)),
        _ => None,
    }
}
