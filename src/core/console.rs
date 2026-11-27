use std::collections::VecDeque;

use chrono::Local;
use parking_lot::Mutex;
use serde::Serialize;

const DEFAULT_CAPACITY: usize = 6000;

/// Single console entry formatted identically to `core/console_service.py`.
#[derive(Debug, Clone, Serialize)]
pub struct ConsoleEntry {
    pub id: u64,
    pub ts: String,
    pub stream: String,
    pub text: String,
}

/// Thread-safe ring buffer console backing `poll_console` / `clear_console`.
pub struct RingConsole {
    inner: Mutex<RingState>,
}

struct RingState {
    capacity: usize,
    next_id: u64,
    entries: VecDeque<ConsoleEntry>,
}

impl RingConsole {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(RingState {
                capacity: capacity.max(100),
                next_id: 1,
                entries: VecDeque::with_capacity(capacity.min(6000)),
            }),
        }
    }

    /// Push a single log line (splitting multi-line strings safely).
    pub fn log(&self, stream: &str, msg: impl AsRef<str>) {
        let raw = msg.as_ref().replace("\r\n", "\n");
        let ts = Local::now().format("%H:%M:%S").to_string();
        let mut st = self.inner.lock();
        for line in raw.split('\n') {
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            let id = st.next_id;
            st.next_id += 1;
            if st.entries.len() >= st.capacity {
                st.entries.pop_front();
            }
            st.entries.push_back(ConsoleEntry {
                id,
                ts: ts.clone(),
                stream: stream.to_string(),
                text: trimmed.to_string(),
            });
        }
    }

    pub fn info(&self, msg: impl AsRef<str>) {
        self.log("stdout", msg);
    }

    pub fn warn(&self, msg: impl AsRef<str>) {
        self.log("stderr", msg);
    }

    /// Return entries with `id > last_id` up to `limit`, plus `latest_id`.
    pub fn poll(&self, last_id: u64, limit: usize) -> (Vec<ConsoleEntry>, u64) {
        let st = self.inner.lock();
        let latest_id = st.next_id.saturating_sub(1);
        let max_items = limit.clamp(1, 1000);
        let items: Vec<ConsoleEntry> = st
            .entries
            .iter()
            .filter(|e| e.id > last_id)
            .take(max_items)
            .cloned()
            .collect();
        (items, latest_id)
    }

    pub fn clear(&self) -> u64 {
        let mut st = self.inner.lock();
        st.entries.clear();
        st.next_id.saturating_sub(1)
    }
}

impl Default for RingConsole {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_respects_capacity_and_poll() {
        let c = RingConsole::new(100);
        for i in 1..=120 {
            c.info(format!("line {i}"));
        }
        let (all, latest) = c.poll(0, 500);
        assert_eq!(latest, 120);
        assert_eq!(all.len(), 100);
        assert_eq!(all.first().unwrap().id, 21);

        let (tail, latest2) = c.poll(115, 50);
        assert_eq!(latest2, 120);
        assert_eq!(tail.len(), 5);
        assert_eq!(tail[0].text, "line 116");
        let cursor = c.clear();
        assert_eq!(cursor, latest);
        c.info("after clear");
        let (after, latest) = c.poll(cursor, 10);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].text, "after clear");
        assert_eq!(latest, cursor + 1);
    }
}
