//! Short host-input lease with physical human activity priority.
//! The fixed privileged reader emits activity timing only, never keystrokes.
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct HostGuard {
    inner: Arc<Inner>,
}
struct Inner {
    enabled: AtomicBool,
    monitoring: AtomicBool,
    monitor_stop: Mutex<Option<CancellationToken>>,
    state: Mutex<State>,
    lease: Mutex<Option<(String, Instant)>>,
}
struct State {
    ready: bool,
    tick: Instant,
    idle_ms: u64,
    activity: Instant,
    devices: u64,
}
impl HostGuard {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                enabled: AtomicBool::new(true),
                monitoring: AtomicBool::new(false),
                monitor_stop: Mutex::new(None),
                state: Mutex::new(State {
                    ready: false,
                    tick: Instant::now(),
                    idle_ms: 0,
                    activity: Instant::now(),
                    devices: 0,
                }),
                lease: Mutex::new(None),
            }),
        }
    }
    pub async fn set_enabled(&self, enabled: bool) {
        self.inner.enabled.store(enabled, Ordering::SeqCst);
        if !enabled {
            *self.inner.lease.lock().await = None;
            if let Some(token) = self.inner.monitor_stop.lock().await.take() {
                token.cancel();
            }
            self.inner.state.lock().await.ready = false;
        }
    }
    pub async fn status(&self) -> Value {
        let state = self.inner.state.lock().await;
        let now = Instant::now();
        let healthy = state.ready && now.duration_since(state.tick) < Duration::from_millis(700);
        let lease = self.inner.lease.lock().await;
        let owner = lease
            .as_ref()
            .filter(|(_, end)| *end > now)
            .map(|(owner, _)| owner);
        json!({"enabled":self.inner.enabled.load(Ordering::SeqCst),"physical_monitor":healthy,"human_idle_ms":if healthy{Some(state.idle_ms)}else{None},"monitored_devices":state.devices,"lease_owner":owner,"lease_remaining_secs":lease.as_ref().map(|(_,end)|end.saturating_duration_since(now).as_secs()).unwrap_or(0),"policy":"DOM and isolated desktop first; host takeover only when necessary, human input immediately cancels it"})
    }
    async fn ensure_monitor(&self) -> Result<()> {
        if self
            .inner
            .monitoring
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(());
        }
        let mut command = crate::privilege::command("/usr/bin/python3").await;
        command
            .args([
                "-I",
                "-B",
                "-u",
                "-c",
                include_str!("../helpers/input_activity.py"),
            ])
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.inner.monitoring.store(false, Ordering::SeqCst);
                return Err(error.into());
            }
        };
        let monitor_stop = CancellationToken::new();
        *self.inner.monitor_stop.lock().await = Some(monitor_stop.clone());
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let mut child = child;
            let Some(stdout) = child.stdout.take() else {
                inner.monitoring.store(false, Ordering::SeqCst);
                return;
            };
            let mut lines = BufReader::new(stdout).lines();
            loop {
                let line = tokio::select! {_=monitor_stop.cancelled()=>break,line=lines.next_line()=>match line{Ok(Some(line))=>line,_=>break}};
                if line.len() > 1024 {
                    break;
                }
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    break;
                };
                let mut state = inner.state.lock().await;
                let now = Instant::now();
                let idle = value["idle_ms"].as_u64().unwrap_or(0);
                if idle < state.idle_ms || state.devices == 0 {
                    state.activity = now.checked_sub(Duration::from_millis(idle)).unwrap_or(now);
                }
                state.tick = now;
                state.idle_ms = idle;
                state.ready = value["ready"] == true;
                state.devices = value["devices"].as_u64().unwrap_or(0);
            }
            inner.state.lock().await.ready = false;
            inner.monitoring.store(false, Ordering::SeqCst);
            let _ = child.kill().await;
            let _ = child.wait().await;
        });
        Ok(())
    }
    pub async fn acquire(&self, owner: &str, cancel: &CancellationToken) -> Result<HostLease> {
        ensure!(
            self.inner.enabled.load(Ordering::SeqCst),
            "HOST_DISABLED: Host takeover was disabled in the GUI. Use browser/desktop isolation instead."
        );
        self.ensure_monitor().await?;
        // Readiness wait is bounded and never waits for user inactivity forever.
        for _ in 0..40 {
            let ready = {
                let state = self.inner.state.lock().await;
                state.ready && state.idle_ms >= 1500
            };
            if ready {
                break;
            }
            tokio::select! {_=cancel.cancelled()=>bail!("Cancelled"),_=tokio::time::sleep(Duration::from_millis(50))=>{}}
        }
        let state = self.inner.state.lock().await;
        ensure!(
            state.ready && state.tick.elapsed() < Duration::from_millis(700),
            "HOST_MONITOR_UNAVAILABLE: Cannot reliably observe physical human input; refusing blind host injection. Isolated browser/desktop remain available."
        );
        ensure!(
            state.idle_ms >= 1500,
            "HUMAN_ACTIVE: Yielding keyboard/mouse to the user. Prefer DOM or the isolated desktop; retry host only after the user is idle."
        );
        let activity = state.activity;
        drop(state);
        let mut lease = self.inner.lease.lock().await;
        let now = Instant::now();
        ensure!(
            lease
                .as_ref()
                .is_none_or(|(prior, end)| prior == owner || *end <= now),
            "COMPUTER_BUSY: Another session owns the short host-input lease"
        );
        let expires = now + Duration::from_secs(30);
        *lease = Some((owner.to_owned(), expires));
        let token = cancel.child_token();
        let watcher_token = token.clone();
        let guard = self.clone();
        let owner = owner.to_owned();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {_=watcher_token.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(25))=>{}}
                let state = guard.inner.state.lock().await;
                if !guard.inner.enabled.load(Ordering::SeqCst)
                    || !state.ready
                    || state.tick.elapsed() > Duration::from_millis(700)
                    || state.activity > activity
                    || Instant::now() > expires
                {
                    watcher_token.cancel();
                    drop(state);
                    let mut lease = guard.inner.lease.lock().await;
                    if lease.as_ref().is_some_and(|(prior, _)| prior == &owner) {
                        *lease = None;
                    }
                    break;
                }
            }
        });
        Ok(HostLease { token, task })
    }
    pub async fn release(&self, owner: Option<&str>) {
        if owner.is_none() {
            if let Some(token) = self.inner.monitor_stop.lock().await.take() {
                token.cancel();
            }
            self.inner.state.lock().await.ready = false;
        }
        let mut lease = self.inner.lease.lock().await;
        if owner.is_none()
            || lease
                .as_ref()
                .is_some_and(|(prior, _)| Some(prior.as_str()) == owner)
        {
            *lease = None;
        }
    }
}
pub struct HostLease {
    pub token: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for HostLease {
    fn drop(&mut self) {
        self.token.cancel();
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "Explicit opt-in: reads physical input activity timing through root; never injects or records keystrokes"]
    async fn physical_monitor_timing_only_smoke() {
        let guard = HostGuard::new();
        let cancel = CancellationToken::new();
        let result = guard.acquire("timing-test", &cancel).await;
        let status = guard.status().await;
        assert_eq!(status["physical_monitor"], true, "{status}");
        assert!(status["monitored_devices"].as_u64().unwrap_or(0) > 0);
        println!(
            "Physical timing monitor healthy; no host screenshot, keyboard or pointer action executed"
        );
        drop(result);
        guard.release(None).await;
    }
    #[tokio::test]
    async fn default_status_does_not_start_monitor() {
        let guard = HostGuard::new();
        let status = guard.status().await;
        assert_eq!(status["enabled"], true);
        assert_eq!(status["physical_monitor"], false);
        assert!(!guard.inner.monitoring.load(Ordering::SeqCst));
        guard.set_enabled(false).await;
        assert!(
            guard
                .acquire("owner", &CancellationToken::new())
                .await
                .is_err()
        );
    }
}
