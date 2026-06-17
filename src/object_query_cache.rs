use std::collections::HashMap;
use std::sync::Mutex;

use crate::object_resolver::ResolvedRuntimeObject;

/// Bounded LRU cache for resolved runtime objects.
/// Keyed by canonical object ref string (e.g. "message:abc123").
pub struct ObjectQueryCache {
    inner: Mutex<CacheInner>,
    capacity: usize,
}

struct CacheInner {
    entries: HashMap<String, CacheEntry>,
    order: Vec<String>, // LRU order: front = oldest, back = newest
}

struct CacheEntry {
    value: ResolvedRuntimeObject,
}

impl ObjectQueryCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(CacheInner {
                entries: HashMap::with_capacity(capacity),
                order: Vec::with_capacity(capacity),
            }),
            capacity,
        }
    }

    pub fn get(&self, key: &str) -> Option<ResolvedRuntimeObject> {
        let mut inner = self.inner.lock().ok()?;
        let cloned = inner.entries.get(key).map(|e| e.value.clone());
        if cloned.is_some() {
            if let Some(pos) = inner.order.iter().position(|k| k.as_str() == key) {
                let _ = inner.order.remove(pos);
            }
            inner.order.push(key.to_string());
            crate::diagnostics::record_object_query_cache_hit();
            cloned
        } else {
            crate::diagnostics::record_object_query_cache_miss();
            None
        }
    }

    pub fn insert(&self, key: String, value: ResolvedRuntimeObject) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if inner.entries.len() >= self.capacity && !inner.entries.contains_key(&key) {
            // Evict oldest
            if let Some(oldest) = inner.order.first().cloned() {
                inner.entries.remove(&oldest);
                inner.order.remove(0);
            }
        }
        if !inner.order.iter().any(|k| k == &key) {
            inner.order.push(key.clone());
        }
        inner.entries.insert(key, CacheEntry { value });
    }

    pub fn clear(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.entries.clear();
            inner.order.clear();
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.entries.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
    };

    fn sample_message() -> ResolvedRuntimeObject {
        let msg = MessageEnvelope::new(
            "agent-a",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "test".into(),
            },
        );
        ResolvedRuntimeObject::Message(msg)
    }

    #[test]
    fn cache_hit_returns_inserted_value() {
        let cache = ObjectQueryCache::new(10);
        let obj = sample_message();
        cache.insert("message:abc".to_string(), obj.clone());
        let result = cache.get("message:abc");
        assert_eq!(result, Some(obj));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_miss_returns_none() {
        let cache = ObjectQueryCache::new(10);
        assert!(cache.get("message:missing").is_none());
    }

    #[test]
    fn cache_evicts_oldest_when_full() {
        let cache = ObjectQueryCache::new(2);
        cache.insert("a".to_string(), sample_message());
        cache.insert("b".to_string(), sample_message());
        cache.insert("c".to_string(), sample_message());
        // "a" should be evicted
        assert!(cache.get("a").is_none());
        assert!(cache.get("b").is_some());
        assert!(cache.get("c").is_some());
    }

    #[test]
    fn cache_lru_promotes_on_access() {
        let cache = ObjectQueryCache::new(2);
        cache.insert("a".to_string(), sample_message());
        cache.insert("b".to_string(), sample_message());
        // Access "a" to promote it
        let _ = cache.get("a");
        // Now insert "c" - "b" should be evicted (oldest after promotion)
        cache.insert("c".to_string(), sample_message());
        assert!(cache.get("a").is_some());
        assert!(cache.get("b").is_none());
        assert!(cache.get("c").is_some());
    }

    #[test]
    fn cache_clear_removes_all_entries() {
        let cache = ObjectQueryCache::new(10);
        cache.insert("a".to_string(), sample_message());
        cache.insert("b".to_string(), sample_message());
        assert_eq!(cache.len(), 2);
        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.get("a").is_none());
    }

    #[test]
    fn cache_insert_overwrites_existing_key() {
        let cache = ObjectQueryCache::new(10);
        let obj1 = sample_message();
        cache.insert("key".to_string(), obj1.clone());
        let obj2 = sample_message();
        cache.insert("key".to_string(), obj2.clone());
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get("key"), Some(obj2));
    }
}
