//! One actor per output. It owns the device, the ramp, the last value it wrote and the
//! pause state, and dispatches the rule's hooks itself so pausing and hook execution
//! can't race.

use super::mapping::{pct_to_raw, raw_to_pct};
use super::ramp::Ramp;
use super::Device;
use crate::config::ManualOverride;
use crate::hooks::{HookRunner, Job};
use crate::manual_override::{Pause, Reason, Until, HOOK_CAP, HOOK_SETTLE};
use anyhow::Result;
use futures_lite::future;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Opens a device that may not be present yet (DDC); retried until it succeeds.
pub type Connector = Arc<dyn Fn() -> Result<Box<dyn Device>> + Send + Sync>;
type Shared = Arc<Mutex<Box<dyn Device>>>;
/// Receives the actor's status whenever it changes.
pub type Publish = Arc<dyn Fn(Status) + Send + Sync>;

/// Device-kind specific behavior.
#[derive(Debug, Clone, Copy)]
pub struct Profile {
    /// Minimum spacing between ramp writes.
    pub step: Duration,
    /// Readings this close to a value we wrote count as our own.
    pub tolerance: u32,
    /// Poll interval for devices without change events; `None` or zero disables polling.
    pub poll: Option<Duration>,
}

impl Profile {
    pub fn backlight() -> Self {
        Self {
            step: Duration::from_millis(16),
            tolerance: 0,
            poll: None,
        }
    }

    pub fn ddc(poll: Duration) -> Self {
        Self {
            step: Duration::from_millis(200),
            tolerance: 1,
            poll: Some(poll),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub hook_settle: Duration,
    pub hook_cap: Duration,
    /// Retry interval while a device can't be opened.
    pub reconnect: Duration,
    /// No poll this soon after our own write (DDC monitors report late).
    pub poll_quiet: Duration,
    /// How long values we wrote are remembered for own-write classification.
    pub own_write_memory: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            hook_settle: HOOK_SETTLE,
            hook_cap: HOOK_CAP,
            reconnect: Duration::from_secs(30),
            poll_quiet: Duration::from_millis(500),
            own_write_memory: Duration::from_secs(1),
        }
    }
}

#[derive(Clone)]
pub struct Settings {
    pub output: String,
    pub enabled: bool,
    pub transition: Duration,
    pub manual_override: ManualOverride,
    pub profile: Profile,
    pub timing: Timing,
    pub publish: Option<Publish>,
}

/// A rule activation re-evaluated outside the normal transition flow; stored, not applied.
pub struct RuleUpdate {
    pub rule: Option<String>,
    pub brightness: Option<u8>,
    pub job: Option<Job>,
}

pub enum Msg {
    /// The engine picked a new active rule; `job` is `Some` when it has `run` commands.
    RuleChanged {
        rule: Option<String>,
        brightness: Option<u8>,
        job: Option<Job>,
    },
    /// Manual pause (reason `Requested`); `None` lasts until resumed.
    Pause {
        duration: Option<Duration>,
    },
    /// Clears any pause and applies `update` (or the stored rule).
    Resume {
        update: Option<RuleUpdate>,
    },
    /// Enabling clears any pause and applies `update` (or the stored rule) right away.
    SetEnabled {
        enabled: bool,
        update: Option<RuleUpdate>,
    },
    Query(async_channel::Sender<Status>),
    Observed(u32),
    HooksFinished(u64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub output: String,
    pub enabled: bool,
    pub current: Option<u8>,
    pub target: Option<u8>,
    pub transitioning: bool,
    pub rule: Option<String>,
    pub paused: bool,
    pub reason: Option<Reason>,
    /// `None` with `paused` means until the next rule change or until resumed.
    pub until: Option<SystemTime>,
    /// `paused` until the next rule change.
    pub until_rule_change: bool,
}

/// Handle to a running output actor.
pub struct Controller {
    sender: async_channel::Sender<Msg>,
    task: smol::Task<()>,
}

impl Controller {
    pub fn spawn(
        settings: Settings,
        device: Option<Box<dyn Device>>,
        connector: Option<Connector>,
        hooks: Arc<dyn HookRunner>,
    ) -> Self {
        let (sender, inbox) = async_channel::unbounded();
        let actor = Actor {
            me: sender.clone(),
            settings,
            hooks,
            connector,
            device: None,
            current: None,
            expected: None,
            recent: VecDeque::new(),
            ramp: None,
            rule: None,
            brightness: None,
            job: None,
            pause: None,
            grace: None,
            generation: 0,
            last_write: None,
            next_poll: None,
            next_connect: None,
            reported_failure: false,
            published: None,
        };
        let task = smol::spawn(actor.run(inbox, device));
        Self { sender, task }
    }

