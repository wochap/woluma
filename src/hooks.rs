//! Per-output FIFO hook queues running a rule's `run` entries via `sh -c`.

use crate::rules::{Rule, Transition};
use async_process::{Command, Stdio};
use futures_lite::{future, AsyncReadExt};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// One rule activation: its `run` entries plus the environment snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub rule: String,
    pub entries: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl Job {
    /// Builds the job for a transition, or `None` when there is nothing to run.
    pub fn from_transition(transition: &Transition, rules: &[Rule]) -> Option<Job> {
        let rule = &rules[transition.next?];
        if rule.run.is_empty() {
            return None;
        }
        Some(Job {
            rule: rule.name.clone(),
            entries: rule.run.clone(),
            env: env(transition, rules),
        })
    }
}

pub fn env(transition: &Transition, rules: &[Rule]) -> Vec<(String, String)> {
    let name = |i: Option<usize>| i.map(|i| rules[i].name.clone()).unwrap_or_default();
    let inputs = &transition.inputs;
    vec![
        ("WOLUMA_OUTPUT".into(), transition.output.clone()),
        ("WOLUMA_RULE".into(), name(transition.next)),
        ("WOLUMA_PREV_RULE".into(), name(transition.prev)),
        ("WOLUMA_LUMA".into(), inputs.luma.to_string()),
        (
            "WOLUMA_ALS".into(),
            inputs.als.map(|a| a.to_string()).unwrap_or_default(),
        ),
        ("WOLUMA_TIME".into(), inputs.now.format("%H:%M").to_string()),
        (
            "WOLUMA_BRIGHTNESS".into(),
            transition
                .next
                .and_then(|i| rules[i].brightness)
                .map(|b| b.to_string())
                .unwrap_or_default(),
        ),
    ]
}

/// Handle to one output's hook worker.
pub struct HookQueue {
    output: String,
    sender: async_channel::Sender<Job>,
    pending: Arc<AtomicUsize>,
    worker: smol::Task<()>,
}

impl HookQueue {
    pub fn spawn(output: String, timeout: Duration, shutdown: Arc<AtomicBool>) -> Self {
        let (sender, receiver) = async_channel::unbounded::<Job>();
        let pending = Arc::new(AtomicUsize::new(0));
        let worker = {
            let output = output.clone();
            let pending = pending.clone();
            smol::spawn(async move {
                while let Ok(job) = receiver.recv().await {
                    if !shutdown.load(Ordering::Relaxed) {
                        run_job(&output, &job, timeout).await;
                    }
                    pending.fetch_sub(1, Ordering::SeqCst);
                }
            })
        };
        Self {
            output,
            sender,
            pending,
            worker,
        }
    }

    /// Enqueues a job without waiting for it to run.
    pub fn enqueue(&self, job: Job) {
        self.pending.fetch_add(1, Ordering::SeqCst);
        if self.sender.try_send(job).is_err() {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            log::error!("Hook queue for '{}' is closed", self.output);
        }
    }

    /// True when no hook job is queued or running. Used later for brightness-change detection.
    #[allow(dead_code)] // consumed by add-brightness-control
    pub fn is_idle(&self) -> bool {
        self.pending.load(Ordering::SeqCst) == 0
    }

    /// Stops accepting jobs and waits for the worker to finish.
    pub async fn close(self) {
        self.sender.close();
        self.worker.await;
    }
}

pub async fn run_job(output: &str, job: &Job, timeout: Duration) {
    for entry in &job.entries {
        run_entry(output, &job.rule, entry, &job.env, timeout).await;
    }
}

