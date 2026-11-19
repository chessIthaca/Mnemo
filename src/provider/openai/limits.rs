// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Machine-wide endpoint limits (plan d3336a8a step 4): the per-endpoint
//! concurrency slots and the shared 429 cooldown.
//!
//! Both live in the machine-wide run folder (`<config dir>/run/`, the same
//! rendezvous the heavy-command slots use), keyed by an 8-hex hash of the
//! endpoint name, so every Mnemo2 instance on the machine shares them:
//!
//! - `endpoint-<hash>/slot-<i>.lock` — OS file locks, taken through
//!   [`HeavySlots::with_slots`] (the primitive the heavy-command limit uses,
//!   [`crate::host::machine_slots`]). A crash releases the lock. They are
//!   held only while a request is in flight: from just before the send until
//!   the response body is fully read.
//! - `endpoint-<hash>/cooldown` — an until-ms timestamp written when the
//!   endpoint answers 429, honoured by every instance before it sends.
//!
//! Failure never blocks a request: an unreachable or unwritable run folder
//! degrades to sending unconstrained (the same contract as the heavy slots),
//! and one log line says so.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::host::machine_slots::{HeavySlots, SlotGuard};

/// The cooldown file inside an endpoint's run folder.
const COOLDOWN_FILE: &str = "cooldown";

/// The read-modify-write lock for the cooldown file.
const COOLDOWN_LOCK: &str = "cooldown.lock";

/// The cooldown written for a 429 that carries no usable `Retry-After`.
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(30);

/// The longest ONE wait sleeps. A longer `Retry-After` is honoured in pieces:
/// the wait stops here, the send goes out, and another 429 re-arms the
/// cooldown — so a clock-skewed value can never freeze an endpoint forever.
pub const MAX_COOLDOWN_WAIT: Duration = Duration::from_secs(300);

/// The endpoint's machine-wide run folder: `<run dir>/endpoint-<8 hex>`,
/// where the hash is the first four bytes of the sha-256 of the name.
///
/// Hashed (not the raw name) so any endpoint name — spaces, slashes, case
/// differences on a case-insensitive file system — maps to one folder.
pub fn endpoint_dir(run_dir: &Path, name: &str) -> PathBuf {
    let digest = Sha256::digest(name.as_bytes());
    let mut hex = String::with_capacity(8);
    for byte in &digest[..4] {
        hex.push_str(&format!("{byte:02x}"));
    }
    run_dir.join(format!("endpoint-{hex}"))
}

/// One endpoint's machine-wide limits: the concurrency slots and the shared
/// 429 cooldown, both under [`endpoint_dir`].
pub struct EndpointLimits {
    name: String,
    dir: PathBuf,
    max_concurrent: Option<u32>,
}

impl EndpointLimits {
    /// The limits of endpoint `name`, whose run folder lives under `run_dir`
    /// (`<config dir>/run`). `max_concurrent` `None` = no slot at all; the
    /// cooldown is shared either way.
    pub fn new(run_dir: &Path, name: &str, max_concurrent: Option<u32>) -> Self {
        Self {
            name: name.to_string(),
            dir: endpoint_dir(run_dir, name),
            max_concurrent,
        }
    }

    /// Take one of the endpoint's slots for one send, honouring the shared
    /// cooldown on BOTH sides of the wait.
    ///
    /// 1. Sleep out a cooldown another request (any instance) left behind.
    /// 2. Take a slot, waiting while every one is held.
    /// 3. Re-read the cooldown and sleep out whatever remains — WITH the slot
    ///    held.
    ///
    /// Step 3 is what keeps a queued request honest (finding
    /// queuedIgnoresCooldown): the request that releases a slot is often the
    /// one whose 429 armed the cooldown, so a request that only checked before
    /// queuing would send straight into the new cooldown. Sleeping here keeps
    /// the queue ordered: nobody behind the 429 slips past it.
    ///
    /// An unlimited endpoint and an unusable run folder both return
    /// [`SlotGuard::none`]; both still honour the cooldown.
    pub async fn acquire(&self) -> SlotGuard {
        self.wait_cooldown().await;
        let Some(max) = self.max_concurrent else {
            return SlotGuard::none();
        };
        let slots = HeavySlots::with_slots(self.dir.clone(), max.max(1) as usize);
        let name = self.name.clone();
        let slot = slots
            .acquire(move |count| {
                eprintln!(
                    "waiting for a request slot at endpoint '{name}' ({count} in flight)"
                );
            })
            .await;
        self.wait_cooldown().await;
        slot
    }

