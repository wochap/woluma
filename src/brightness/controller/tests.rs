use super::*;
use crate::brightness::backlight::{tests::fake_sysfs, Backlight};

#[derive(Default)]
struct MockState {
    value: u32,
    writes: Vec<u32>,
    sink: Option<async_channel::Sender<u32>>,
    /// Report own writes back like inotify does.
    echo: bool,
}

#[derive(Clone, Default)]
struct Mock(Arc<Mutex<MockState>>);

impl Mock {
    fn new(value: u32, echo: bool) -> Self {
        let mock = Self::default();
        mock.0.lock().unwrap().value = value;
        mock.0.lock().unwrap().echo = echo;
        mock
    }

    /// Someone else (a key press, a script) changes the brightness.
    fn external(&self, raw: u32) {
        let mut state = self.0.lock().unwrap();
        state.value = raw;
        if let Some(sink) = &state.sink {
            let _ = sink.try_send(raw);
        }
    }

    fn writes(&self) -> Vec<u32> {
        self.0.lock().unwrap().writes.clone()
    }

    fn value(&self) -> u32 {
        self.0.lock().unwrap().value
    }
}

impl Device for Mock {
    fn read(&mut self) -> Result<u32> {
        Ok(self.0.lock().unwrap().value)
    }
    fn write(&mut self, raw: u32) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        state.value = raw;
        state.writes.push(raw);
        if state.echo {
            if let Some(sink) = &state.sink {
                let _ = sink.try_send(raw);
            }
        }
        Ok(())
    }
    fn min(&self) -> u32 {
        1
    }
    fn max(&self) -> u32 {
        255
    }
    fn watch(&mut self, sink: async_channel::Sender<u32>) -> Result<()> {
        self.0.lock().unwrap().sink = Some(sink);
        Ok(())
    }
}

/// Records dispatched jobs; each "runs" for 50 ms and optionally sets brightness.
#[derive(Clone, Default)]
struct Hooks {
    jobs: Arc<Mutex<Vec<String>>>,
    script_sets: Arc<Mutex<Option<(Mock, u32)>>>,
}

impl HookRunner for Hooks {
    fn dispatch(&self, job: Job) -> async_channel::Receiver<()> {
        self.jobs.lock().unwrap().push(job.rule);
        let (done, finished) = async_channel::bounded(1);
        let script = self.script_sets.lock().unwrap().clone();
        smol::spawn(async move {
            smol::Timer::after(Duration::from_millis(50)).await;
            if let Some((mock, raw)) = script {
                mock.external(raw);
            }
            drop(done);
        })
        .detach();
        finished
    }
}

impl Hooks {
    fn jobs(&self) -> Vec<String> {
        self.jobs.lock().unwrap().clone()
    }
}

fn settings(transition: Duration, mode: ManualOverride) -> Settings {
    Settings {
        output: "eDP-1".into(),
        enabled: true,
        transition,
        manual_override: mode,
        profile: Profile::backlight(),
        timing: Timing {
            hook_settle: Duration::from_millis(100),
            hook_cap: Duration::from_secs(2),
            ..Timing::default()
        },
        publish: None,
    }
}

fn spawn(mock: &Mock, hooks: &Hooks, settings: Settings) -> Controller {
    Controller::spawn(
        settings,
        Some(Box::new(mock.clone())),
        None,
        Arc::new(hooks.clone()),
    )
}

fn rule(name: &str, brightness: Option<u8>, run: bool) -> Msg {
    Msg::RuleChanged {
        rule: Some(name.into()),
        brightness,
        job: run.then(|| Job {
            rule: name.into(),
            entries: vec!["true".into()],
            env: vec![],
        }),
    }
}

async fn wait(ms: u64) {
    smol::Timer::after(Duration::from_millis(ms)).await;
}

async fn status(c: &Controller) -> Status {
    c.query().await.unwrap()
}

