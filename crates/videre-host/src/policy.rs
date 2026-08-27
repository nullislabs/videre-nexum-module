//! Venue-routing policy types videre owns.
//!
//! These lived in `nexum_runtime::engine_config` until the runtime retired
//! `[limits.quota]` and `[limits.watch]`: both sections described venue
//! semantics, not runtime semantics, so they belong at this layer. A
//! composition root builds them and hands them to
//! [`VenueRegistryBuilder`](crate::VenueRegistryBuilder).
//!
//! [`Liveness`] replaces the runtime's actor-liveness flag, which left with
//! the extension-installed component path. A native adapter marks itself
//! dead when it can no longer serve, and the registry answers `unavailable`
//! for a dead venue rather than `unknown-venue`.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Default per-caller submission budget within [`DEFAULT_QUOTA_WINDOW`].
pub const DEFAULT_QUOTA_MAX_CHARGES: u32 = 256;
/// Default sliding window the per-caller submission budget is counted over.
pub const DEFAULT_QUOTA_WINDOW: Duration = Duration::from_secs(60);
/// Default cap on receipts under status watch at once.
pub const DEFAULT_WATCH_MAX_ENTRIES: usize = 1024;
/// Default base window a healthy venue refreshes within; the give-up
/// deadline is the derived `grace`, not this directly.
pub const DEFAULT_WATCH_EXPIRY: Duration = Duration::from_secs(86_400);
/// Derived grace defaults to this many `expiry` windows.
pub const WATCH_GRACE_MULTIPLIER: u64 = 2;
/// Ceiling on the derived grace window.
pub const WATCH_GRACE_MAX: Duration = Duration::from_secs(86_400);

/// Give-up window derived from `expiry`: `min(MULTIPLIER * expiry, MAX)`.
const fn derive_grace(expiry: Duration) -> Duration {
    let scaled = expiry.as_secs().saturating_mul(WATCH_GRACE_MULTIPLIER);
    let capped = if scaled < WATCH_GRACE_MAX.as_secs() {
        scaled
    } else {
        WATCH_GRACE_MAX.as_secs()
    };
    Duration::from_secs(capped)
}

/// Per-caller submission quota toward venues. A submission and a charged
/// decode failure each consume one unit; the window slides.
#[derive(Debug, Clone, Copy)]
pub struct SubmitQuota {
    /// Maximum charges a single caller may accrue within `window`.
    pub max_charges: u32,
    /// Sliding window the charges are counted across.
    pub window: Duration,
}

impl SubmitQuota {
    /// Budget paired with the window it is counted over.
    #[must_use]
    pub const fn new(max_charges: u32, window: Duration) -> Self {
        Self {
            max_charges,
            window,
        }
    }
}

impl Default for SubmitQuota {
    fn default() -> Self {
        Self::new(DEFAULT_QUOTA_MAX_CHARGES, DEFAULT_QUOTA_WINDOW)
    }
}

/// Bounds on a venue status-watch set: `max_entries` caps the per-cadence
/// poll fan-out, `grace` is the give-up deadline, `expiry` the base window
/// it derives from.
#[derive(Debug, Clone, Copy)]
pub struct WatchLimit {
    /// Maximum receipts under status watch at once.
    pub max_entries: usize,
    /// Base window a healthy venue refreshes the deadline within.
    pub expiry: Duration,
    /// Give-up deadline: how long a watch survives an unreachable venue
    /// before unreported eviction. A reachable poll resets it; a resolve
    /// failure or errored poll rides out against it. Derived unless set.
    pub grace: Duration,
}

impl WatchLimit {
    /// Pair a cap with the base expiry; `grace` derives from `expiry`.
    #[must_use]
    pub const fn new(max_entries: usize, expiry: Duration) -> Self {
        Self::with_grace(max_entries, expiry, derive_grace(expiry))
    }

    /// As [`new`](Self::new) but with an explicit `grace`.
    #[must_use]
    pub const fn with_grace(max_entries: usize, expiry: Duration, grace: Duration) -> Self {
        Self {
            max_entries,
            expiry,
            grace,
        }
    }
}

impl Default for WatchLimit {
    fn default() -> Self {
        Self::new(DEFAULT_WATCH_MAX_ENTRIES, DEFAULT_WATCH_EXPIRY)
    }
}

/// Whether a registered venue is currently callable, shared with whoever
/// registered it. Cheap to clone; every clone observes the same state.
#[derive(Clone, Default)]
pub struct Liveness(Arc<Mutex<Option<Instant>>>);

impl Liveness {
    /// A live flag.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the venue is currently callable.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.lock().is_none()
    }

    /// When the venue died, while it is dead.
    #[must_use]
    pub fn dead_since(&self) -> Option<Instant> {
        *self.lock()
    }

    /// Mark dead, keeping the first death instant if already dead.
    pub fn mark_dead(&self) {
        let mut died_at = self.lock();
        if died_at.is_none() {
            *died_at = Some(Instant::now());
        }
    }

    /// Mark the venue alive again after a recovery.
    pub fn mark_alive(&self) {
        *self.lock() = None;
    }

    /// The flag, recovered from a poisoned lock.
    fn lock(&self) -> MutexGuard<'_, Option<Instant>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl std::fmt::Debug for Liveness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Liveness")
            .field("alive", &self.is_alive())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{Liveness, SubmitQuota, WATCH_GRACE_MAX, WatchLimit};
    use std::time::Duration;

    #[test]
    fn a_fresh_flag_is_alive() {
        let liveness = Liveness::new();
        assert!(liveness.is_alive());
        assert!(liveness.dead_since().is_none());
    }

    #[test]
    fn marking_dead_is_idempotent_and_reversible() {
        let liveness = Liveness::new();
        liveness.mark_dead();
        let first = liveness.dead_since().expect("dead since the first mark");
        liveness.mark_dead();
        assert_eq!(liveness.dead_since(), Some(first));
        assert!(!liveness.is_alive());
        liveness.mark_alive();
        assert!(liveness.is_alive());
    }

    #[test]
    fn clones_share_one_flag() {
        let liveness = Liveness::new();
        let clone = liveness.clone();
        clone.mark_dead();
        assert!(!liveness.is_alive());
    }

    #[test]
    fn derived_grace_is_two_expiries_capped_at_the_ceiling() {
        let short = WatchLimit::new(8, Duration::from_secs(30));
        assert_eq!(short.grace, Duration::from_secs(60));
        let long = WatchLimit::new(8, Duration::from_secs(86_400));
        assert_eq!(long.grace, WATCH_GRACE_MAX);
    }

    #[test]
    fn defaults_match_the_documented_constants() {
        assert_eq!(SubmitQuota::default().max_charges, 256);
        assert_eq!(SubmitQuota::default().window, Duration::from_secs(60));
        assert_eq!(WatchLimit::default().max_entries, 1024);
    }
}