async fn run_entry(
    output: &str,
    rule: &str,
    entry: &str,
    env: &[(String, String)],
    timeout: Duration,
) {
    log::debug!("Running hook for rule '{rule}' on '{output}': {entry}");
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(entry)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            log::error!("Unable to start hook for rule '{rule}' on '{output}' ({entry}): {error}");
            return;
        }
    };

    let (mut stdout, mut stderr) = (child.stdout.take(), child.stderr.take());
    let outcome = {
        let finished = async {
            let read = async {
                let mut out = String::new();
                let mut err = String::new();
                if let Some(s) = stdout.as_mut() {
                    let _ = s.read_to_string(&mut out).await;
                }
                if let Some(s) = stderr.as_mut() {
                    let _ = s.read_to_string(&mut err).await;
                }
                (out, err)
            };
            let ((out, err), status) = future::zip(read, child.status()).await;
            Some((out, err, status))
        };
        let expired = async {
            smol::Timer::after(timeout).await;
            None
        };
        future::or(finished, expired).await
    };

    match outcome {
        None => {
            let _ = child.kill();
            let _ = child.status().await;
            log::warn!("Hook for rule '{rule}' on '{output}' timed out after {timeout:?} and was killed: {entry}");
        }
        Some((_, _, Err(error))) => {
            log::error!("Hook for rule '{rule}' on '{output}' failed ({entry}): {error}");
        }
        Some((out, err, Ok(status))) if !status.success() => {
            log::warn!(
                "Hook for rule '{rule}' on '{output}' exited with {status} ({entry})\nstdout: {}\nstderr: {}",
                out.trim_end(),
                err.trim_end()
            );
        }
        Some(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Inputs, When};
    use chrono::NaiveTime;
    use std::time::Instant;

    fn rule(name: &str, brightness: Option<u8>, run: &[&str]) -> Rule {
        Rule {
            name: name.into(),
            when: When::default(),
            brightness,
            run: run.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn transition(prev: Option<usize>, next: Option<usize>) -> Transition {
        Transition {
            output: "eDP-1".into(),
            prev,
            next,
            inputs: Inputs {
                luma: 85,
                als: None,
                now: NaiveTime::from_hms_opt(21, 40, 0).unwrap(),
            },
        }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("woluma-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn environment_contents() {
        let rules = vec![
            rule("night", Some(10), &["x"]),
            rule("night-bright", Some(30), &["x"]),
        ];
        let env = env(&transition(Some(0), Some(1)), &rules);
        let get = |k: &str| env.iter().find(|(key, _)| key == k).unwrap().1.as_str();
        assert_eq!(get("WOLUMA_OUTPUT"), "eDP-1");
        assert_eq!(get("WOLUMA_RULE"), "night-bright");
        assert_eq!(get("WOLUMA_PREV_RULE"), "night");
        assert_eq!(get("WOLUMA_LUMA"), "85");
        assert_eq!(get("WOLUMA_ALS"), "");
        assert_eq!(get("WOLUMA_TIME"), "21:40");
        assert_eq!(get("WOLUMA_BRIGHTNESS"), "30");

        let rules = vec![rule("only-run", None, &["x"])];
        let env = super::env(&transition(None, Some(0)), &rules);
        let get = |k: &str| env.iter().find(|(key, _)| key == k).unwrap().1.as_str();
        assert_eq!(get("WOLUMA_BRIGHTNESS"), "");
        assert_eq!(get("WOLUMA_PREV_RULE"), "");
    }

    #[test]
    fn no_job_for_no_rule_or_empty_run() {
        let rules = vec![rule("a", Some(1), &[])];
        assert_eq!(
            Job::from_transition(&transition(None, Some(0)), &rules),
            None
        );
        assert_eq!(
            Job::from_transition(&transition(Some(0), None), &rules),
            None
        );
    }

    #[test]
    fn ordered_and_failure_continues_with_env() {
        let dir = tmp("order");
        let f = dir.join("f");
        let f = f.display();
        let rules = vec![rule(
            "bright",
            None,
            &[
                &format!("echo 1 >> {f}"),
                "exit 1",
                &format!("echo 2 >> {f}; echo \"$WOLUMA_RULE\" >> {f}"),
            ],
        )];
        let job = Job::from_transition(&transition(None, Some(0)), &rules).unwrap();
        smol::block_on(run_job("eDP-1", &job, DEFAULT_TIMEOUT));
        assert_eq!(
            std::fs::read_to_string(dir.join("f")).unwrap(),
            "1\n2\nbright\n"
        );
    }

    #[test]
    fn timeout_kills() {
        let job = Job {
            rule: "slow".into(),
            entries: vec!["sleep 10".into()],
            env: vec![],
        };
        let start = Instant::now();
        smol::block_on(run_job("eDP-1", &job, Duration::from_millis(200)));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn queue_is_fifo_and_non_blocking() {
        let dir = tmp("fifo");
        let f = dir.join("f").display().to_string();
        smol::block_on(async {
            let queue = HookQueue::spawn(
                "eDP-1".into(),
                DEFAULT_TIMEOUT,
                Arc::new(AtomicBool::new(false)),
            );
            let start = Instant::now();
            for i in 0..3 {
                queue.enqueue(Job {
                    rule: format!("r{i}"),
                    entries: vec![format!("sleep 0.1; echo {i} >> {f}")],
                    env: vec![],
                });
            }
            assert!(
                start.elapsed() < Duration::from_millis(100),
                "enqueue must not wait"
            );
            assert!(!queue.is_idle());
            queue.close().await;
        });
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "0\n1\n2\n");
    }
}
