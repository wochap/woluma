//! Runs the ambient light source on a dedicated blocking thread.

use super::{AlsReading, AlsSource};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub struct Sampler {
    trigger: mpsc::Sender<()>,
    thread: JoinHandle<()>,
}

impl Sampler {
    /// Samples at startup, every `interval`, and on `trigger`, sending each reading to
    /// the returned receiver.
    pub fn spawn(
        mut source: Box<dyn AlsSource>,
        interval: Duration,
    ) -> (Self, async_channel::Receiver<AlsReading>) {
        let (readings, receiver) = async_channel::unbounded();
        let (trigger, triggers) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("woluma-als".into())
            .spawn(move || loop {
                if readings.send_blocking(source.sample()).is_err() {
                    break;
                }
                match triggers.recv_timeout(interval) {
                    Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            })
            .expect("unable to spawn the ALS thread");
        (Self { trigger, thread }, receiver)
    }

    /// Takes a sample now, e.g. when the daemon is re-enabled after a toggle-off.
    #[allow(dead_code)] // used by the control socket
    pub fn trigger(&self) {
        let _ = self.trigger.send(());
    }

    /// Stops sampling, letting an in-flight sample finish (and restore the camera
    /// controls) for up to `timeout`.
    pub async fn shutdown(self, timeout: Duration) {
        drop(self.trigger);
        let deadline = Instant::now() + timeout;
        while !self.thread.is_finished() {
            if Instant::now() >= deadline {
                log::warn!("Ambient light sample still running at shutdown");
                return;
            }
            smol::Timer::after(Duration::from_millis(20)).await;
        }
        let _ = self.thread.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::als::{SourceKind, UnknownReason};
    use crate::config::Hysteresis;
    use crate::rules::{parse_num_cond, Engine, Inputs, Rule, When};
    use chrono::NaiveTime;
    use std::collections::VecDeque;
    use std::sync::Arc;

    struct Scripted(VecDeque<Result<u8, UnknownReason>>);

    impl AlsSource for Scripted {
        fn sample(&mut self) -> AlsReading {
            let next = self.0.pop_front().unwrap_or(Err(UnknownReason::Error));
            AlsReading::from_result(SourceKind::Webcam, next)
        }
    }

    fn rule(name: &str, als: Option<&str>) -> Rule {
        Rule {
            name: name.into(),
            when: When {
                als: als.map(|s| parse_num_cond(s).unwrap()),
                ..When::default()
            },
            brightness: Some(50),
            run: vec![],
        }
    }

    #[test]
    fn readings_drive_als_rules() {
        let source = Scripted(VecDeque::from([
            Ok(10),
            Err(UnknownReason::Placeholder),
            Ok(12),
        ]));
        let (sampler, readings) = Sampler::spawn(Box::new(source), Duration::from_secs(3600));
        let mut engine = Engine::new(
            "eDP-1".into(),
            Arc::new(vec![rule("dim", Some("< 20")), rule("default", None)]),
            Duration::ZERO,
            Hysteresis::default(),
        );
        let mut active = Vec::new();
        smol::block_on(async {
            for _ in 0..3 {
                let reading = readings.recv().await.unwrap();
                engine.step(
                    Inputs {
                        luma: 50,
                        als: reading.value,
                        now: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
                    },
                    Instant::now(),
                );
                active.push((
                    reading.reason,
                    engine.active().map(|i| engine.rules()[i].name.clone()),
                ));
                sampler.trigger();
            }
            sampler.shutdown(Duration::from_secs(5)).await;
        });
        assert_eq!(
            active,
            vec![
                (None, Some("dim".into())),
                (Some(UnknownReason::Placeholder), Some("default".into())),
                (None, Some("dim".into())),
            ]
        );
    }
}