    pub fn send(&self, msg: Msg) {
        let _ = self.sender.try_send(msg);
    }

    pub async fn query(&self) -> Option<Status> {
        let (reply, response) = async_channel::bounded(1);
        self.sender.send(Msg::Query(reply)).await.ok()?;
        response.recv().await.ok()
    }

    /// Stops the actor once it has handled the messages already sent.
    pub async fn close(self) {
        self.sender.close();
        self.task.await;
    }
}

struct Grace {
    generation: u64,
    cap: Instant,
    settle_at: Option<Instant>,
}

struct Actor {
    me: async_channel::Sender<Msg>,
    settings: Settings,
    hooks: Arc<dyn HookRunner>,
    connector: Option<Connector>,
    device: Option<(Shared, u32, u32)>,
    current: Option<u32>,
    expected: Option<u32>,
    recent: VecDeque<(u32, Instant)>,
    ramp: Option<Ramp>,
    rule: Option<String>,
    brightness: Option<u8>,
    job: Option<Job>,
    pause: Option<Pause>,
    grace: Option<Grace>,
    generation: u64,
    last_write: Option<Instant>,
    next_poll: Option<Instant>,
    next_connect: Option<Instant>,
    reported_failure: bool,
    published: Option<Status>,
}

impl Actor {
    async fn run(mut self, inbox: async_channel::Receiver<Msg>, device: Option<Box<dyn Device>>) {
        match device {
            Some(device) => self.install(device).await,
            None if self.connector.is_some() => self.next_connect = Some(Instant::now()),
            None => {}
        }
        loop {
            let msg = match self.deadline() {
                Some(at) => {
                    future::or(async { Some(inbox.recv().await) }, async {
                        smol::Timer::at(at).await;
                        None
                    })
                    .await
                }
                None => Some(inbox.recv().await),
            };
            match msg {
                Some(Ok(msg)) => self.handle(msg).await,
                Some(Err(_)) => break,
                None => self.on_timer(Instant::now()).await,
            }
            self.publish();
        }
    }

    fn publish(&mut self) {
        let Some(publish) = &self.settings.publish else {
            return;
        };
        let status = self.status();
        if self.published.as_ref() != Some(&status) {
            publish(status.clone());
            self.published = Some(status);
        }
    }

    fn store(&mut self, update: Option<RuleUpdate>) {
        if let Some(update) = update {
            self.rule = update.rule;
            self.brightness = update.brightness;
            self.job = update.job;
        }
    }

    fn output(&self) -> &str {
        &self.settings.output
    }

    fn deadline(&self) -> Option<Instant> {
        [
            self.ramp.as_ref().map(Ramp::due),
            self.grace
                .as_ref()
                .map(|g| g.settle_at.map_or(g.cap, |s| s.min(g.cap))),
            self.pause.as_ref().and_then(Pause::deadline),
            self.next_poll,
            self.next_connect,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    async fn device_call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut dyn Device) -> Result<T> + Send + 'static,
    ) -> Option<Result<T>> {
        let (device, _, _) = self.device.as_ref()?;
        let device = device.clone();
        Some(smol::unblock(move || f(&mut **device.lock().unwrap())).await)
    }

    async fn install(&mut self, mut device: Box<dyn Device>) {
        let (sink, values) = async_channel::unbounded();
        if let Err(e) = device.watch(sink) {
            log::warn!(
                "{}: external brightness changes won't be detected: {e:#}",
                self.output()
            );
        }
        let me = self.me.clone();
        smol::spawn(async move {
            while let Ok(raw) = values.recv().await {
                if me.send(Msg::Observed(raw)).await.is_err() {
                    break;
                }
            }
        })
        .detach();

        let (min, max) = (device.min(), device.max());
        self.device = Some((Arc::new(Mutex::new(device)), min, max));
        self.current = self.read().await;
        self.expected = self.current;
        self.next_poll = self
            .settings
            .profile
            .poll
            .filter(|p| !p.is_zero())
            .map(|p| Instant::now() + p);
    }

    async fn read(&mut self) -> Option<u32> {
        match self.device_call(|d| d.read()).await? {
            Ok(raw) => Some(raw),
            Err(e) => {
                log::error!("{}: unable to read brightness: {e:#}", self.output());
                None
            }
        }
    }

