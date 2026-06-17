use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};

const MINIMUM_DELAY: u32 = 20;
const FATIGUE_THRESHOLD: u32 = 12;

// ── Delay correlation model ─────────────────────────────────────────
// Discretizes delay values into NUM_STATES buckets and uses a first-order
// Markov chain to create correlation between consecutive delays.
// This keeps timing variation from becoming fully independent white noise,
// while staying inside the configured bounds.

const NUM_STATES: usize = 8;

struct MarkovChain {
    current: usize,
    transitions: [[f64; NUM_STATES]; NUM_STATES],
}

impl Default for MarkovChain {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkovChain {
    fn new() -> Self {
        let mut transitions = [[0.0; NUM_STATES]; NUM_STATES];
        for (i, row) in transitions.iter_mut().enumerate() {
            for (j, probability) in row.iter_mut().enumerate() {
                let dist = (i as f64 - j as f64).abs();
                // Exponential decay: staying near current state is more probable
                // with occasional jumps to distant states
                *probability = (-dist * 0.6).exp();
            }
            // Normalize row to sum to 1.0
            let sum: f64 = row.iter().sum();
            for probability in row {
                *probability /= sum;
            }
        }
        Self {
            current: NUM_STATES / 2,
            transitions,
        }
    }

    fn next(&mut self) -> usize {
        let r = fastrand::f64();
        let mut cum = 0.0;
        for j in 0..NUM_STATES {
            cum += self.transitions[self.current][j];
            if r < cum {
                self.current = j;
                return j;
            }
        }
        self.current = NUM_STATES - 1;
        self.current
    }

    /// Returns a bias factor in [-0.15, 0.15] based on the Markov state.
    /// This creates correlation between consecutive delays.
    fn bias(&mut self) -> f64 {
        let state = self.next();
        (state as f64 / (NUM_STATES - 1) as f64 - 0.5) * 0.3
    }
}

#[derive(Default)]
struct DelayState {
    last_delay: u32,
    consecutive_count: u8,
    keystrokes_since_pause: u32,
    tempo_factor: f64,
}

impl DelayState {
    fn with_defaults() -> Self {
        Self {
            tempo_factor: 1.0,
            ..Self::default()
        }
    }
}

#[derive(Default)]
struct TimingState {
    delay_states: HashMap<usize, DelayState>,
    spare_gaussian: Option<f64>,
    last_press_duration: u32,
    markov: MarkovChain,
}

static STATE: Lazy<Mutex<TimingState>> = Lazy::new(|| Mutex::new(TimingState::default()));
static VARIABILITY_LEVEL: AtomicU8 = AtomicU8::new(1);

#[cfg(test)]
pub(crate) static TEST_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

pub fn init() {
    fastrand::seed(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0),
    );
}

pub fn reset() {
    *STATE.lock() = TimingState::default();
}

pub fn set_timing_variation_level(level: u8) {
    VARIABILITY_LEVEL.store(level.min(2), Ordering::Release);
}

pub fn next_delay(base_delay: u32, random_range: u32, profile_id: usize) -> u32 {
    let level = VARIABILITY_LEVEL.load(Ordering::Acquire);
    let mut timing = STATE.lock();
    let min_delay = minimum_delay(base_delay, random_range);
    let max_delay = base_delay.saturating_add(random_range).max(min_delay);

    let mut delay = if random_range == 0 {
        base_delay
    } else {
        let sigma = f64::from(random_range) / 3.0;
        let jitter = (next_gaussian(&mut timing) * sigma.max(1.0))
            .round()
            .clamp(-f64::from(random_range), f64::from(random_range)) as i64;
        (i64::from(base_delay) + jitter).clamp(i64::from(min_delay), i64::from(max_delay)) as u32
    };
    delay = delay.clamp(min_delay, max_delay);

    // Apply Markov chain bias for delay correlation (level >= 1)
    if level >= 1 && random_range > 0 {
        let markov_bias = timing.markov.bias();
        let biased = f64::from(delay) * (1.0 + markov_bias);
        delay = biased
            .round()
            .clamp(f64::from(min_delay), f64::from(max_delay)) as u32;
    }

    let tempo_delta = (level >= 2).then(|| next_gaussian(&mut timing) * 0.006);
    let state = timing
        .delay_states
        .entry(profile_id)
        .or_insert_with(DelayState::with_defaults);

    if level >= 1 {
        delay = apply_repeat_variation(delay, state, min_delay, max_delay);
    }

    if let Some(tempo_delta) = tempo_delta {
        state.tempo_factor = (state.tempo_factor + tempo_delta).clamp(0.92, 1.08);
        delay = (f64::from(delay) * state.tempo_factor)
            .round()
            .clamp(f64::from(min_delay), f64::from(max_delay)) as u32;

        state.keystrokes_since_pause = state.keystrokes_since_pause.saturating_add(1);
        let pause_probability =
            (f64::from(state.keystrokes_since_pause) / f64::from(FATIGUE_THRESHOLD) * 0.06)
                .min(0.18);
        if fastrand::f64() < pause_probability {
            let fatigue_factor = (state.keystrokes_since_pause / 5).min(4);
            delay = delay.saturating_add(fastrand::u32(35..(75 + fatigue_factor * 25)));
            state.keystrokes_since_pause = 0;
        }
    }

    state.last_delay = delay;
    delay
}

