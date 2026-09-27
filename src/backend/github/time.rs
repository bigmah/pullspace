/// Days between 1970-01-01 and a date, by Howard Hinnant's `days_from_civil` —
/// the standard closed form, and shorter than the leap-year rules it encodes.
pub(super) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    // March-first years, so the leap day lands at the end where it costs
    // nothing to reason about.
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Seconds since the epoch for `2024-05-06T07:08:09Z`, which is the only shape
/// GitHub writes a time in.
///
/// `None` for anything else — the only thing read out of the answer is a
/// duration, and a missing duration is a line that says a little less rather
/// than a row that fails to draw.
pub(super) fn epoch_secs(ts: &str) -> Option<i64> {
    let (date, rest) = ts.split_once('T')?;
    let mut ymd = date.split('-');
    let year: i64 = ymd.next()?.parse().ok()?;
    let month: i64 = ymd.next()?.parse().ok()?;
    let day: i64 = ymd.next()?.parse().ok()?;

    let mut hms = rest.split(':');
    let hour: i64 = hms.next()?.parse().ok()?;
    let minute: i64 = hms.next()?.parse().ok()?;
    // Whatever follows the seconds — the `Z`, a fraction, an offset — is past
    // what a duration in whole seconds is measured in.
    let secs: i64 = hms
        .next()?
        .split(['Z', '.', '+', '-'])
        .next()?
        .parse()
        .ok()?;

    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + secs)
}

/// Seconds since the epoch, now.
pub fn now_secs() -> i64 {
    // web_time reads `Date.now()` in a page; std's SystemTime panics there.
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// How far back `ts` is from `now`, as a count and the unit it is a count of —
/// the long word for it, and the letter. Zero of nothing is "under a minute".
///
/// Whole units, rounded down, in the largest unit that leaves a number worth
/// reading: a pull request is "3 weeks" old, not "23 days" and not "0 months".
pub(super) fn age(ts: &str, now: i64) -> Option<(i64, &'static str, &'static str)> {
    const MIN: i64 = 60;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    // Two machines, two clocks: something from a few seconds into the future
    // happened just now.
    let secs = (now - epoch_secs(ts)?).max(0);
    Some(match secs {
        s if s < MIN => (0, "", ""),
        s if s < HOUR => (s / MIN, "minute", "m"),
        s if s < DAY => (s / HOUR, "hour", "h"),
        s if s < 7 * DAY => (s / DAY, "day", "d"),
        s if s < 30 * DAY => (s / (7 * DAY), "week", "w"),
        s if s < 365 * DAY => (s / (30 * DAY), "month", "mo"),
        s => (s / (365 * DAY), "year", "y"),
    })
}

/// When something happened, the way somebody would say it: `3 days ago`.
///
/// Empty for a time that cannot be read, so the sentence it goes in says a
/// little less rather than saying something wrong.
pub fn ago(ts: &str, now: i64) -> String {
    match age(ts, now) {
        None => String::new(),
        Some((0, ..)) => "just now".to_string(),
        Some((1, unit, _)) => format!("1 {unit} ago"),
        Some((n, unit, _)) => format!("{n} {unit}s ago"),
    }
}

/// The same, in the width of a column: `3d`.
pub fn ago_short(ts: &str, now: i64) -> String {
    match age(ts, now) {
        None => String::new(),
        Some((0, ..)) => "now".to_string(),
        Some((n, _, letter)) => format!("{n}{letter}"),
    }
}

/// How long something took, in the words a build log uses.
///
/// Both ends are needed: a check that is still running has no end to measure
/// to, and one that never said when it started cannot be measured at all.
pub(super) fn took(started: Option<&str>, finished: Option<&str>) -> String {
    let Some((from, to)) = started
        .and_then(epoch_secs)
        .zip(finished.and_then(epoch_secs))
    else {
        return String::new();
    };
    let secs = to - from;
    // Two machines, two clocks. A negative duration is not one to print.
    if secs < 0 {
        return String::new();
    }
    match (secs / 3_600, (secs % 3_600) / 60, secs % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m {s}s"),
        (h, m, _) => format!("{h}h {m}m"),
    }
}