    async fn handle(&mut self, msg: Msg) {
        let now = Instant::now();
        match msg {
            Msg::RuleChanged {
                rule,
                brightness,
                job,
            } => {
                let changed = rule != self.rule;
                self.rule = rule;
                self.brightness = brightness;
                self.job = job;
                if !self.settings.enabled {
                    return;
                }
                if let Some(pause) = self.pause {
                    if pause.until == Until::RuleChange && changed {
                        log::info!("{}: active rule changed, resuming", self.output());
                        self.pause = None;
                    } else {
                        log::debug!("{}: paused, not applying the new rule", self.output());
                        return;
                    }
                }
                self.apply(now).await;
            }
            Msg::Pause { duration } => {
                self.ramp = None;
                self.pause = Some(Pause::start(
                    Reason::Requested,
                    duration,
                    &self.settings.manual_override,
                    now,
                ));
                log::info!("{}: paused on request", self.output());
            }
            Msg::Resume { update } => {
                self.store(update);
                if self.pause.take().is_some() {
                    log::info!("{}: resumed on request", self.output());
                    if self.settings.enabled {
                        self.apply(now).await;
                    }
                }
            }
            Msg::SetEnabled { enabled, update } => {
                self.store(update);
                if enabled == self.settings.enabled {
                    return;
                }
                self.settings.enabled = enabled;
                if enabled {
                    log::info!("{}: enabled", self.output());
                    self.pause = None;
                    self.apply(now).await;
                } else {
                    log::info!("{}: disabled", self.output());
                    self.ramp = None;
                }
            }
            Msg::Query(reply) => {
                let _ = reply.try_send(self.status());
            }
            Msg::Observed(raw) => self.observed(raw, now),
            Msg::HooksFinished(generation) => {
                if let Some(grace) = &mut self.grace {
                    if grace.generation == generation {
                        grace.settle_at = Some(now + self.settings.timing.hook_settle);
                    }
                }
            }
        }
    }

    /// Applies the active rule: brightness first, then hooks.
    async fn apply(&mut self, now: Instant) {
        if let Some(pct) = self.brightness {
            self.set_target(pct, now).await;
        }
        if let Some(job) = self.job.clone() {
            self.generation += 1;
            let generation = self.generation;
            self.grace = Some(Grace {
                generation,
                cap: now + self.settings.timing.hook_cap,
                settle_at: None,
            });
            let finished = self.hooks.dispatch(job);
            let me = self.me.clone();
            smol::spawn(async move {
                let _ = finished.recv().await;
                let _ = me.send(Msg::HooksFinished(generation)).await;
            })
            .detach();
        }
    }

    async fn set_target(&mut self, pct: u8, now: Instant) {
        let Some((_, min, max)) = self.device else {
            return;
        };
        let target = pct_to_raw(pct, min, max);
        if let Some(ramp) = &mut self.ramp {
            ramp.retarget(target, self.settings.transition, now);
            return;
        }
        let Some(current) = self.read().await else {
            return;
        };
        self.current = Some(current);
        self.expected = Some(current);
        if current == target {
            return;
        }
        self.ramp = Some(Ramp::new(
            target,
            self.settings.transition,
            self.settings.profile.step,
            now,
        ));
        self.step_ramp(now).await;
    }

    async fn step_ramp(&mut self, now: Instant) {
        let (Some(ramp), Some(current)) = (&mut self.ramp, self.current) else {
            self.ramp = None;
            return;
        };
        let target = ramp.target();
        let next = ramp.step(current, now);
        self.remember(next, now);
        match self.device_call(move |d| d.write(next)).await {
            Some(Ok(())) => {
                self.reported_failure = false;
                self.current = Some(next);
                self.last_write = Some(Instant::now());
                if next == target {
                    self.ramp = None;
                }
            }
            Some(Err(e)) => {
                if !self.reported_failure {
                    log::error!("{}: unable to set brightness: {e:#}", self.output());
                    self.reported_failure = true;
                }
                self.ramp = None;
            }
            None => self.ramp = None,
        }
    }

    /// Records a value we are about to write, so its change notification is ours.
    fn remember(&mut self, raw: u32, now: Instant) {
        self.expected = Some(raw);
        self.recent.push_back((raw, now));
        let memory = self.settings.timing.own_write_memory;
        while self
            .recent
            .front()
            .is_some_and(|&(_, at)| now.duration_since(at) > memory)
        {
            self.recent.pop_front();
        }
    }