    /// Sleep out a cooldown another request (any instance) left behind.
    ///
    /// At most [`MAX_COOLDOWN_WAIT`] per send: a longer cooldown simply makes
    /// the next 429 wait again.
    pub async fn wait_cooldown(&self) {
        let Some(until_ms) = cooldown_until_ms(&self.dir) else {
            return;
        };
        let now = now_ms();
        if until_ms <= now {
            return;
        }
        let wait = Duration::from_millis(until_ms - now).min(MAX_COOLDOWN_WAIT);
        eprintln!(
            "waiting {} for the shared cooldown of endpoint '{}'",
            human_duration(wait),
            self.name
        );
        tokio::time::sleep(wait).await;
    }

    /// Record a 429: every instance waits out the cooldown before its next
    /// send. Keeps the larger of the existing and the new value, under the
    /// cooldown lock so two writers cannot shrink each other's value.
    pub async fn note_rate_limited(&self, retry_after: Option<&str>) {
        let now = now_ms();
        let until = retry_after
            .and_then(|value| parse_retry_after(value, now))
            .unwrap_or_else(|| now + DEFAULT_COOLDOWN.as_millis() as u64);
        if until <= now {
            // A `Retry-After` naming the past asks for no wait at all.
            return;
        }
        let _lock = self.lock_cooldown().await;
        let value = cooldown_until_ms(&self.dir).map_or(until, |old| old.max(until));
        if let Err(error) = crate::config::write_atomic(
            &self.dir.join(COOLDOWN_FILE),
            &format!("{value}\n"),
        ) {
            warn_once(&self.dir, &error.to_string());
        }
    }

    /// Take the cooldown's read-modify-write lock, waiting briefly for a
    /// competing writer. `None` (with one log line) when the lock cannot be
    /// taken: the write then proceeds unlocked, last-writer-wins — a shorter
    /// cooldown, never a failed request.
    async fn lock_cooldown(&self) -> Option<std::fs::File> {
        if let Err(error) = std::fs::create_dir_all(&self.dir) {
            warn_once(&self.dir, &error.to_string());
            return None;
        }
        let file = match std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join(COOLDOWN_LOCK))
        {
            Ok(file) => file,
            Err(error) => {
                warn_once(&self.dir, &error.to_string());
                return None;
            }
        };
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            match file.try_lock() {
                Ok(()) => return Some(file),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    warn_once(&self.dir, &error.to_string());
                    return None;
                }
            }
        }
    }
}

/// The cooldown until-ms an endpoint's run folder carries, `None` when there
/// is none or it cannot be read.
fn cooldown_until_ms(dir: &Path) -> Option<u64> {
    std::fs::read_to_string(dir.join(COOLDOWN_FILE))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// The folders this process already warned about: ONE log line each, so a
/// broken run folder cannot flood the log on every request.
static WARNED: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> =
    std::sync::OnceLock::new();

/// Report an unusable endpoint run folder once per process.
fn warn_once(dir: &Path, error: &str) {
    let seen = WARNED.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()));
    let key = dir.display().to_string();
    let first = seen
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key.clone());
    if first {
        eprintln!(
            "warning: endpoint limit folder {key} is unusable ({error}); requests run \
             without endpoint limits"
        );
    }
}