#[test]
fn applies_rule_brightness_and_skips_redundant_writes() {
    smol::block_on(async {
        let (mock, hooks) = (Mock::new(179, true), Hooks::default());
        let c = spawn(
            &mock,
            &hooks,
            settings(Duration::ZERO, ManualOverride::UntilRuleChange),
        );
        c.send(rule("bright", Some(40), false));
        wait(20).await;
        assert_eq!(mock.writes(), vec![102]);
        c.send(rule("other", Some(40), false));
        c.send(rule("run-only", None, true));
        wait(20).await;
        assert_eq!(
            mock.writes(),
            vec![102],
            "same raw value and no brightness key write nothing"
        );
        assert_eq!(hooks.jobs(), vec!["run-only"]);
        let s = status(&c).await;
        assert_eq!((s.current, s.paused), (Some(40), false));
        c.close().await;
    });
}

#[test]
fn own_ramp_is_not_external_and_retargets() {
    smol::block_on(async {
        let (mock, hooks) = (Mock::new(179, true), Hooks::default());
        let c = spawn(
            &mock,
            &hooks,
            settings(Duration::from_millis(300), ManualOverride::UntilRuleChange),
        );
        c.send(rule("bright", Some(40), false));
        wait(100).await;
        let s = status(&c).await;
        assert!(s.transitioning && s.target == Some(40), "{s:?}");
        let turned_at = mock.value();
        assert!(turned_at < 179 && turned_at > 102, "{turned_at}");
        c.send(rule("default", Some(70), false));
        wait(450).await;
        let writes = mock.writes();
        assert!(writes.len() > 3, "{writes:?}");
        assert!(
            writes.iter().all(|&w| w >= turned_at.min(102)),
            "never reached 40%: {writes:?}"
        );
        assert!(!writes.contains(&102), "{writes:?}");
        assert_eq!(*writes.last().unwrap(), 179);
        let s = status(&c).await;
        assert!(!s.paused && !s.transitioning, "{s:?}");
        c.close().await;
    });
}

#[test]
fn key_press_pauses_and_cancels_ramp() {
    smol::block_on(async {
        let (mock, hooks) = (Mock::new(179, true), Hooks::default());
        let c = spawn(
            &mock,
            &hooks,
            settings(Duration::from_millis(300), ManualOverride::UntilRuleChange),
        );
        c.send(rule("bright", Some(40), false));
        wait(60).await;
        mock.external(200);
        wait(20).await;
        let writes = mock.writes().len();
        wait(300).await;
        assert_eq!(mock.writes().len(), writes, "ramp stopped");
        assert_eq!(mock.value(), 200);
        let s = status(&c).await;
        assert!(
            s.paused && s.reason == Some(Reason::Manual) && s.until.is_none(),
            "{s:?}"
        );
        c.close().await;
    });
}

#[test]
fn hook_script_change_is_snapshotted_then_later_change_is_manual() {
    smol::block_on(async {
        let (mock, hooks) = (Mock::new(179, true), Hooks::default());
        *hooks.script_sets.lock().unwrap() = Some((mock.clone(), 77));
        let c = spawn(
            &mock,
            &hooks,
            settings(Duration::ZERO, ManualOverride::UntilRuleChange),
        );
        c.send(rule("bright", Some(40), true));
        wait(250).await;
        let s = status(&c).await;
        assert!(!s.paused, "script change is not manual: {s:?}");
        assert_eq!(s.current, Some(30));
        mock.external(128);
        wait(20).await;
        assert!(status(&c).await.paused);
        c.close().await;
    });
}

#[test]
fn same_rule_keeps_manual_level_and_rule_change_resumes() {
    smol::block_on(async {
        let (mock, hooks) = (Mock::new(179, true), Hooks::default());
        let c = spawn(
            &mock,
            &hooks,
            settings(Duration::ZERO, ManualOverride::UntilRuleChange),
        );
        c.send(rule("bright", Some(40), false));
        wait(20).await;
        mock.external(140);
        wait(20).await;
        c.send(rule("bright", Some(40), true));
        wait(20).await;
        assert_eq!(mock.value(), 140);
        assert!(hooks.jobs().is_empty());
        assert!(status(&c).await.paused);

        c.send(rule("default", Some(70), true));
        wait(20).await;
        assert_eq!(mock.value(), 179);
        assert_eq!(hooks.jobs(), vec!["default"]);
        assert!(!status(&c).await.paused);
        c.close().await;
    });
}

