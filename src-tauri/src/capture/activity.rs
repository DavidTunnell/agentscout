use device_query::{DeviceQuery, DeviceState};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

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
        let state = DeviceState::new();
        let initial_cursor = state.get_mouse().coords;
        let initial_keys = state.get_keys();

        let inner = Arc::new(Mutex::new(Inner {
            last_activity: Instant::now(),
            last_cursor: initial_cursor,
            last_keys_snapshot: initial_keys,
        }));

        let handle_inner = inner.clone();
        let handle = tokio::spawn(async move {
            let device = DeviceState::new();
            let mut interval = tokio::time::interval(poll_interval);
            interval.tick().await;
            loop {
                interval.tick().await;
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
