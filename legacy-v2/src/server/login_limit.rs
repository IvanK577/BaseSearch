//! Small process-local login backoff keyed independently by the real peer IP
//! and normalized username. The bounded maps intentionally avoid persistent
//! account lockouts while still protecting Argon2 verification from bursts.

use std::collections::HashMap;
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_TRACKED_KEYS: usize = 2_048;
const STALE_AFTER: Duration = Duration::from_secs(15 * 60);
const ATTEMPTS_BEFORE_BACKOFF: u32 = 5;
const MAX_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
struct Policy {
    max_tracked_keys: usize,
    stale_after: Duration,
    attempts_before_backoff: u32,
    max_backoff: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_tracked_keys: MAX_TRACKED_KEYS,
            stale_after: STALE_AFTER,
            attempts_before_backoff: ATTEMPTS_BEFORE_BACKOFF,
            max_backoff: MAX_BACKOFF,
        }
    }
}

#[derive(Clone, Copy)]
struct Entry {
    attempts: u32,
    blocked_until: Instant,
    last_seen: Instant,
}

#[derive(Default)]
struct State {
    ips: HashMap<IpAddr, Entry>,
    usernames: HashMap<String, Entry>,
}

pub(crate) struct LoginRateLimiter {
    policy: Policy,
    state: Mutex<State>,
}

impl Default for LoginRateLimiter {
    fn default() -> Self {
        Self {
            policy: Policy::default(),
            state: Mutex::new(State::default()),
        }
    }
}

impl LoginRateLimiter {
    pub(crate) fn check(&self, peer_ip: IpAddr, username: &str) -> Result<(), Duration> {
        self.check_at(peer_ip, username, Instant::now())
    }

    pub(crate) fn record_failure(&self, peer_ip: IpAddr, username: &str) {
        self.record_failure_at(peer_ip, username, Instant::now());
    }

    pub(crate) fn clear(&self, peer_ip: IpAddr, username: &str) {
        let username = normalize_username(username);
        if let Ok(mut state) = self.state.lock() {
            state.ips.remove(&peer_ip);
            state.usernames.remove(&username);
        }
    }

    fn check_at(&self, peer_ip: IpAddr, username: &str, now: Instant) -> Result<(), Duration> {
        let username = normalize_username(username);
        let Ok(mut state) = self.state.lock() else {
            return Err(Duration::from_secs(1));
        };
        prune_stale(&mut state.ips, now, self.policy.stale_after);
        prune_stale(&mut state.usernames, now, self.policy.stale_after);

        let ip_wait = remaining_wait(state.ips.get(&peer_ip), now);
        let username_wait = remaining_wait(state.usernames.get(&username), now);
        let wait = ip_wait.max(username_wait);
        if !wait.is_zero() {
            return Err(wait);
        }

        Ok(())
    }

    fn record_failure_at(&self, peer_ip: IpAddr, username: &str, now: Instant) {
        let username = normalize_username(username);
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        prune_stale(&mut state.ips, now, self.policy.stale_after);
        prune_stale(&mut state.usernames, now, self.policy.stale_after);
        reserve_key(&mut state.ips, peer_ip, now, self.policy);
        reserve_key(&mut state.usernames, username, now, self.policy);
    }

    #[cfg(test)]
    fn with_policy(policy: Policy) -> Self {
        Self {
            policy,
            state: Mutex::new(State::default()),
        }
    }

    #[cfg(test)]
    fn entry_counts(&self) -> (usize, usize) {
        let state = self.state.lock().unwrap();
        (state.ips.len(), state.usernames.len())
    }
}

fn normalize_username(username: &str) -> String {
    username
        .trim()
        .chars()
        .flat_map(char::to_lowercase)
        .take(128)
        .collect()
}

fn remaining_wait(entry: Option<&Entry>, now: Instant) -> Duration {
    entry
        .and_then(|entry| entry.blocked_until.checked_duration_since(now))
        .unwrap_or(Duration::ZERO)
}

