use device_query::{DeviceQuery, DeviceState};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct ActivityMonitor {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    last_activity: Instant,
    last_cursor: (i32, i32),
    last_keys_snapshot: Vec<device_query::Keycode>,
}

impl ActivityMonitor {
    pub fn start(poll_interval: Duration) -> (Self, JoinHandle<()>) {
        // Polling runs on a dedicated OS thread because `DeviceState` on
        // Linux holds an `Rc<X11Connection>` and is therefore !Send — it
        // cannot be moved into a tokio task. The polling loop is purely
        // sleep + read-syscall + small mutex update, so a plain thread
        // is the right shape here regardless of platform.
        let state = DeviceState::new();
        let initial_cursor = state.get_mouse().coords;
        let initial_keys = state.get_keys();

        let inner = Arc::new(Mutex::new(Inner {
            last_activity: Instant::now(),
            last_cursor: initial_cursor,
            last_keys_snapshot: initial_keys,
        }));

        let handle_inner = inner.clone();
        let handle = thread::spawn(move || {
            let device = DeviceState::new();
            loop {
                thread::sleep(poll_interval);
                let cursor = device.get_mouse().coords;
                let keys = device.get_keys();
                let mut guard = handle_inner.lock().expect("activity mutex poisoned");
                let moved = cursor != guard.last_cursor;
                let keys_changed = keys != guard.last_keys_snapshot;
                if moved || keys_changed {
                    guard.last_activity = Instant::now();
                    guard.last_cursor = cursor;
                    guard.last_keys_snapshot = keys;
                }
            }
        });

        (Self { inner }, handle)
    }

    pub fn last_activity(&self) -> Instant {
        self.inner
            .lock()
            .expect("activity mutex poisoned")
            .last_activity
    }

    pub fn idle_duration(&self) -> Duration {
        self.last_activity().elapsed()
    }

    pub fn is_active_within(&self, window: Duration) -> bool {
        self.idle_duration() <= window
    }

    /// Build an ActivityMonitor that always reports "active right now"
    /// without spawning the polling task. Used by tests and the smoke
    /// binary, where there's no real input to observe but we want the
    /// active-check gate to pass.
    pub fn fake_always_active() -> Self {
        let inner = Arc::new(Mutex::new(Inner {
            last_activity: Instant::now(),
            last_cursor: (0, 0),
            last_keys_snapshot: Vec::new(),
        }));
        Self { inner }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn idle_duration_grows_over_time() {
        let (monitor, _handle) = ActivityMonitor::start(Duration::from_secs(60));
        let first = monitor.idle_duration();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let second = monitor.idle_duration();
        assert!(second >= first);
    }
}
