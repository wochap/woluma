//! Wires luma, clock and ALS inputs into per-output engines and dispatches hooks.

use crate::capture;
use crate::config::Config;
use crate::hooks::{self, HookQueue, Job};
use crate::rules::{Engine, Inputs, Transition};
use anyhow::{Context, Result};
use async_signal::{Signal, Signals};
use chrono::{NaiveTime, Timelike};
use futures_lite::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_TICK: Duration = Duration::from_secs(60);
/// Lands clock ticks just past a boundary rather than on it.
const TICK_SLACK: Duration = Duration::from_millis(50);

enum Event {
    Luma(usize, u8),
    Als(Option<u8>),
    Tick,
    Shutdown,
}

/// Ambient light stub: never yields, so the reading stays unknown.
fn als_stub() -> (
    async_channel::Sender<Option<u8>>,
    async_channel::Receiver<Option<u8>>,
) {
    async_channel::unbounded()
}

/// Delay until the next configured time boundary, at most `MAX_TICK`.
pub fn next_tick_delay(boundaries: &[NaiveTime], now: NaiveTime) -> Duration {
    const DAY: i64 = 24 * 3600 * 1000;
    let now_ms =
        now.num_seconds_from_midnight() as i64 * 1000 + (now.nanosecond() / 1_000_000) as i64;
    boundaries
        .iter()
        .map(|b| {
            let delta = (b.num_seconds_from_midnight() as i64 * 1000 - now_ms).rem_euclid(DAY);
            Duration::from_millis(if delta == 0 { DAY } else { delta } as u64) + TICK_SLACK
        })
        .min()
        .unwrap_or(MAX_TICK)
        .min(MAX_TICK)
}

pub fn run(config: Config) -> Result<()> {
    smol::block_on(run_async(config))
}

async fn run_async(config: Config) -> Result<()> {
    capture::check_protocols().context("unable to start screen capture")?;

    let (events, inbox) = async_channel::unbounded::<Event>();
    let capture_active = Arc::new(AtomicBool::new(true));
    let hooks_shutdown = Arc::new(AtomicBool::new(false));

    let mut engines = Vec::new();
    let mut queues = Vec::new();
    let mut boundaries = Vec::new();
    for (index, output) in config.outputs.iter().enumerate() {
        boundaries.extend(
            output
                .rules
                .iter()
                .filter_map(|r| r.when.time)
                .flat_map(|t| [t.start, t.end]),
        );
        engines.push(Engine::new(
            output.name.clone(),
            Arc::new(output.rules.clone()),
            config.general.settle,
            config.hysteresis,
        ));
        queues.push(HookQueue::spawn(
            output.name.clone(),
            hooks::DEFAULT_TIMEOUT,
            hooks_shutdown.clone(),
        ));

        let stream = capture::luma_stream(output.name.clone(), capture_active.clone());
        let events = events.clone();
        smol::spawn(async move {
            while let Ok(luma) = stream.recv().await {
                if events.send(Event::Luma(index, luma)).await.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    {
        let events = events.clone();
        smol::spawn(async move {
            loop {
                smol::Timer::after(next_tick_delay(&boundaries, chrono::Local::now().time())).await;
                if events.send(Event::Tick).await.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    let (_als_sender, als) = als_stub();
    {
        let events = events.clone();
        smol::spawn(async move {
            while let Ok(reading) = als.recv().await {
                if events.send(Event::Als(reading)).await.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    let mut signals =
        Signals::new([Signal::Int, Signal::Term]).context("unable to install signal handlers")?;
    {
        let events = events.clone();
        smol::spawn(async move {
            if let Some(Ok(signal)) = signals.next().await {
                log::info!("Received {signal:?}, shutting down");
                let _ = events.send(Event::Shutdown).await;
            }
        })
        .detach();
    }

    if config.als != crate::config::Als::None {
        log::info!("Ambient light sources are not supported yet; the ALS reading stays unknown");
    }
    for output in &config.outputs {
        log::debug!(
            "{}: brightness backend {:?} (not written yet)",
            output.name,
            output.backend
        );
    }
    if !config.general.enabled {
        log::info!("Daemon is disabled: rules are tracked but hooks will not run");
    }
    log::info!(
        "woluma started for outputs: {}",
        config
            .outputs
            .iter()
            .map(|o| o.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let mut als_value: Option<u8> = None;
    let dispatch = |engine: &Engine, queue: &HookQueue, transition: Option<Transition>| {
        let Some(transition) = transition else { return };
        let name = |i: Option<usize>| i.map_or("<none>", |i| engine.rules()[i].name.as_str());
        log::info!(
            "{}: {} -> {} (luma {}, als {}, time {})",
            transition.output,
            name(transition.prev),
            name(transition.next),
            transition.inputs.luma,
            transition
                .inputs
                .als
                .map_or("unknown".into(), |a| a.to_string()),
            transition.inputs.now.format("%H:%M"),
        );
        if config.general.enabled {
            if let Some(job) = Job::from_transition(&transition, engine.rules()) {
                queue.enqueue(job);
            }
        }
    };

    while let Ok(event) = inbox.recv().await {
        let now = chrono::Local::now().time();
        match event {
            Event::Luma(index, luma) => {
                log::trace!("{}: luma {luma}", engines[index].output());
                let transition = engines[index].step(
                    Inputs {
                        luma,
                        als: als_value,
                        now,
                    },
                    Instant::now(),
                );
                dispatch(&engines[index], &queues[index], transition);
            }
            Event::Als(_) | Event::Tick => {
                if let Event::Als(reading) = event {
                    if reading == als_value {
                        continue;
                    }
                    log::trace!("als {reading:?}");
                    als_value = reading;
                }
                for (engine, queue) in engines.iter_mut().zip(&queues) {
                    if let Some(last) = engine.last_inputs() {
                        let transition = engine.step(
                            Inputs {
                                als: als_value,
                                now,
                                ..last
                            },
                            Instant::now(),
                        );
                        dispatch(engine, queue, transition);
                    }
                }
            }
            Event::Shutdown => break,
        }
    }

    capture_active.store(false, Ordering::Relaxed);
    hooks_shutdown.store(true, Ordering::Relaxed);
    for queue in queues {
        queue.close().await;
    }
    log::info!("woluma stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: u32, m: u32, s: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, s).unwrap()
    }

    #[test]
    fn tick_waits_for_next_boundary_capped_at_a_minute() {
        assert_eq!(next_tick_delay(&[], t(12, 0, 0)), MAX_TICK);
        assert_eq!(
            next_tick_delay(&[t(21, 0, 0)], t(20, 59, 30)),
            Duration::from_secs(30) + TICK_SLACK
        );
        assert_eq!(next_tick_delay(&[t(21, 0, 0)], t(12, 0, 0)), MAX_TICK);
        assert_eq!(
            next_tick_delay(&[t(0, 0, 0)], t(23, 59, 50)),
            Duration::from_secs(10) + TICK_SLACK
        );
        assert_eq!(next_tick_delay(&[t(21, 0, 0)], t(21, 0, 0)), MAX_TICK);
    }
}