fn reserve_key<K>(entries: &mut HashMap<K, Entry>, key: K, now: Instant, policy: Policy)
where
    K: Clone + Eq + Hash,
{
    if !entries.contains_key(&key) && entries.len() >= policy.max_tracked_keys {
        evict_oldest(entries);
    }
    let entry = entries.entry(key).or_insert(Entry {
        attempts: 0,
        blocked_until: now,
        last_seen: now,
    });
    entry.attempts = entry.attempts.saturating_add(1);
    entry.last_seen = now;
    if entry.attempts >= policy.attempts_before_backoff {
        let exponent = entry
            .attempts
            .saturating_sub(policy.attempts_before_backoff)
            .min(31);
        let seconds = 1u64.checked_shl(exponent).unwrap_or(u64::MAX);
        let backoff = Duration::from_secs(seconds).min(policy.max_backoff);
        entry.blocked_until = now + backoff;
    }
}

fn prune_stale<K>(entries: &mut HashMap<K, Entry>, now: Instant, stale_after: Duration) {
    entries.retain(|_, entry| {
        now.checked_duration_since(entry.last_seen)
            .is_none_or(|age| age < stale_after)
    });
}

fn evict_oldest<K>(entries: &mut HashMap<K, Entry>)
where
    K: Clone + Eq + Hash,
{
    if let Some(oldest) = entries
        .iter()
        .min_by_key(|(_, entry)| entry.last_seen)
        .map(|(key, _)| key.clone())
    {
        entries.remove(&oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last_octet: u8) -> IpAddr {
        IpAddr::from([192, 0, 2, last_octet])
    }

    #[test]
    fn peer_ip_and_normalized_username_have_independent_buckets() {
        let limiter = LoginRateLimiter::default();
        let now = Instant::now();
        for attempt in 0..5 {
            assert!(
                limiter
                    .check_at(ip(1), &format!("unknown-{attempt}"), now)
                    .is_ok()
            );
            limiter.record_failure_at(ip(1), &format!("unknown-{attempt}"), now);
        }
        assert!(limiter.check_at(ip(1), "new-name", now).is_err());

        let limiter = LoginRateLimiter::default();
        for peer in 1..=5 {
            assert!(limiter.check_at(ip(peer), " OWNER ", now).is_ok());
            limiter.record_failure_at(ip(peer), " OWNER ", now);
        }
        assert!(limiter.check_at(ip(6), "owner", now).is_err());
    }

    #[test]
    fn successful_login_clears_both_buckets() {
        let limiter = LoginRateLimiter::default();
        let now = Instant::now();
        for _ in 0..5 {
            assert!(limiter.check_at(ip(1), "owner", now).is_ok());
            limiter.record_failure_at(ip(1), "owner", now);
        }
        assert!(limiter.check_at(ip(1), "owner", now).is_err());
        limiter.clear(ip(1), "OWNER");
        assert!(limiter.check_at(ip(1), "owner", now).is_ok());
    }

    #[test]
    fn backoff_starts_when_the_failure_finishes() {
        let limiter = LoginRateLimiter::default();
        let started = Instant::now();
        let finished = started + Duration::from_secs(10);

        for _ in 0..4 {
            assert!(limiter.check_at(ip(1), "owner", started).is_ok());
            limiter.record_failure_at(ip(1), "owner", started);
        }
        assert!(limiter.check_at(ip(1), "owner", started).is_ok());
        limiter.record_failure_at(ip(1), "owner", finished);

        assert!(limiter.check_at(ip(1), "owner", finished).is_err());
    }

    #[test]
    fn maps_are_bounded_and_stale_entries_are_removed() {
        let limiter = LoginRateLimiter::with_policy(Policy {
            max_tracked_keys: 2,
            stale_after: Duration::from_secs(10),
            attempts_before_backoff: 5,
            max_backoff: Duration::from_secs(60),
        });
        let start = Instant::now();
        for peer in 1..=3 {
            assert!(
                limiter
                    .check_at(
                        ip(peer),
                        &format!("user-{peer}"),
                        start + Duration::from_secs(peer.into()),
                    )
                    .is_ok()
            );
            limiter.record_failure_at(
                ip(peer),
                &format!("user-{peer}"),
                start + Duration::from_secs(peer.into()),
            );
        }
        assert_eq!(limiter.entry_counts(), (2, 2));

        assert!(
            limiter
                .check_at(ip(10), "fresh", start + Duration::from_secs(20))
                .is_ok()
        );
        limiter.record_failure_at(ip(10), "fresh", start + Duration::from_secs(20));
        assert_eq!(limiter.entry_counts(), (1, 1));
    }
}
