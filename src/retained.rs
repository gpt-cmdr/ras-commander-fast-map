//! Byte-bounded least-recently-used immutable session state.
use std::collections::VecDeque;
use std::sync::Arc;

pub(crate) struct Retained<K, V> {
    entries: VecDeque<(K, Arc<V>, u64)>,
    bytes: u64,
    limit: u64,
}

impl<K: PartialEq, V> Default for Retained<K, V> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            limit: 0,
        }
    }
}

impl<K: PartialEq, V> Retained<K, V> {
    pub fn resize(&mut self, limit: u64) {
        let previous = self.limit;
        self.limit = limit;
        if limit == 0 {
            self.clear();
            return;
        }
        while self.bytes > limit {
            self.evict();
        }
        if limit < previous {
            self.entries.shrink_to_fit();
        }
    }
    fn evict(&mut self) {
        if let Some((_, _, bytes)) = self.entries.pop_front() {
            self.bytes -= bytes;
        }
    }
    pub fn clear(&mut self) {
        self.entries = VecDeque::new();
        self.bytes = 0;
    }
    pub fn get(&mut self, key: &K) -> Option<Arc<V>> {
        let index = self.entries.iter().position(|(k, _, _)| k == key)?;
        let entry = self.entries.remove(index).unwrap();
        let value = entry.1.clone();
        self.entries.push_back(entry);
        Some(value)
    }
    pub fn insert(&mut self, key: K, value: Arc<V>, bytes: u64) {
        if bytes > self.limit || self.limit == 0 {
            return;
        }
        if let Some(index) = self.entries.iter().position(|(k, _, _)| k == &key) {
            self.bytes -= self.entries.remove(index).unwrap().2;
        }
        while self.bytes > self.limit - bytes {
            self.evict();
        }
        if self.entries.capacity() > self.entries.len().saturating_mul(2) + 8 {
            self.entries.shrink_to_fit();
        }
        self.entries.push_back((key, value, bytes));
        self.bytes += bytes;
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lru_hits_replacements_resize_and_oversize() {
        let mut c = Retained::default();
        c.resize(10);
        c.insert(1, Arc::new(1), 4);
        c.insert(2, Arc::new(2), 4);
        assert_eq!(*c.get(&1).unwrap(), 1);
        c.insert(3, Arc::new(3), 4);
        assert!(c.get(&2).is_none());
        c.insert(1, Arc::new(11), 5);
        assert_eq!(c.bytes(), 9);
        c.insert(4, Arc::new(4), 11);
        assert_eq!(c.len(), 2);
        c.resize(5);
        assert!(c.get(&3).is_none());
        assert_eq!(*c.get(&1).unwrap(), 11);
        c.resize(0);
        assert_eq!(c.len(), 0);
        assert_eq!(c.entries.capacity(), 0);
    }
}
