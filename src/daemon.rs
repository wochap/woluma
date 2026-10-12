//! Wires luma, clock and ALS inputs into per-output engines, dispatches hooks and serves the
//! control socket. In observe mode (standalone `watch`) only the inputs and rule matching
//! run: no brightness writes, hooks, socket or instance lock.

use crate::als::{self, AlsReading, Sampler};
use crate::brightness::{
    self, backlight::Backlight, ddc::Ddc, Controller, Device, Msg, RuleUpdate,
};
use crate::capture;
use crate::config::{Als, Backend, Config, Output, DEFAULT_ALS_INTERVAL, DEFAULT_POLL};
use crate::control::server::{self, Server};
use crate::control::{Hub, Request, Response, Target};
use crate::hooks::{self, HookQueue, Job};
use crate::rules::{Engine, Inputs};
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
/// How long shutdown waits for an in-flight ambient sample to restore the camera.
const ALS_SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

enum Event {
    Luma(usize, u8),
    Als(AlsReading),
    Tick,
    Control(Request, async_channel::Sender<Response>),
    Shutdown,
}

/// Steps the engine, publishes its view, and hands a rule change to the controller. In
/// observe mode there is no controller, so nothing is written and no hooks run.
fn advance(
    engine: &mut Engine,
    index: usize,
    inputs: Inputs,
    new_luma: bool,
    hub: &Hub,
    controller: Option<&Controller>,
) {
    let now = Instant::now();
    let transition = engine.step(inputs, now);
    hub.set_input(
        index,
        new_luma.then_some(inputs.luma),
        rule_name(engine),
        engine.settle_remaining(now),
    );
    let (Some(transition), Some(controller)) = (transition, controller) else {
        return;
    };
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
}

pub fn als_source(als: &Als) -> &'static str {
    match als {
        Als::None => "none",
        Als::Webcam { .. } => "webcam",
        Als::Iio { .. } => "iio",
    }
}

pub fn hub(config: &Config) -> Arc<Hub> {
    let names: Vec<_> = config.outputs.iter().map(|o| o.name.clone()).collect();
    Arc::new(Hub::new(
        &names,
        config.general.enabled,
        als_source(&config.als),
    ))
}

fn rule_name(engine: &Engine) -> Option<String> {
    engine.active().map(|i| engine.rules()[i].name.clone())
}

/// Re-evaluates now, skipping settle, and returns the activation for the controller.
fn force(engine: &mut Engine, index: usize, hub: &Hub) -> Option<RuleUpdate> {
    let transition = engine.force()?;
    hub.set_input(index, None, rule_name(engine), None);
    let rule = transition.next.map(|i| &engine.rules()[i]);
    Some(RuleUpdate {
        rule: rule.map(|r| r.name.clone()),
        brightness: rule.and_then(|r| r.brightness),
        job: Job::from_transition(&transition, engine.rules()),
    })
}

/// Applies a toggle/pause/resume and reports the resulting state of the targeted outputs.
async fn control(
    request: Request,
    engines: &mut [Engine],
    controllers: &[Controller],
    hub: &Hub,
) -> Response {
    let target = match &request {
        Request::Toggle { target } | Request::Pause { target, .. } | Request::Resume { target } => {
            target
        }
        Request::Status | Request::Subscribe => return Response::status(hub.snapshot()),
    };
    let target = Target::parse(target);
    let indices = match target.resolve(&hub.names()) {
        Ok(indices) => indices,
        Err(error) => return Response::error(error),
    };
    match request {
        Request::Toggle { .. } => {
            let snapshot = hub.snapshot();
            // `*` normalizes: anything on turns everything off, otherwise all on.
            let enable = !indices.iter().any(|&i| snapshot.outputs[i].enabled);
            for &i in &indices {
                let update = if enable {
                    force(&mut engines[i], i, hub)
                } else {
                    None
                };
                controllers[i].send(Msg::SetEnabled {
                    enabled: enable,
                    update,
                });
            }
        }
        Request::Pause { duration_secs, .. } => {
            for &i in &indices {
                controllers[i].send(Msg::Pause {
                    duration: duration_secs.map(Duration::from_secs),
                });
            }
        }
        Request::Resume { .. } => {
            for &i in &indices {
                let update = force(&mut engines[i], i, hub);
                controllers[i].send(Msg::Resume { update });
            }
        }
        Request::Status | Request::Subscribe => unreachable!(),
    }
    // Controllers publish before answering a later query, so the hub is current after this.
    for &i in &indices {
        controllers[i].query().await;
    }
    let snapshot = hub.snapshot();
    Response::outputs(
        indices
            .iter()
            .map(|&i| snapshot.outputs[i].clone())
            .collect(),
    )
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
fn controller(
    config: &Config,
    index: usize,
    output: &Output,
    queue: &HookQueue,
    hub: &Arc<Hub>,
) -> Result<Controller> {
    let hub = hub.clone();
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
            publish: Some(Arc::new(move |status| hub.set_controller(index, status))),
        },
        device,
        connector,
        Arc::new(queue.handle()),
    ))
}