#[test]
fn duration_pause_ignores_rule_changes_and_expires_quietly() {
    smol::block_on(async {
        let (mock, hooks) = (Mock::new(179, true), Hooks::default());
        let mode = ManualOverride::For(Duration::from_millis(200));
        let c = spawn(&mock, &hooks, settings(Duration::ZERO, mode));
        c.send(rule("bright", Some(40), false));
        wait(20).await;
        mock.external(140);
        wait(20).await;
        let s = status(&c).await;
        assert!(s.paused && s.until.is_some(), "{s:?}");
        c.send(rule("default", Some(70), true));
        wait(50).await;
        assert!(status(&c).await.paused);
        wait(200).await;
        assert!(!status(&c).await.paused);
        assert_eq!(mock.writes(), vec![102], "expiry writes nothing");
        assert!(hooks.jobs().is_empty(), "expiry runs no hooks");

        c.send(rule("night", Some(10), true));
        wait(20).await;
        assert_eq!(mock.value(), 26);
        assert_eq!(hooks.jobs(), vec!["night"]);
        c.close().await;
    });
}

#[test]
fn further_manual_change_restarts_pause() {
    smol::block_on(async {
        let (mock, hooks) = (Mock::new(179, true), Hooks::default());
        let mode = ManualOverride::For(Duration::from_millis(300));
        let c = spawn(&mock, &hooks, settings(Duration::ZERO, mode));
        mock.external(100);
        wait(200).await;
        mock.external(120);
        wait(200).await;
        assert!(status(&c).await.paused, "restarted at the second change");
        wait(200).await;
        assert!(!status(&c).await.paused);
        c.close().await;
    });
}

#[test]
fn explicit_resume_applies_and_requested_pause() {
    smol::block_on(async {
        let (mock, hooks) = (Mock::new(179, true), Hooks::default());
        let c = spawn(
            &mock,
            &hooks,
            settings(Duration::ZERO, ManualOverride::UntilRuleChange),
        );
        c.send(Msg::Pause {
            duration: Some(Duration::from_secs(600)),
        });
        c.send(rule("bright", Some(40), true));
        wait(20).await;
        let s = status(&c).await;
        assert!(s.paused && s.reason == Some(Reason::Requested), "{s:?}");
        let until = s.until.unwrap().duration_since(SystemTime::now()).unwrap();
        assert!(until > Duration::from_secs(590));
        assert!(mock.writes().is_empty() && hooks.jobs().is_empty());

        c.send(Msg::Resume { update: None });
        wait(20).await;
        assert_eq!(mock.writes(), vec![102]);
        assert_eq!(hooks.jobs(), vec!["bright"]);
        assert!(!status(&c).await.paused);
        c.close().await;
    });
}

#[test]
fn ddc_poll_detects_changes_with_tolerance() {
    smol::block_on(async {
        let (mock, hooks) = (Mock::new(179, false), Hooks::default());
        let mut s = settings(Duration::ZERO, ManualOverride::UntilRuleChange);
        s.profile = Profile::ddc(Duration::from_millis(50));
        s.timing.poll_quiet = Duration::from_millis(100);
        let c = spawn(&mock, &hooks, s);
        c.send(rule("bright", Some(40), false));
        wait(20).await;
        // The monitor rounds our write by one unit.
        mock.0.lock().unwrap().value = 103;
        wait(200).await;
        assert!(!status(&c).await.paused);
        mock.0.lock().unwrap().value = 150;
        wait(150).await;
        assert!(status(&c).await.paused);
        c.close().await;
    });
}

#[test]
fn backlight_on_fake_sysfs() {
    let root = fake_sysfs("controller", "amdgpu_bl1", 179, 255);
    let device = Backlight::open_at(&root, "eDP-1", "amdgpu_bl1").unwrap();
    let file = root.join("amdgpu_bl1/brightness");
    smol::block_on(async {
        let hooks = Hooks::default();
        let c = Controller::spawn(
            settings(Duration::from_millis(100), ManualOverride::UntilRuleChange),
            Some(Box::new(device)),
            None,
            Arc::new(hooks),
        );
        c.send(rule("bright", Some(40), false));
        wait(300).await;
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "102");
        assert!(!status(&c).await.paused, "own writes are not external");
        std::fs::write(&file, "50").unwrap();
        wait(200).await;
        let s = status(&c).await;
        assert!(s.paused && s.current == Some(20), "{s:?}");
        c.close().await;
    });
}

