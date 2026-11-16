/* Copyright 2026 Mission Center Developers
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

/// Darwin CPU tick counters wrap at 32 bits. The first sample is a baseline,
/// not an average since boot. Idle includes neither user nor system ticks.
pub(crate) fn cpu_delta(previous: [u32; 4], current: [u32; 4]) -> Option<(f32, f32)> {
    let delta = std::array::from_fn::<_, 4, _>(|i| current[i].wrapping_sub(previous[i]) as u64);
    let total: u64 = delta.iter().sum();
    (total > 0).then(|| {
        (
            (delta[0] + delta[1] + delta[3]) as f32 * 100. / total as f32,
            delta[1] as f32 * 100. / total as f32,
        )
    })
}

pub(crate) fn rate(previous: u64, current: u64, seconds: f64) -> Option<f64> {
    if seconds <= 0. || !seconds.is_finite() {
        return None;
    }
    Some(current.checked_sub(previous)? as f64 / seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_counts_system_user_nice_and_wraps() {
        assert_eq!(cpu_delta([0; 4], [20, 10, 60, 10]), Some((40., 10.)));
        assert_eq!(cpu_delta([0; 4], [0; 4]), None);
        assert_eq!(
            cpu_delta([u32::MAX, 0, 0, 0], [0, 0, 1, 0]),
            Some((50., 0.))
        );
    }

    #[test]
    fn counter_resets_and_invalid_intervals_are_not_traffic() {
        assert_eq!(rate(10, 20, 2.), Some(5.));
        assert_eq!(rate(20, 10, 1.), None);
        for seconds in [0., -1., f64::NAN, f64::INFINITY] {
            assert_eq!(rate(0, 10, seconds), None);
        }
    }
}
