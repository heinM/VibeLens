//! When to rebuild a catalog whose credentials are about to expire.
//!
//! Glue and S3 Tables get *temporary* AWS credentials (SSO / assume-role) baked into the catalog
//! client and the FileIO props. Both are immutable after construction, so "refresh" means "build a
//! new catalog before the old credentials run out". This module is the pure decision logic with an
//! injectable clock; the plumbing is in [`super::catalog::ManagedCatalog`].

use chrono::{DateTime, Duration, Utc};

/// Rebuild when fewer than this many seconds of validity remain. Matches the 5 minute buffer of
/// `aws::creds::CachingProvider`, so by then the provider is guaranteed to hand out fresh credentials.
pub(crate) const REFRESH_MARGIN_SECS: i64 = 300;

/// After a failed rebuild, wait this long before trying again (the old catalog keeps serving
/// until its credentials really expire).
pub(crate) const RETRY_BACKOFF_SECS: i64 = 20;

/// What the caller should do before using the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Credentials are valid for comfortably longer than the margin (or do not expire).
    Use,
    /// Inside the margin: rebuild now.
    Rebuild,
    /// Inside the margin but a rebuild failed moments ago and the credentials still work: carry on.
    UseStale,
    /// Credentials have expired and a rebuild failed moments ago: report the failure instead of calling AWS.
    Expired,
}

/// Expiry bookkeeping for one built catalog.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Lease {
    /// When the credentials baked into the catalog stop working. `None`: static, never expires.
    pub expires_at: Option<DateTime<Utc>>,
    /// When the last rebuild attempt failed.
    pub last_failure: Option<DateTime<Utc>>,
}

impl Lease {
    pub fn new(expires_at: Option<DateTime<Utc>>) -> Self {
        Self {
            expires_at,
            last_failure: None,
        }
    }

    /// Decides what to do at `now`. Pure.
    pub fn decide(&self, now: DateTime<Utc>) -> Decision {
        let Some(exp) = self.expires_at else {
            return Decision::Use;
        };
        if now + Duration::seconds(REFRESH_MARGIN_SECS) < exp {
            return Decision::Use;
        }
        let backing_off = self
            .last_failure
            .is_some_and(|f| now < f + Duration::seconds(RETRY_BACKOFF_SECS));
        match (backing_off, now < exp) {
            (false, _) => Decision::Rebuild,
            (true, true) => Decision::UseStale,
            (true, false) => Decision::Expired,
        }
    }

    pub fn record_failure(&mut self, now: DateTime<Utc>) {
        self.last_failure = Some(now);
    }
}

/// The earliest of two optional expiries (`None` = never).
pub(crate) fn earliest(
    a: Option<DateTime<Utc>>,
    b: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// A clock the test can move.
    struct FakeClock(Mutex<DateTime<Utc>>);
    impl FakeClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
        fn advance(&self, secs: i64) {
            let mut g = self.0.lock().unwrap();
            *g += Duration::seconds(secs);
        }
    }

    #[test]
    fn static_credentials_never_rebuild() {
        let lease = Lease::new(None);
        assert_eq!(lease.decide(t("2030-01-01T00:00:00Z")), Decision::Use);
    }

    #[test]
    fn rebuilds_when_less_than_five_minutes_remain() {
        let clock = FakeClock(Mutex::new(t("2026-11-01T10:00:00Z")));
        let lease = Lease::new(Some(t("2026-11-01T11:00:00Z")));
        assert_eq!(lease.decide(clock.now()), Decision::Use);
        clock.advance(54 * 60); // 6 minutes left
        assert_eq!(lease.decide(clock.now()), Decision::Use);
        clock.advance(60 + 1); // 4m59s left
        assert_eq!(lease.decide(clock.now()), Decision::Rebuild);
        clock.advance(10 * 60); // already expired
        assert_eq!(lease.decide(clock.now()), Decision::Rebuild);
    }

    #[test]
    fn exactly_five_minutes_left_rebuilds() {
        let lease = Lease::new(Some(t("2026-11-01T11:00:00Z")));
        assert_eq!(lease.decide(t("2026-11-01T10:55:00Z")), Decision::Rebuild);
        assert_eq!(lease.decide(t("2026-11-01T10:54:59Z")), Decision::Use);
    }

    #[test]
    fn a_failed_rebuild_backs_off_and_keeps_serving_until_expiry() {
        let clock = FakeClock(Mutex::new(t("2026-11-01T10:57:00Z")));
        let mut lease = Lease::new(Some(t("2026-11-01T11:00:00Z")));
        assert_eq!(lease.decide(clock.now()), Decision::Rebuild);
        lease.record_failure(clock.now());
        assert_eq!(lease.decide(clock.now()), Decision::UseStale);
        clock.advance(RETRY_BACKOFF_SECS - 1);
        assert_eq!(lease.decide(clock.now()), Decision::UseStale);
        clock.advance(2);
        assert_eq!(
            lease.decide(clock.now()),
            Decision::Rebuild,
            "backoff elapsed: try again"
        );
    }

    #[test]
    fn expired_credentials_during_backoff_are_reported() {
        let mut lease = Lease::new(Some(t("2026-11-01T11:00:00Z")));
        let now = t("2026-11-01T11:00:05Z");
        lease.record_failure(now);
        assert_eq!(lease.decide(now + Duration::seconds(1)), Decision::Expired);
    }

    #[test]
    fn earliest_expiry_wins_and_none_means_never() {
        let a = Some(t("2026-11-01T10:00:00Z"));
        let b = Some(t("2026-11-01T09:00:00Z"));
        assert_eq!(earliest(a, b), b);
        assert_eq!(earliest(a, None), a);
        assert_eq!(earliest(None, b), b);
        assert_eq!(earliest(None, None), None);
    }
}