pub fn run(config: Config) -> Result<()> {
    let server = Server::bind(&server::runtime_dir()?)?;
    let hub = hub(&config);
    smol::block_on(run_async(config, hub, Some(server)))
}

/// Samples inputs and matches rules, publishing to `hub`, until SIGINT/SIGTERM.
pub fn observe(config: Config, hub: Arc<Hub>) -> Result<()> {
    smol::block_on(run_async(config, hub, None))
}

async fn run_async(config: Config, hub: Arc<Hub>, server: Option<Server>) -> Result<()> {
    let observe = server.is_none();
    // Without capture, observe mode still shows ALS and time.
    let capture = match capture::check_protocols().context("unable to start screen capture") {
        Ok(()) => true,
        Err(error) if observe => {
            eprintln!("woluma: {error:#}");
            false
        }
        Err(error) => return Err(error),
    };

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
        if !observe {
            controllers.push(controller(&config, index, output, &queue, &hub)?);
            queues.push(queue);
        }
        if !capture {
            continue;
        }

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

    let interval = match &config.als {
        Als::Webcam { interval, .. } | Als::Iio { interval, .. } => *interval,
        Als::None => DEFAULT_ALS_INTERVAL,
    };
    let (sampler, als) = Sampler::spawn(als::source(&config.als), interval);
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

    let mut serving = None;
    if let Some(server) = server {
        let (commands, requests) = async_channel::unbounded::<server::Command>();
        serving = Some(smol::spawn(server.serve(
            hub.clone(),
            commands,
            server::HEALTH_INTERVAL,
        )));
        let events = events.clone();
        smol::spawn(async move {
            while let Ok((request, reply)) = requests.recv().await {
                if events.send(Event::Control(request, reply)).await.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    if !observe {
        if !config.general.enabled {
            log::info!(
                "Daemon is disabled: rules are tracked but brightness and hooks are left alone"
            );
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
    }

    let mut als_value: Option<u8> = None;
    while let Ok(event) = inbox.recv().await {
        let now = chrono::Local::now().time();
        match event {
            Event::Luma(index, luma) => {
                log::trace!("{}: luma {luma}", engines[index].output());
                let inputs = Inputs {
                    luma,
                    als: als_value,
                    now,
                };
                advance(
                    &mut engines[index],
                    index,
                    inputs,
                    true,
                    &hub,
                    controllers.get(index),
                );
            }
            Event::Control(request, reply) => {
                let response = control(request, &mut engines, &controllers, &hub).await;
                let _ = reply.try_send(response);
            }
            Event::Als(_) | Event::Tick => {
                if let Event::Als(reading) = event {
                    log::debug!("als {reading}");
                    let value = reading.value;
                    hub.set_als(value);
                    if value == als_value {
                        continue;
                    }
                    als_value = value;
                }
                for (index, engine) in engines.iter_mut().enumerate() {
                    if let Some(last) = engine.last_inputs() {
                        let inputs = Inputs {
                            als: als_value,
                            now,
                            ..last
                        };
                        advance(engine, index, inputs, false, &hub, controllers.get(index));
                    }
                }
            }
            Event::Shutdown => break,
        }
    }

    drop(serving);
    capture_active.store(false, Ordering::Relaxed);
    sampler.shutdown(ALS_SHUTDOWN_WAIT).await;
    hooks_shutdown.store(true, Ordering::Relaxed);
    for controller in controllers {
        controller.close().await;
    }
    for queue in queues {
        queue.close().await;
    }
    if !observe {
        log::info!("woluma stopped");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: u32, m: u32, s: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, s).unwrap()
    }

    #[test]
    fn observe_mode_matches_rules_without_a_controller() {
        let rule = |name: &str, luma: Option<&str>| crate::rules::Rule {
            name: name.into(),
            when: crate::rules::When {
                luma: luma.map(|l| crate::rules::parse_num_cond(l).unwrap()),
                ..Default::default()
            },
            brightness: Some(40),
            run: vec!["exit 1".into()],
        };
        let mut engine = Engine::new(
            "eDP-1".into(),
            Arc::new(vec![rule("bright", Some(">= 70")), rule("default", None)]),
            Duration::from_secs(2),
            Default::default(),
        );
        let hub = Hub::new(&["eDP-1".into()], true, "none");
        let inputs = Inputs {
            luma: 88,
            als: None,
            now: t(12, 0, 0),
        };
        advance(&mut engine, 0, inputs, true, &hub, None);
        let o = &hub.snapshot().outputs[0];
        assert_eq!((o.luma, o.rule.as_deref()), (Some(88), Some("bright")));
        assert_eq!(o.brightness, None, "nothing was written");

        advance(
            &mut engine,
            0,
            Inputs { luma: 10, ..inputs },
            true,
            &hub,
            None,
        );
        let o = &hub.snapshot().outputs[0];
        assert_eq!(o.state, crate::control::State::Settling);
        assert!(o.settle_remaining_ms.unwrap() <= 2000);
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
