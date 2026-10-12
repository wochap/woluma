//! Wires luma, clock and ALS inputs into per-output engines and dispatches hooks.

use crate::brightness::{self, backlight::Backlight, ddc::Ddc, Controller, Device, Msg};
use crate::capture;
use crate::config::{Backend, Config, Output, DEFAULT_POLL};
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

/// Opens the output's device and starts its controller. A missing backlight is fatal; a
/// missing DDC display is retried by the controller.
fn controller(config: &Config, output: &Output, queue: &HookQueue) -> Result<Controller> {
    let (profile, device, connector): (_, Option<Box<dyn Device>>, Option<brightness::Connector>) =
        match &output.backend {
            Backend::Backlight(name) => (
                brightness::Profile::backlight(),
                Some(Box::new(Backlight::open(&output.name, name)?)),
                None,
            ),
            Backend::Ddcutil { display, poll } => {
                let display = display.clone();
                (
                    brightness::Profile::ddc(poll.unwrap_or(DEFAULT_POLL)),
                    None,
                    Some(Arc::new(move || {
                        Ddc::find(&display).map(|d| Box::new(d) as Box<dyn Device>)
                    })),
                )
            }
        };
    Ok(Controller::spawn(
        brightness::Settings {
            output: output.name.clone(),
            enabled: config.general.enabled,
            transition: config.general.transition,
            manual_override: config.general.manual_override.clone(),
            profile,
            timing: brightness::Timing::default(),
        },
        device,
        connector,
        Arc::new(queue.handle()),
    ))
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
    let mut controllers = Vec::new();
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
        let queue = HookQueue::spawn(
            output.name.clone(),
            hooks::DEFAULT_TIMEOUT,
            hooks_shutdown.clone(),
        );
        controllers.push(controller(&config, output, &queue)?);
        queues.push(queue);

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
    if !config.general.enabled {
        log::info!("Daemon is disabled: rules are tracked but brightness and hooks are left alone");
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
    let dispatch = |engine: &Engine, controller: &Controller, transition: Option<Transition>| {
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
        let rule = transition.next.map(|i| &engine.rules()[i]);
        controller.send(Msg::RuleChanged {
            rule: rule.map(|r| r.name.clone()),
            brightness: rule.and_then(|r| r.brightness),
            job: Job::from_transition(&transition, engine.rules()),
        });
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
                dispatch(&engines[index], &controllers[index], transition);
            }
            Event::Als(_) | Event::Tick => {
                if let Event::Als(reading) = event {
                    if reading == als_value {
                        continue;
                    }
                    log::trace!("als {reading:?}");
                    als_value = reading;
                }
                for (engine, controller) in engines.iter_mut().zip(&controllers) {
                    if let Some(last) = engine.last_inputs() {
                        let transition = engine.step(
                            Inputs {
                                als: als_value,
                                now,
                                ..last
                            },
                            Instant::now(),
                        );
                        dispatch(engine, controller, transition);
                    }
                }
            }
            Event::Shutdown => break,
        }
    }

    capture_active.store(false, Ordering::Relaxed);
    hooks_shutdown.store(true, Ordering::Relaxed);
    for controller in controllers {
        controller.close().await;
    }
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
