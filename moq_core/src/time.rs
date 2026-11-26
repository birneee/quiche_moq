//! Time as the caller passes it in: moq_core never reads a clock itself.

use core::ops::{Add, AddAssign, Sub, SubAssign};
use core::time::Duration;

/// A point in time, in microseconds from an origin the caller chooses, only
/// differences matter. The `std` feature adds `Timestamp::now()`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    pub fn saturating_duration_since(self, earlier: Timestamp) -> Duration {
        Duration::from_micros(self.0.saturating_sub(earlier.0).max(0) as u64)
    }
}

impl Add<Duration> for Timestamp {
    type Output = Self;

    fn add(self, duration: Duration) -> Self {
        Self(self.0.saturating_add(micros(duration)))
    }
}

impl Sub<Duration> for Timestamp {
    type Output = Self;

    fn sub(self, duration: Duration) -> Self {
        Self(self.0.saturating_sub(micros(duration)))
    }
}

impl AddAssign<Duration> for Timestamp {
    fn add_assign(&mut self, duration: Duration) {
        *self = *self + duration;
    }
}

impl SubAssign<Duration> for Timestamp {
    fn sub_assign(&mut self, duration: Duration) {
        *self = *self - duration;
    }
}

fn micros(duration: Duration) -> i64 {
    i64::try_from(duration.as_micros()).unwrap_or(i64::MAX)
}

#[cfg(feature = "std")]
mod clock {
    use super::{Timestamp, micros};
    use std::sync::OnceLock;
    use web_time::Instant;

    /// Process-wide, so timestamps of all sessions compare.
    static ORIGIN: OnceLock<Instant> = OnceLock::new();

    impl Timestamp {
        pub fn now() -> Self {
            Instant::now().into()
        }
    }

    impl From<Instant> for Timestamp {
        fn from(instant: Instant) -> Self {
            let origin = *ORIGIN.get_or_init(Instant::now);
            match instant.checked_duration_since(origin) {
                Some(after) => Timestamp(micros(after)),
                None => Timestamp(-micros(origin - instant)),
            }
        }
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use web_time::Instant;

    #[test]
    fn instants_before_and_after_origin_keep_their_distance() {
        let before = Instant::now();
        let origin = Timestamp::now();
        let after = Instant::now() + Duration::from_millis(5);
        assert!(Timestamp::from(before) <= origin);
        assert_eq!(Timestamp::from(after).saturating_duration_since(Timestamp::from(before)).as_millis(), (after - before).as_millis());
    }

    #[test]
    fn arithmetic_saturates() {
        let t = Timestamp::from_micros(i64::MAX - 1);
        assert_eq!(t + Duration::from_secs(1), Timestamp::from_micros(i64::MAX));
        assert_eq!(Timestamp::from_micros(0).saturating_duration_since(t), Duration::ZERO);
    }
}
