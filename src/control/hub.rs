//! Current daemon state plus latest-value fan-out to subscribers.

use super::{AlsSnapshot, OutputSnapshot, PauseReason, Snapshot, State, UNTIL_RULE_CHANGE};
use crate::brightness::Status;
use crate::manual_override::Reason;
use chrono::{DateTime, Local};
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone)]
struct Output {
    name: String,
    enabled: bool,
    luma: Option<u8>,
    rule: Option<String>,
    settle: Option<Duration>,
    /// Latest controller status; `None` before the first one and in observe mode.
    controller: Option<Status>,
}

/// A subscriber's channel, plus our own receiver clone to drop a stale queued value.
type Subscriber = (
    async_channel::Sender<Snapshot>,
    async_channel::Receiver<Snapshot>,
);

struct Inner {
    outputs: Vec<Output>,
    als: AlsSnapshot,
    subscribers: Vec<Subscriber>,
}

/// Holds the state the rule, brightness and ALS tasks report. Every change publishes a new
/// snapshot; a subscriber that isn't reading only ever has the newest one queued.
pub struct Hub {
    inner: Mutex<Inner>,
}

impl Hub {
    pub fn new(names: &[String], enabled: bool, als_source: &str) -> Self {
        Self {
            inner: Mutex::new(Inner {
                outputs: names
                    .iter()
                    .map(|name| Output {
                        name: name.clone(),
                        enabled,
                        luma: None,
                        rule: None,
                        settle: None,
                        controller: None,
                    })
                    .collect(),
                als: AlsSnapshot {
                    source: als_source.into(),
                    value: None,
                },
                subscribers: Vec::new(),
            }),
        }
    }

    pub fn names(&self) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        inner.outputs.iter().map(|o| o.name.clone()).collect()
    }

    /// Records a luma sample with the rule engine's view of it.
    pub fn set_input(
        &self,
        index: usize,
        luma: Option<u8>,
        rule: Option<String>,
        settle: Option<Duration>,
    ) {
        self.update(|inner| {
            let output = &mut inner.outputs[index];
            output.luma = luma.or(output.luma);
            output.rule = rule;
            output.settle = settle;
        });
    }

    pub fn set_als(&self, value: Option<u8>) {
        self.update(|inner| inner.als.value = value);
    }

    pub fn set_controller(&self, index: usize, status: Status) {
        self.update(|inner| {
            let output = &mut inner.outputs[index];
            output.enabled = status.enabled;
            output.controller = Some(status);
        });
    }

    pub fn snapshot(&self) -> Snapshot {
        build(&self.inner.lock().unwrap(), Local::now())
    }

    /// A latest-value stream; it starts with the current snapshot.
    pub fn subscribe(&self) -> async_channel::Receiver<Snapshot> {
        let mut inner = self.inner.lock().unwrap();
        let (sender, receiver) = async_channel::bounded(1);
        let _ = sender.try_send(build(&inner, Local::now()));
        inner.subscribers.push((sender, receiver.clone()));
        receiver
    }

    fn update(&self, f: impl FnOnce(&mut Inner)) {
        let mut inner = self.inner.lock().unwrap();
        f(&mut inner);
        let snapshot = build(&inner, Local::now());
        // Only our own receiver clone left means the subscriber is gone.
        inner.subscribers.retain(|(sender, own)| {
            if sender.receiver_count() <= 1 {
                return false;
            }
            while let Err(async_channel::TrySendError::Full(_)) = sender.try_send(snapshot.clone())
            {
                let _ = own.try_recv();
            }
            true
        });
    }
}

fn build(inner: &Inner, now: DateTime<Local>) -> Snapshot {
    Snapshot {
        version: env!("CARGO_PKG_VERSION").into(),
        time: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        als: inner.als.clone(),
        outputs: inner.outputs.iter().map(output).collect(),
    }
}

fn output(o: &Output) -> OutputSnapshot {
    let status = o.controller.as_ref();
    let paused = status.is_some_and(|s| s.paused);
    let state = if !o.enabled {
        State::Disabled
    } else if paused {
        State::Paused
    } else if o.settle.is_some() {
        State::Settling
    } else {
        State::Active
    };
    let until = status.filter(|s| s.paused).and_then(|s| match s.until {
        Some(at) => {
            Some(DateTime::<Local>::from(at).to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
        }
        None if s.until_rule_change => Some(UNTIL_RULE_CHANGE.into()),
        None => None,
    });
    OutputSnapshot {
        name: o.name.clone(),
        enabled: o.enabled,
        luma: o.luma,
        rule: o.rule.clone(),
        brightness: status.and_then(|s| s.current),
        state,
        pause_reason: status.and_then(|s| s.reason).map(|r| match r {
            Reason::Requested => PauseReason::Manual,
            Reason::Manual => PauseReason::ManualOverride,
        }),
        until,
        settle_remaining_ms: (state == State::Settling)
            .then(|| o.settle.map(|d| d.as_millis() as u64))
            .flatten(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub() -> Hub {
        Hub::new(&["eDP-1".into(), "DP-1".into()], true, "none")
    }

    #[test]
    fn slow_subscriber_gets_only_latest() {
        let hub = hub();
        let slow = hub.subscribe();
        let fast = hub.subscribe();
        assert_eq!(fast.try_recv().unwrap().outputs[0].luma, None);
        // Publishing never blocks, however many updates the slow one misses.
        for luma in 0..100 {
            hub.set_input(0, Some(luma), None, None);
        }
        assert_eq!(slow.len(), 1);
        assert_eq!(slow.try_recv().unwrap().outputs[0].luma, Some(99));
        assert!(slow.try_recv().is_err());
        assert_eq!(fast.try_recv().unwrap().outputs[0].luma, Some(99));
    }

    #[test]
    fn dropped_subscribers_are_forgotten() {
        let hub = hub();
        drop(hub.subscribe());
        hub.set_als(Some(3));
        assert!(hub.inner.lock().unwrap().subscribers.is_empty());
        assert_eq!(hub.snapshot().als.value, Some(3));
    }

    fn status(enabled: bool, paused: bool) -> Status {
        Status {
            output: "eDP-1".into(),
            enabled,
            current: Some(40),
            target: None,
            transitioning: false,
            rule: Some("bright".into()),
            paused,
            reason: paused.then_some(Reason::Manual),
            until: None,
            until_rule_change: paused,
        }
    }

    #[test]
    fn derives_states() {
        let hub = hub();
        hub.set_input(
            0,
            Some(88),
            Some("bright".into()),
            Some(Duration::from_millis(1200)),
        );
        let o = &hub.snapshot().outputs[0];
        assert_eq!(
            (o.state, o.settle_remaining_ms),
            (State::Settling, Some(1200))
        );
        assert_eq!(o.brightness, None);

        hub.set_controller(0, status(true, true));
        let o = &hub.snapshot().outputs[0];
        assert_eq!(o.state, State::Paused);
        assert_eq!(o.pause_reason, Some(PauseReason::ManualOverride));
        assert_eq!(o.until.as_deref(), Some(UNTIL_RULE_CHANGE));
        assert_eq!(o.settle_remaining_ms, None);

        hub.set_controller(0, status(false, false));
        let o = &hub.snapshot().outputs[0];
        assert_eq!(
            (o.state, o.enabled, o.brightness),
            (State::Disabled, false, Some(40))
        );
    }
}
