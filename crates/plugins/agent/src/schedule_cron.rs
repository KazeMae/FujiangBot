use chrono::{DateTime, Datelike, Duration, Local, Timelike};

struct Cron {
    minute: u64,
    hour: u64,
    dom: u64,
    month: u64,
    dow: u64,
    dom_star: bool,
    dow_star: bool,
}

pub(crate) fn next_cron(expr: &str, after: i64) -> Option<i64> {
    let cron = parse(expr)?;
    let start = DateTime::from_timestamp(after, 0)?.with_timezone(&Local);
    let mut cursor = start.with_second(0)?.with_nanosecond(0)? + Duration::minutes(1);
    let end = cursor + Duration::days(366 * 4 + 2);
    while cursor < end {
        if cron.hit(cursor) {
            return Some(cursor.timestamp());
        }
        cursor += Duration::minutes(1);
    }
    None
}

fn parse(expr: &str) -> Option<Cron> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }
    Some(Cron {
        minute: parse_field(parts[0], 0, 59)?,
        hour: parse_field(parts[1], 0, 23)?,
        dom: parse_field(parts[2], 1, 31)?,
        month: parse_field(parts[3], 1, 12)?,
        dow: parse_dow(parts[4])?,
        dom_star: parts[2] == "*",
        dow_star: parts[4] == "*",
    })
}

fn parse_dow(raw: &str) -> Option<u64> {
    let mut bits = parse_field(raw, 0, 7)?;
    if bits & (1 << 7) != 0 {
        bits |= 1;
        bits &= !(1 << 7);
    }
    Some(bits)
}

fn parse_field(raw: &str, min: u32, max: u32) -> Option<u64> {
    if raw.is_empty() {
        return None;
    }
    let mut bits = 0u64;
    for part in raw.split(',') {
        if part.is_empty() {
            return None;
        }
        let (span, step_text) = match part.split_once('/') {
            Some((span, step)) => (span, step),
            None => (part, "1"),
        };
        if span.is_empty() || step_text.is_empty() {
            return None;
        }
        let step: u32 = step_text.parse().ok()?;
        if step == 0 {
            return None;
        }
        let (start, end) = if span == "*" {
            (min, max)
        } else if let Some((left, right)) = span.split_once('-') {
            if left.is_empty() || right.is_empty() || right.contains('-') {
                return None;
            }
            let start = left.parse::<u32>().ok()?;
            let end = right.parse::<u32>().ok()?;
            if start < min || end > max || start > end {
                return None;
            }
            (start, end)
        } else {
            let value = span.parse::<u32>().ok()?;
            if value < min || value > max {
                return None;
            }
            (value, value)
        };
        let mut value = start;
        loop {
            bits |= 1u64 << value;
            let Some(next) = value.checked_add(step) else {
                break;
            };
            if next > end {
                break;
            }
            value = next;
        }
    }
    Some(bits)
}

impl Cron {
    fn hit(&self, at: DateTime<Local>) -> bool {
        let dom_ok = on(self.dom, at.day());
        let dow_ok = on(self.dow, at.weekday().num_days_from_sunday());
        let date_ok = match (self.dom_star, self.dow_star) {
            (true, true) => true,
            (false, true) => dom_ok,
            (true, false) => dow_ok,
            (false, false) => dom_ok || dow_ok,
        };
        on(self.minute, at.minute())
            && on(self.hour, at.hour())
            && on(self.month, at.month())
            && date_ok
    }
}

fn on(bits: u64, value: u32) -> bool {
    value < 64 && bits & (1u64 << value) != 0
}