    fn is_own(&self, raw: u32) -> bool {
        let tolerance = self.settings.profile.tolerance;
        let memory = self.settings.timing.own_write_memory;
        self.expected.is_some_and(|e| e.abs_diff(raw) <= tolerance)
            || self
                .recent
                .iter()
                .any(|&(written, at)| at.elapsed() <= memory && written.abs_diff(raw) <= tolerance)
    }

    fn observed(&mut self, raw: u32, now: Instant) {
        if self.is_own(raw) {
            return;
        }
        self.current = Some(raw);
        self.expected = Some(raw);
        if self.grace.is_some() {
            log::debug!(
                "{}: brightness changed to {raw} during hook run",
                self.output()
            );
            return;
        }
        if !self.settings.enabled {
            return;
        }
        self.ramp = None;
        let restart = self.pause.is_some();
        self.pause = Some(Pause::start(
            Reason::Manual,
            None,
            &self.settings.manual_override,
            now,
        ));
        log::info!(
            "{}: manual brightness change to {raw}, {} pause",
            self.output(),
            if restart { "restarting" } else { "starting" }
        );
    }

    async fn on_timer(&mut self, now: Instant) {
        if self.ramp.as_ref().is_some_and(|r| r.due() <= now) {
            self.step_ramp(now).await;
        }
        if let Some(grace) = &self.grace {
            let settled = grace.settle_at.is_some_and(|s| s <= now);
            if settled || grace.cap <= now {
                if !settled {
                    log::warn!(
                        "{}: hooks still running after {:?}, taking the brightness snapshot now",
                        self.output(),
                        self.settings.timing.hook_cap
                    );
                }
                self.grace = None;
                if self.ramp.is_none() {
                    if let Some(raw) = self.read().await {
                        self.current = Some(raw);
                        self.expected = Some(raw);
                    }
                }
            }
        }
        if self
            .pause
            .and_then(|p| p.deadline())
            .is_some_and(|at| at <= now)
        {
            log::info!("{}: pause expired", self.output());
            self.pause = None;
        }
        if self.next_poll.is_some_and(|at| at <= now) {
            self.poll(now).await;
        }
        if self.next_connect.is_some_and(|at| at <= now) {
            self.connect(now).await;
        }
    }

    async fn poll(&mut self, now: Instant) {
        let interval = self.settings.profile.poll.unwrap_or_default();
        let quiet = self.settings.timing.poll_quiet;
        if let Some(last) = self.last_write.filter(|w| now.duration_since(*w) < quiet) {
            self.next_poll = Some(last + quiet);
            return;
        }
        // Mid-ramp readings would only race our own writes.
        if self.ramp.is_none() {
            if let Some(raw) = self.read().await {
                self.observed(raw, now);
            }
        }
        self.next_poll = Some(Instant::now() + interval);
    }

    async fn connect(&mut self, now: Instant) {
        let Some(connector) = self.connector.clone() else {
            return;
        };
        match smol::unblock(move || connector()).await {
            Ok(device) => {
                self.next_connect = None;
                self.install(device).await;
                if self.settings.enabled && self.pause.is_none() {
                    if let Some(pct) = self.brightness {
                        self.set_target(pct, Instant::now()).await;
                    }
                }
            }
            Err(e) => {
                let retry = self.settings.timing.reconnect;
                if !self.reported_failure {
                    log::error!("{}: {e:#}; retrying every {retry:?}", self.output());
                    self.reported_failure = true;
                } else {
                    log::debug!("{}: {e:#}", self.output());
                }
                self.next_connect = Some(now + retry);
            }
        }
    }

    fn status(&self) -> Status {
        let pct = |raw: u32| {
            self.device
                .as_ref()
                .map(|&(_, _, max)| raw_to_pct(raw, max))
        };
        let target = match &self.ramp {
            Some(ramp) => pct(ramp.target()),
            None if self.pause.is_none() && self.settings.enabled => self.brightness,
            None => None,
        };
        Status {
            output: self.settings.output.clone(),
            enabled: self.settings.enabled,
            current: self.current.and_then(pct),
            target,
            transitioning: self.ramp.is_some(),
            rule: self.rule.clone(),
            paused: self.pause.is_some(),
            reason: self.pause.map(|p| p.reason),
            until: self.pause.and_then(|p| match p.until {
                Until::At(_, at) => Some(at),
                Until::RuleChange | Until::Resume => None,
            }),
            until_rule_change: self.pause.is_some_and(|p| p.until == Until::RuleChange),
        }
    }
}

#[cfg(test)]
mod tests;
