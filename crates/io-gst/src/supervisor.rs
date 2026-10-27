//! Reconnection policy (spec `io-gst.md`, « Signal perdu et reconnexion »): a lost pipeline is
//! destroyed, then recreated after 1 s, 2 s, 5 s, then every 10 s, without end.

use std::time::Duration;

/// Delay before attempt `attempt` (1 = first retry after a loss).
pub(crate) fn retry_delay(attempt: u32) -> Duration {
    Duration::from_secs(match attempt {
        0 | 1 => 1,
        2 => 2,
        3 => 5,
        _ => 10,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_1_2_5_then_10_seconds() {
        let delays: Vec<u64> = (1..=6).map(|a| retry_delay(a).as_secs()).collect();
        assert_eq!(delays, [1, 2, 5, 10, 10, 10]);
        assert_eq!(retry_delay(u32::MAX), Duration::from_secs(10));
    }
}
