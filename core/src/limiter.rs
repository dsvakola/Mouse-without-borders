//! Slows down PIN guessing: 5 wrong PINs from one address lock it out for 60 s.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

const MAX_FAILURES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct AttemptLimiter {
    entries: HashMap<IpAddr, (u32, Option<Instant>)>,
}

impl AttemptLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// `Err(seconds_left)` while the address is locked out.
    pub fn check(&mut self, ip: IpAddr, now: Instant) -> std::result::Result<(), u64> {
        if let Some((_, Some(until))) = self.entries.get(&ip) {
            if now < *until {
                return Err((*until - now).as_secs() + 1);
            }
            self.entries.remove(&ip);
        }
        Ok(())
    }

    pub fn record_failure(&mut self, ip: IpAddr, now: Instant) {
        let e = self.entries.entry(ip).or_insert((0, None));
        e.0 += 1;
        if e.0 >= MAX_FAILURES {
            e.1 = Some(now + LOCKOUT);
        }
    }

    pub fn record_success(&mut self, ip: IpAddr) {
        self.entries.remove(&ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_after_five_failures_and_unlocks_later() {
        let ip: IpAddr = "192.168.1.9".parse().unwrap();
        let t0 = Instant::now();
        let mut l = AttemptLimiter::new();
        for _ in 0..4 {
            l.record_failure(ip, t0);
            assert!(l.check(ip, t0).is_ok());
        }
        l.record_failure(ip, t0);
        assert!(l.check(ip, t0).is_err());
        assert!(l.check(ip, t0 + Duration::from_secs(61)).is_ok());
    }

    #[test]
    fn success_clears_failures() {
        let ip: IpAddr = "10.0.0.2".parse().unwrap();
        let t0 = Instant::now();
        let mut l = AttemptLimiter::new();
        for _ in 0..4 {
            l.record_failure(ip, t0);
        }
        l.record_success(ip);
        l.record_failure(ip, t0);
        assert!(l.check(ip, t0).is_ok());
    }
}
