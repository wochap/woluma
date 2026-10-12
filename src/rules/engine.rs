use super::{Rule, When};
use crate::config::Hysteresis;
use chrono::NaiveTime;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Inputs {
    pub luma: u8,
    pub als: Option<u8>,
    pub now: NaiveTime,
}

/// A change of the active rule. `prev`/`next` index into the output's rules.
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    pub output: String,
    pub prev: Option<usize>,
    pub next: Option<usize>,
    pub inputs: Inputs,
}

/// Per-output active-rule state machine. Pure: time is injected.
#[derive(Debug)]
pub struct Engine {
    output: String,
    rules: Arc<Vec<Rule>>,
    settle: Duration,
    hysteresis: Hysteresis,
    started: bool,
    active: Option<usize>,
    candidate: Option<(Option<usize>, Instant)>,
    /// Inputs and target of the previous step.
    last: Option<(Inputs, Option<usize>)>,
}

impl Engine {
    pub fn new(
        output: String,
        rules: Arc<Vec<Rule>>,
        settle: Duration,
        hysteresis: Hysteresis,
    ) -> Self {
        Self {
            output,
            rules,
            settle,
            hysteresis,
            started: false,
            active: None,
            candidate: None,
            last: None,
        }
    }

    #[allow(dead_code)] // used by tests and the future control socket
    pub fn active(&self) -> Option<usize> {
        self.active
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    pub fn output(&self) -> &str {
        &self.output
    }

    /// Latest inputs seen, if any.
    pub fn last_inputs(&self) -> Option<Inputs> {
        self.last.map(|(inputs, _)| inputs)
    }

    fn matches(&self, when: &When, inputs: &Inputs, widen: bool) -> bool {
        let num = |cond: &Option<super::NumCond>, value: Option<u8>, h: f64| match cond {
            None => true,
            Some(cond) => value.is_some_and(|v| {
                let cond = if widen { cond.widened(h) } else { *cond };
                cond.matches(v as f64)
            }),
        };
        num(&when.luma, Some(inputs.luma), self.hysteresis.luma)
            && num(&when.als, inputs.als, self.hysteresis.als)
            && when.time.is_none_or(|t| t.contains(inputs.now))
    }

    /// First matching rule; the active rule is checked with widened thresholds.
    pub fn evaluate(&self, inputs: &Inputs) -> Option<usize> {
        (0..self.rules.len())
            .find(|&i| self.matches(&self.rules[i].when, inputs, self.active == Some(i)))
    }

    pub fn step(&mut self, inputs: Inputs, now: Instant) -> Option<Transition> {
        let target = self.evaluate(&inputs);
        let previous = self.last.replace((inputs, target));

        if !self.started {
            self.started = true;
            return self.switch(target, inputs);
        }
        if target == self.active {
            self.candidate = None;
            return None;
        }

        // Time-driven: the previous step agreed with the active rule, and only the clock moved.
        let time_driven = previous.is_some_and(|(prev, prev_target)| {
            prev_target == self.active
                && self.evaluate(&Inputs {
                    now: inputs.now,
                    ..prev
                }) == target
        });
        if time_driven || self.settle.is_zero() {
            return self.switch(target, inputs);
        }

        match self.candidate {
            Some((candidate, since)) if candidate == target => {
                if now.saturating_duration_since(since) >= self.settle {
                    return self.switch(target, inputs);
                }
            }
            _ => self.candidate = Some((target, now)),
        }
        None
    }

    fn switch(&mut self, target: Option<usize>, inputs: Inputs) -> Option<Transition> {
        self.candidate = None;
        let prev = std::mem::replace(&mut self.active, target);
        (prev != target).then(|| Transition {
            output: self.output.clone(),
            prev,
            next: target,
            inputs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{parse_num_cond, parse_time_range};

    fn rule(name: &str, luma: Option<&str>, als: Option<&str>, time: Option<&str>) -> Rule {
        Rule {
            name: name.into(),
            when: When {
                luma: luma.map(|s| parse_num_cond(s).unwrap()),
                als: als.map(|s| parse_num_cond(s).unwrap()),
                time: time.map(|s| parse_time_range(s).unwrap()),
            },
            brightness: Some(50),
            run: vec![],
        }
    }

    fn engine(rules: Vec<Rule>) -> Engine {
        Engine::new(
            "eDP-1".into(),
            Arc::new(rules),
            Duration::from_secs(2),
            Hysteresis::default(),
        )
    }

    fn at(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn inputs(luma: u8) -> Inputs {
        Inputs {
            luma,
            als: None,
            now: at(12, 0),
        }
    }

    fn name(e: &Engine) -> Option<&str> {
        e.active().map(|i| e.rules()[i].name.as_str())
    }

    fn standard() -> Engine {
        engine(vec![
            rule("night-bright", Some(">= 70"), None, Some("21:00-07:00")),
            rule("bright", Some(">= 70"), None, None),
            rule("default", None, None, None),
        ])
    }

    /// Feeds `luma` every 500ms for `secs` seconds starting at `t0`, returning the transitions.
    fn feed(e: &mut Engine, t0: Instant, from_ms: u64, secs: u64, luma: u8) -> Vec<Transition> {
        (0..=secs * 2)
            .filter_map(|i| e.step(inputs(luma), t0 + Duration::from_millis(from_ms + i * 500)))
            .collect()
    }

    #[test]
    fn first_match_wins() {
        let mut e = standard();
        let t = e
            .step(
                Inputs {
                    luma: 85,
                    als: None,
                    now: at(22, 0),
                },
                Instant::now(),
            )
            .unwrap();
        assert_eq!(name(&e), Some("night-bright"));
        assert_eq!(t.prev, None);
        assert_eq!(t.next, Some(0));
    }

    #[test]
    fn conjunction_requires_all() {
        let mut e = standard();
        e.step(
            Inputs {
                luma: 50,
                als: None,
                now: at(22, 0),
            },
            Instant::now(),
        );
        assert_eq!(name(&e), Some("default"));
    }

    #[test]
    fn fallback_and_no_match() {
        let mut e = standard();
        e.step(inputs(10), Instant::now());
        assert_eq!(name(&e), Some("default"));

        let mut e = engine(vec![rule("bright", Some(">= 70"), None, None)]);
        assert_eq!(e.step(inputs(20), Instant::now()), None);
        assert_eq!(e.active(), None);
    }

    #[test]
    fn unknown_als_never_matches() {
        let mut e = engine(vec![
            rule("dim", None, Some("< 20"), None),
            rule("default", None, None, None),
        ]);
        e.step(inputs(50), Instant::now());
        assert_eq!(name(&e), Some("default"));

        let mut e = engine(vec![
            rule("ne", Some(">= 0"), Some("!= 50"), None),
            rule("default", None, None, None),
        ]);
        e.step(inputs(50), Instant::now());
        assert_eq!(name(&e), Some("default"));

        let mut e = engine(vec![
            rule("dim", None, Some("< 20"), None),
            rule("default", None, None, None),
        ]);
        e.step(
            Inputs {
                als: Some(10),
                ..inputs(50)
            },
            Instant::now(),
        );
        assert_eq!(name(&e), Some("dim"));
    }

    #[test]
    fn brief_flash_ignored() {
        let mut e = standard();
        let t0 = Instant::now();
        e.step(inputs(88), t0);
        assert_eq!(name(&e), Some("bright"));
        assert!(feed(&mut e, t0, 500, 1, 18).is_empty());
        assert!(feed(&mut e, t0, 2000, 3, 88).is_empty());
        assert_eq!(name(&e), Some("bright"));
    }

    #[test]
    fn sustained_change_applied_after_settle() {
        let mut e = standard();
        let t0 = Instant::now();
        e.step(inputs(20), t0);
        assert_eq!(name(&e), Some("default"));
        // Rises at 1s, so switches at 3s.
        let start = t0 + Duration::from_secs(1);
        assert_eq!(e.step(inputs(88), start), None);
        assert_eq!(
            e.step(inputs(88), start + Duration::from_millis(1500)),
            None
        );
        let t = e.step(inputs(88), start + Duration::from_secs(2)).unwrap();
        assert_eq!((t.prev, t.next), (Some(2), Some(1)));
        assert_eq!(e.step(inputs(88), start + Duration::from_secs(3)), None);
    }

    #[test]
    fn hysteresis_stay_leave_enter() {
        let t0 = Instant::now();
        let mut e = standard();
        e.step(inputs(88), t0);
        assert!(
            feed(&mut e, t0, 500, 5, 67).is_empty(),
            "stays through the dip"
        );
        assert_eq!(name(&e), Some("bright"));

        let t = feed(&mut e, t0, 6000, 3, 64);
        assert_eq!(t.len(), 1, "leaves after the margin");
        assert_eq!(name(&e), Some("default"));

        assert!(
            feed(&mut e, t0, 10_000, 5, 68).is_empty(),
            "entering uses exact threshold"
        );
        assert_eq!(name(&e), Some("default"));
    }

    #[test]
    fn earlier_rule_preempts_widened_active() {
        let mut e = engine(vec![
            rule("dark", Some("< 30"), None, None),
            rule("mid", Some("<= 60"), None, None),
        ]);
        let t0 = Instant::now();
        e.step(inputs(50), t0);
        assert_eq!(name(&e), Some("mid"));
        feed(&mut e, t0, 500, 3, 20);
        assert_eq!(name(&e), Some("dark"));
    }

    #[test]
    fn time_boundary_switches_immediately() {
        let mut e = standard();
        let t0 = Instant::now();
        e.step(
            Inputs {
                luma: 85,
                als: None,
                now: at(20, 59),
            },
            t0,
        );
        assert_eq!(name(&e), Some("bright"));
        let t = e.step(
            Inputs {
                luma: 85,
                als: None,
                now: at(21, 0),
            },
            t0 + Duration::from_millis(500),
        );
        assert!(t.is_some());
        assert_eq!(name(&e), Some("night-bright"));
    }

    #[test]
    fn pending_luma_change_still_needs_settle() {
        let mut e = standard();
        let t0 = Instant::now();
        e.step(inputs(20), t0);
        assert_eq!(e.step(inputs(88), t0 + Duration::from_millis(500)), None);
        assert_eq!(e.step(inputs(88), t0 + Duration::from_millis(1000)), None);
        assert_eq!(name(&e), Some("default"));
    }

    #[test]
    fn stays_active_without_transitions() {
        let mut e = standard();
        let t0 = Instant::now();
        assert!(e.step(inputs(88), t0).is_some());
        assert!(feed(&mut e, t0, 500, 50, 88).is_empty());
    }

    #[test]
    fn transition_to_no_rule() {
        let mut e = engine(vec![rule("bright", Some(">= 70"), None, None)]);
        let t0 = Instant::now();
        e.step(inputs(90), t0);
        let t = feed(&mut e, t0, 500, 3, 10);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].next, None);
        assert_eq!(e.active(), None);
    }
}