fn update(name: &str, brightness: u8) -> Option<RuleUpdate> {
    Some(RuleUpdate {
        rule: Some(name.into()),
        brightness: Some(brightness),
        job: Some(Job {
            rule: name.into(),
            entries: vec!["true".into()],
            env: vec![],
        }),
    })
}

#[test]
fn disabling_leaves_brightness_and_enabling_applies_once() {
    smol::block_on(async {
        let (mock, hooks) = (Mock::new(179, true), Hooks::default());
        let c = spawn(
            &mock,
            &hooks,
            settings(Duration::ZERO, ManualOverride::UntilRuleChange),
        );
        c.send(rule("bright", Some(40), true));
        wait(20).await;
        assert_eq!(mock.writes(), vec![102]);
        c.send(Msg::SetEnabled {
            enabled: false,
            update: None,
        });
        c.send(rule("default", Some(70), true));
        mock.external(30);
        wait(200).await;
        let s = status(&c).await;
        assert!(!s.enabled && !s.paused, "{s:?}");
        assert_eq!(mock.writes(), vec![102], "nothing written while disabled");
        assert_eq!(hooks.jobs(), vec!["bright"]);

        c.send(Msg::SetEnabled {
            enabled: true,
            update: update("default", 70),
        });
        wait(20).await;
        assert_eq!(mock.writes(), vec![102, 179]);
        assert_eq!(hooks.jobs(), vec!["bright", "default"]);
        assert!(status(&c).await.enabled);
        c.close().await;
    });
}

#[test]
fn resume_after_manual_override_applies_rule() {
    smol::block_on(async {
        let (mock, hooks) = (Mock::new(179, true), Hooks::default());
        let c = spawn(
            &mock,
            &hooks,
            settings(Duration::ZERO, ManualOverride::UntilRuleChange),
        );
        c.send(rule("bright", Some(40), false));
        wait(20).await;
        mock.external(30);
        wait(20).await;
        let s = status(&c).await;
        assert!(s.paused && s.reason == Some(Reason::Manual) && s.until_rule_change);
        c.send(Msg::Resume {
            update: update("bright", 40),
        });
        wait(20).await;
        assert_eq!(mock.value(), 102);
        assert!(!status(&c).await.paused);
        c.close().await;
    });
}

#[test]
fn requested_pause_expires_or_waits_for_resume() {
    smol::block_on(async {
        let (mock, hooks) = (Mock::new(179, true), Hooks::default());
        let c = spawn(
            &mock,
            &hooks,
            settings(Duration::ZERO, ManualOverride::UntilRuleChange),
        );
        c.send(Msg::Pause {
            duration: Some(Duration::from_millis(100)),
        });
        wait(20).await;
        assert!(status(&c).await.paused);
        wait(150).await;
        assert!(!status(&c).await.paused, "expired");

        c.send(Msg::Pause { duration: None });
        c.send(rule("bright", Some(40), false));
        wait(20).await;
        let s = status(&c).await;
        assert!(s.paused && !s.until_rule_change, "survives a rule change");
        assert!(mock.writes().is_empty());
        c.close().await;
    });
}

#[test]
fn publishes_status_changes() {
    smol::block_on(async {
        let (mock, hooks) = (Mock::new(179, true), Hooks::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut s = settings(Duration::ZERO, ManualOverride::UntilRuleChange);
        let sink = seen.clone();
        s.publish = Some(Arc::new(move |status: Status| {
            sink.lock().unwrap().push(status)
        }));
        let c = spawn(&mock, &hooks, s);
        c.send(rule("bright", Some(40), false));
        wait(20).await;
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.last().unwrap().rule.as_deref(), Some("bright"));
        assert_eq!(seen.last().unwrap().current, Some(40));
        c.close().await;
    });
}
