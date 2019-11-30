use std::collections::VecDeque;
use std::time::*;

pub struct TimedEntry<K, V> {
    key: K,
    value: V,
    timestamp: Instant,
}

pub struct TimeoutMap<K, V> {
    // By not using an HashMap I avoid deriving Eq, Hash, Copy and Clone for K
    // A VecDeque is used because elements are inserted an removed almost always as FIFO.
    buffer: VecDeque<TimedEntry<K, V>>,
    timeout: Duration,
}

impl<K, V> TimeoutMap<K, V> {
    pub fn new(timeout: Duration) -> Self {
        Self {
            buffer: VecDeque::new(),
            timeout,
        }
    }

    pub fn insert(&mut self, key: K, value: V) {
        self.buffer.push_back(TimedEntry {
            key,
            value,
            timestamp: Instant::now(),
        });
    }

    pub fn remove_any(&mut self) -> Option<(K, V)> {
        self.buffer
            .pop_front()
            .map(|TimedEntry { key, value, .. }| (key, value))
    }

    pub fn remove_expired(&mut self) -> Vec<V> {
        let max_time = Instant::now() - self.timeout;

        let idx_to_be_removed: Vec<_> = self
            .buffer
            .iter()
            .enumerate()
            .filter(|(_, TimedEntry { timestamp, .. })| *timestamp > max_time)
            .map(|(i, _)| i)
            .collect();

        idx_to_be_removed
            .iter()
            .map(|i| self.buffer.remove(*i).unwrap().value)
            .collect()
    }
}

impl<K: PartialEq, V> TimeoutMap<K, V> {
    pub fn remove(&mut self, key: &K) -> Option<V> {
        // front to back iterator
        let entry_ref = self
            .buffer
            .iter()
            .enumerate()
            .find(|(_, TimedEntry { key: k, .. })| *k == *key);

        if let Some((idx, _)) = entry_ref {
            self.buffer
                .remove(idx)
                .map(|TimedEntry { value, .. }| value)
        } else {
            None
        }
    }
}