/// Unix milliseconds now (0 before the epoch, which never happens).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// The absolute until-ms a `Retry-After` header names: delta-seconds
/// (`"120"`) or an HTTP-date. `None` when the value is unparseable or the
/// date lies in the past (no wait to take).
pub fn parse_retry_after(value: &str, now_ms: u64) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(now_ms.saturating_add(seconds.saturating_mul(1000)));
    }
    let at = http_date_ms(value)?;
    (at > now_ms).then_some(at)
}

/// Parse an HTTP-date (RFC 7231 §7.1.1.1) into unix milliseconds.
///
/// All three formats a recipient must accept are handled: the preferred
/// IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), the obsolete RFC 850 form
/// (`Sunday, 06-Nov-94 08:49:37 GMT`) and asctime (`Sun Nov  6 08:49:37
/// 1994`). Everything is UTC.
fn http_date_ms(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Some((_weekday, rest)) = text.split_once(',') {
        let parts: Vec<&str> = rest.split_whitespace().collect();
        return match parts.as_slice() {
            // IMF-fixdate: "06 Nov 1994 08:49:37 GMT".
            [day, month, year, time, zone] => {
                if !is_gmt(zone) {
                    return None;
                }
                combine(
                    year.parse::<i64>().ok()?,
                    month_number(month)?,
                    day.parse::<u32>().ok()?,
                    time,
                )
            }
            // RFC 850: "06-Nov-94 08:49:37 GMT" — a two-digit year windowed
            // the way RFC 7231 prescribes.
            [date, time, zone] => {
                if !is_gmt(zone) {
                    return None;
                }
                let mut it = date.split('-');
                let day = it.next()?.parse::<u32>().ok()?;
                let month = month_number(it.next()?)?;
                let year: i64 = it.next()?.parse().ok()?;
                if it.next().is_some() {
                    return None;
                }
                let year = match year {
                    0..=69 => 2000 + year,
                    70..=99 => 1900 + year,
                    _ => year,
                };
                combine(year, month, day, time)
            }
            _ => None,
        };
    }
    // asctime: "Sun Nov  6 08:49:37 1994".
    let parts: Vec<&str> = text.split_whitespace().collect();
    match parts.as_slice() {
        [_weekday, month, day, time, year] => combine(
            year.parse::<i64>().ok()?,
            month_number(month)?,
            day.parse::<u32>().ok()?,
            time,
        ),
        _ => None,
    }
}

/// Whether `zone` names UTC in the way the HTTP date formats do.
fn is_gmt(zone: &str) -> bool {
    zone.eq_ignore_ascii_case("GMT") || zone.eq_ignore_ascii_case("UTC")
}

/// The month number of a three-letter English month name.
fn month_number(name: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    MONTHS
        .iter()
        .position(|month| month.eq_ignore_ascii_case(name))
        .map(|index| index as u32 + 1)
}