pub fn next_pre_press_delay(profile_id: usize) -> u32 {
    if VARIABILITY_LEVEL.load(Ordering::Acquire) <= 1 {
        return 0;
    }

    let mut timing = STATE.lock();
    timing
        .delay_states
        .entry(profile_id)
        .or_insert_with(DelayState::with_defaults);
    if fastrand::f64() < 0.025 {
        fastrand::u32(8..32)
    } else if fastrand::f64() < 0.004 {
        fastrand::u32(80..180)
    } else {
        0
    }
}

pub fn next_press_duration() -> u32 {
    let level = VARIABILITY_LEVEL.load(Ordering::Acquire);
    if level == 0 {
        return 45;
    }

    let mut timing = STATE.lock();
    let gaussian = next_gaussian(&mut timing).abs();
    let mut duration = if fastrand::f64() < 0.85 {
        35 + (gaussian * 12.0) as u32
    } else {
        80 + (gaussian * 30.0) as u32
    };
    duration = duration.clamp(20, 200);

    if duration == timing.last_press_duration {
        let adjustment = fastrand::u32(2..8) as i32 * if fastrand::bool() { 1 } else { -1 };
        duration = (duration as i32 + adjustment).clamp(20, 200) as u32;
    }
    timing.last_press_duration = duration;
    duration
}

fn minimum_delay(base_delay: u32, random_range: u32) -> u32 {
    if random_range > 0 {
        return base_delay.saturating_sub(random_range).max(MINIMUM_DELAY);
    }
    if base_delay <= MINIMUM_DELAY {
        MINIMUM_DELAY
    } else if base_delay < 1000 {
        (base_delay / 2).max(MINIMUM_DELAY)
    } else {
        MINIMUM_DELAY
    }
}

fn apply_repeat_variation(
    mut delay: u32,
    state: &mut DelayState,
    min_delay: u32,
    max_delay: u32,
) -> u32 {
    if state.last_delay == 0 || min_delay >= max_delay {
        return delay;
    }

    if delay.abs_diff(state.last_delay) <= 2 {
        delay = nudge_away(delay, state.last_delay, min_delay, max_delay, 2, 18);
    }

    if delay.abs_diff(state.last_delay) < 8 {
        state.consecutive_count = state.consecutive_count.saturating_add(1);
    } else {
        state.consecutive_count = 0;
    }

    if state.consecutive_count >= 3 {
        delay = nudge_away(delay, state.last_delay, min_delay, max_delay, 15, 40);
        state.consecutive_count = 0;
    }
    delay
}

fn nudge_away(
    delay: u32,
    last_delay: u32,
    min_delay: u32,
    max_delay: u32,
    min_step: u32,
    max_step: u32,
) -> u32 {
    let step = fastrand::u32(min_step..=max_step);
    let up_room = max_delay.saturating_sub(delay);
    let down_room = delay.saturating_sub(min_delay);

    if delay >= last_delay && up_room > 0 {
        return delay.saturating_add(step.min(up_room));
    }
    if delay < last_delay && down_room > 0 {
        return delay.saturating_sub(step.min(down_room));
    }
    if up_room >= down_room && up_room > 0 {
        return delay.saturating_add(step.min(up_room));
    }
    if down_room > 0 {
        return delay.saturating_sub(step.min(down_room));
    }
    delay
}

fn next_gaussian(state: &mut TimingState) -> f64 {
    if let Some(spare) = state.spare_gaussian.take() {
        return spare;
    }

    loop {
        let u = fastrand::f64() * 2.0 - 1.0;
        let v = fastrand::f64() * 2.0 - 1.0;
        let square_sum = u * u + v * v;
        if !(0.0..1.0).contains(&square_sum) || square_sum == 0.0 {
            continue;
        }

        let factor = (-2.0 * square_sum.ln() / square_sum).sqrt();
        state.spare_gaussian = Some(v * factor);
        return u * factor;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_stays_in_configured_bounds_without_extra_pauses() {
        let _guard = TEST_LOCK.lock();
        reset();
        set_timing_variation_level(1);
        for _ in 0..500 {
            let delay = next_delay(1000, 200, 0);
            assert!((800..=1200).contains(&delay));
        }
    }

    #[test]
    fn level_zero_keeps_configured_random_range() {
        let _guard = TEST_LOCK.lock();
        reset();
        set_timing_variation_level(0);
        let mut changed = false;
        for _ in 0..200 {
            let delay = next_delay(500, 200, 0);
            assert!((300..=700).contains(&delay));
            changed |= delay != 500;
        }
        assert!(changed);
        assert_eq!(next_press_duration(), 45);
        assert_eq!(next_pre_press_delay(0), 0);
    }

    #[test]
    fn level_two_short_pauses_remain_bounded() {
        let _guard = TEST_LOCK.lock();
        reset();
        set_timing_variation_level(2);
        for _ in 0..2000 {
            let delay = next_delay(1000, 200, 0);
            assert!((800..=1374).contains(&delay));
        }
    }

    #[test]
    fn markov_chain_produces_correlated_delays() {
        let _guard = TEST_LOCK.lock();
        reset();
        set_timing_variation_level(1);

        // Collect a sequence of delays and check for correlation
        let delays: Vec<u32> = (0..200).map(|_| next_delay(1000, 200, 0)).collect();

        // Count consecutive pairs where both are above or both are below median
        let median = 1000u32;
        let mut correlated = 0usize;
        let mut total = 0usize;
        for window in delays.windows(2) {
            total += 1;
            if (window[0] >= median) == (window[1] >= median) {
                correlated += 1;
            }
        }

        // Correlated timing should stay above pure independent jitter.
        let correlation_ratio = correlated as f64 / total as f64;
        assert!(
            correlation_ratio > 0.50,
            "delay correlation ratio should be > 0.50, got {correlation_ratio:.2}"
        );
    }
}
