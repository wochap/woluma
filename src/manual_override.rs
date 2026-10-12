//! Per-output pause state used when the user takes over brightness.

use crate::config::ManualOverride;
use std::time::{Duration, Instant, SystemTime};

/// Settle period after the last hook exits before the post-hook snapshot.
pub const HOOK_SETTLE: Duration = Duration::from_secs(1);
/// Longest a hook grace window stays open.
pub const HOOK_CAP: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Manual,
    Requested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Until {
    RuleChange,
    /// Only an explicit resume ends it.
    Resume,
    /// Monotonic deadline plus its wall-clock equivalent for reporting.
    At(Instant, SystemTime),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pause {
    pub reason: Reason,
    pub until: Until,
}

impl Pause {
    /// Starts (or restarts) a pause. Without an explicit duration, a manual-override pause
    /// follows the configured mode and a requested pause lasts until resumed.
    pub fn start(
        reason: Reason,
        duration: Option<Duration>,
        mode: &ManualOverride,
        now: Instant,
    ) -> Self {
        let duration = match reason {
            Reason::Manual => duration.or(match mode {
                ManualOverride::UntilRuleChange => None,
                ManualOverride::For(d) => Some(*d),
            }),
            Reason::Requested => duration,
        };
        let until = match (duration, reason) {
            (Some(d), _) => Until::At(now + d, SystemTime::now() + d),
            (None, Reason::Manual) => Until::RuleChange,
            (None, Reason::Requested) => Until::Resume,
        };
        Self { reason, until }
    }

    pub fn deadline(&self) -> Option<Instant> {
        match self.until {
            Until::At(at, _) => Some(at),
            Until::RuleChange | Until::Resume => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_decides_without_explicit_duration() {
        let now = Instant::now();
        let p = Pause::start(Reason::Manual, None, &ManualOverride::UntilRuleChange, now);
        assert_eq!(p.until, Until::RuleChange);
        let m = ManualOverride::For(Duration::from_secs(900));
        let p = Pause::start(Reason::Manual, None, &m, now);
        assert_eq!(p.deadline(), Some(now + Duration::from_secs(900)));
        let p = Pause::start(
            Reason::Requested,
            Some(Duration::from_secs(600)),
            &ManualOverride::UntilRuleChange,
            now,
        );
        assert_eq!(p.deadline(), Some(now + Duration::from_secs(600)));
        assert_eq!(p.reason, Reason::Requested);
        let p = Pause::start(Reason::Requested, None, &m, now);
        assert_eq!(p.until, Until::Resume);
    }
}