/// Combine a civil date and an `HH:MM:SS` time into unix milliseconds.
fn combine(year: i64, month: u32, day: u32, time: &str) -> Option<u64> {
    let mut parts = time.split(':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some()
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
        || day == 0
        || day > 31
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second;
    (secs >= 0).then(|| secs as u64 * 1000)
}

/// Days since 1970-01-01 for a proleptic-Gregorian civil date (Howard
/// Hinnant's `days_from_civil`, valid for the whole range we parse).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// A compact human duration: `45s`, `1m20s`, `12m`.
fn human_duration(wait: Duration) -> String {
    let secs = wait.as_secs().max(1);
    if secs < 60 {
        return format!("{secs}s");
    }
    let (minutes, rest) = (secs / 60, secs % 60);
    if rest == 0 {
        format!("{minutes}m")
    } else {
        format!("{minutes}m{rest}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique run dir under the OS temp folder, removed by the caller.
    fn temp_run_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "mnemo-endpoint-limits-{}-{}",
            std::process::id(),
            now_ms()
        ))
    }

    #[test]
    fn retry_after_delta_seconds() {
        assert_eq!(parse_retry_after("120", 1_000), Some(121_000));
        assert_eq!(parse_retry_after(" 0 ", 1_000), Some(1_000));
        assert_eq!(parse_retry_after("soon", 1_000), None);
        assert_eq!(parse_retry_after("", 1_000), None);
    }

    #[test]
    fn retry_after_http_date_parsed() {
        // The RFC 7231 example date: Sun, 06 Nov 1994 08:49:37 GMT.
        let at = http_date_ms("Sun, 06 Nov 1994 08:49:37 GMT").expect("IMF-fixdate");
        assert_eq!(at, 784_111_777_000);
        // The obsolete RFC 850 form names the same instant.
        assert_eq!(http_date_ms("Sunday, 06-Nov-94 08:49:37 GMT"), Some(at));
        // asctime too (with its double space before a one-digit day).
        assert_eq!(http_date_ms("Sun Nov  6 08:49:37 1994"), Some(at));
        // Relative to a past instant it yields that absolute time.
        assert_eq!(parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", 0), Some(at));
        // A date in the past asks for no wait.
        assert_eq!(parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", at), None);
        // Garbage is rejected.
        assert_eq!(http_date_ms("Sun, 06 Foo 1994 08:49:37 GMT"), None);
        assert_eq!(http_date_ms("Sun, 06 Nov 1994 08:49:37 PST"), None);
    }

    #[test]
    fn endpoint_dir_is_the_hashed_name() {
        let dir = endpoint_dir(Path::new("/tmp/run"), "openai-prod");
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        assert!(name.starts_with("endpoint-"), "{name}");
        assert_eq!(name.len(), "endpoint-".len() + 8, "{name}");
        // Stable across calls, distinct per name, and case-sensitive.
        assert_eq!(dir, endpoint_dir(Path::new("/tmp/run"), "openai-prod"));
        assert_ne!(dir, endpoint_dir(Path::new("/tmp/run"), "OpenAI-prod"));
        assert_eq!(
            dir.parent().and_then(|p| p.to_str()),
            Some("/tmp/run"),
            "the folder lives directly under the run dir"
        );
    }

    #[test]
    fn cooldown_file_round_trips_through_the_guard() {
        let tmp = temp_run_dir();
        let limits = EndpointLimits::new(&tmp, "ep", Some(2));
        assert_eq!(cooldown_until_ms(&limits.dir), None);
        std::fs::create_dir_all(&limits.dir).unwrap();
        crate::config::write_atomic(&limits.dir.join(COOLDOWN_FILE), "1234\n").unwrap();
        assert_eq!(cooldown_until_ms(&limits.dir), Some(1234));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Finding cooldownMaxUntested: `note_rate_limited` keeps the LARGER of
    /// the stored and the new value, so a short `Retry-After` arriving while
    /// a long cooldown is armed can never shrink it (every instance would
    /// otherwise start sending again early), while a longer one extends it.
    #[tokio::test]
    async fn a_shorter_retry_after_never_shrinks_the_cooldown() {
        let tmp = temp_run_dir();
        let limits = EndpointLimits::new(&tmp, "ep", None);

        limits.note_rate_limited(Some("120")).await;
        let long = cooldown_until_ms(&limits.dir).expect("the first 429 writes a cooldown");
        let now = now_ms();
        assert!(
            long >= now + 110_000 && long <= now + 130_000,
            "~120 s out, got {long} vs now {now}"
        );

        // A second 429 asking for ONE second must not shrink it.
        limits.note_rate_limited(Some("1")).await;
        assert_eq!(
            cooldown_until_ms(&limits.dir),
            Some(long),
            "a shorter Retry-After must keep the longer cooldown"
        );

        // A 429 without a usable Retry-After (the 30 s default) must not
        // shrink it either.
        limits.note_rate_limited(None).await;
        assert_eq!(cooldown_until_ms(&limits.dir), Some(long));

        // A LONGER Retry-After does extend it.
        limits.note_rate_limited(Some("600")).await;
        let extended = cooldown_until_ms(&limits.dir).expect("still armed");
        let now = now_ms();
        assert!(
            extended > long && extended >= now + 590_000,
            "~600 s out, got {extended} vs now {now} (old {long})"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
