//! `web-time` without JavaScript.
//!
//! The real crate exists for one reason: `std::time::Instant` panics on
//! `wasm32-unknown-unknown`, so it reaches for `Date.now()` and
//! `performance.now()` through `wasm-bindgen`. A module that imports nothing
//! cannot do that — and the browser viewer refuses to instantiate a module
//! that does.
//!
//! So in the browser this is a *logical* clock: every `Instant::now()` costs a
//! millisecond of logical time, counted in an atomic. It is monotonic and
//! cheap and it is not a wall clock. Everything that reads it here is
//! bookkeeping — which cache entry was used when, how long a computation took
//! — so the shape of the number is enough; the truth of it was never available
//! to us without an import.
//!
//! On every other target this is exactly `std::time`, so native builds behave
//! as if the patch were not there.

#[cfg(not(all(
    any(target_arch = "wasm32", target_arch = "wasm64"),
    target_os = "unknown"
)))]
mod native {
    pub use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
}

#[cfg(not(all(
    any(target_arch = "wasm32", target_arch = "wasm64"),
    target_os = "unknown"
)))]
pub use native::*;

#[cfg(all(
    any(target_arch = "wasm32", target_arch = "wasm64"),
    target_os = "unknown"
))]
mod logical {
    use core::ops::{Add, AddAssign, Sub};
    use core::sync::atomic::{AtomicU64, Ordering};

    pub use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// What one call to [`Instant::now`] advances the clock by.
    const NANOS_PER_TICK: u64 = 1_000_000;

    static TICKS: AtomicU64 = AtomicU64::new(0);

    /// A point on the logical clock: nanoseconds since the module started.
    #[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct Instant(Duration);

    impl Instant {
        pub fn now() -> Instant {
            Instant(Duration::from_nanos(
                TICKS.fetch_add(1, Ordering::Relaxed) * NANOS_PER_TICK,
            ))
        }

        pub fn elapsed(&self) -> Duration {
            Instant::now().duration_since(*self)
        }

        pub fn duration_since(&self, earlier: Instant) -> Duration {
            self.0
                .checked_sub(earlier.0)
                .expect("the logical clock is monotonic")
        }

        pub fn checked_duration_since(&self, earlier: Instant) -> Option<Duration> {
            self.0.checked_sub(earlier.0)
        }

        pub fn saturating_duration_since(&self, earlier: Instant) -> Duration {
            self.0.saturating_sub(earlier.0)
        }

        pub fn checked_add(&self, duration: Duration) -> Option<Instant> {
            self.0.checked_add(duration).map(Instant)
        }

        pub fn checked_sub(&self, duration: Duration) -> Option<Instant> {
            self.0.checked_sub(duration).map(Instant)
        }
    }

    impl Add<Duration> for Instant {
        type Output = Instant;
        fn add(self, other: Duration) -> Instant {
            Instant(self.0 + other)
        }
    }

    impl AddAssign<Duration> for Instant {
        fn add_assign(&mut self, other: Duration) {
            self.0 += other;
        }
    }

    impl Sub<Duration> for Instant {
        type Output = Instant;
        fn sub(self, other: Duration) -> Instant {
            Instant(self.0 - other)
        }
    }

    impl Sub<Instant> for Instant {
        type Output = Duration;
        fn sub(self, other: Instant) -> Duration {
            self.duration_since(other)
        }
    }
}

#[cfg(all(
    any(target_arch = "wasm32", target_arch = "wasm64"),
    target_os = "unknown"
))]
pub use logical::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clock_only_moves_forward() {
        let a = Instant::now();
        let b = Instant::now();
        assert!(b >= a);
        assert!(b.duration_since(a) <= Duration::from_secs(1));
        assert!(a.elapsed() >= Duration::ZERO);
    }

    #[test]
    fn adding_a_duration_moves_it_on() {
        let a = Instant::now();
        let later = a + Duration::from_secs(30);
        assert!(later > a);
        assert_eq!(later - a, Duration::from_secs(30));
    }
}
