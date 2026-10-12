//! Retargetable ramp: each step covers an even share of the remaining distance over the
//! remaining transition time.

use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Ramp {
    target: u32,
    deadline: Instant,
    interval: Duration,
    next_step: Instant,
}

impl Ramp {
    pub fn new(target: u32, transition: Duration, interval: Duration, now: Instant) -> Self {
        Self {
            target,
            deadline: now + transition,
            interval,
            next_step: now,
        }
    }

    pub fn target(&self) -> u32 {
        self.target
    }

    /// Replaces the target; the ramp continues from wherever the device is now, keeping
    /// the minimum spacing between writes.
    pub fn retarget(&mut self, target: u32, transition: Duration, now: Instant) {
        self.target = target;
        self.deadline = now + transition;
    }

    /// When the next step is due.
    pub fn due(&self) -> Instant {
        self.next_step
    }

    /// The value to write next when the device is at `current`. Equal to the target on the
    /// last step.
    pub fn step(&mut self, current: u32, now: Instant) -> u32 {
        self.next_step = now.max(self.next_step) + self.interval;
        let remaining = self.deadline.saturating_duration_since(now);
        let steps = (remaining.as_nanos() / self.interval.as_nanos().max(1)).max(1) as u32;
        let distance = current.abs_diff(self.target);
        let delta = distance.div_ceil(steps);
        if current < self.target {
            current + delta
        } else {
            current - delta
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEP: Duration = Duration::from_millis(16);

    fn run(ramp: &mut Ramp, mut current: u32, mut now: Instant, writes: &mut Vec<u32>) -> Instant {
        while current != ramp.target() {
            now = now.max(ramp.due());
            current = ramp.step(current, now);
            writes.push(current);
            assert!(writes.len() < 1000);
        }
        now
    }

    #[test]
    fn ramps_monotonically_and_ends_on_target() {
        let start = Instant::now();
        let mut ramp = Ramp::new(102, Duration::from_millis(400), STEP, start);
        let mut writes = Vec::new();
        let end = run(&mut ramp, 179, start, &mut writes);
        assert_eq!(*writes.last().unwrap(), 102);
        assert!(writes.windows(2).all(|w| w[1] < w[0]));
        assert!(writes.len() > 10, "{writes:?}");
        let took = end - start;
        assert!(
            took >= Duration::from_millis(350) && took <= Duration::from_millis(420),
            "{took:?}"
        );
    }

    #[test]
    fn zero_transition_is_a_single_write() {
        let now = Instant::now();
        let mut ramp = Ramp::new(102, Duration::ZERO, STEP, now);
        let mut writes = Vec::new();
        run(&mut ramp, 179, now, &mut writes);
        assert_eq!(writes, vec![102]);
    }

    #[test]
    fn retarget_turns_around_from_current() {
        let start = Instant::now();
        let mut ramp = Ramp::new(102, Duration::from_millis(400), STEP, start);
        let mut current = 179;
        let mut now = start;
        while current > 140 {
            now = ramp.due();
            current = ramp.step(current, now);
        }
        ramp.retarget(179, Duration::from_millis(400), now);
        let mut writes = Vec::new();
        run(&mut ramp, current, now, &mut writes);
        assert!(writes.iter().all(|&w| w > current - 1), "{writes:?}");
        assert_eq!(*writes.last().unwrap(), 179);
    }
}
