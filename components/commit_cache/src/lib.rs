// Copyright 2023 TiKV Project Authors. Licensed under Apache-2.0.
#![feature(once_cell)]

use std::sync::LazyLock;

/// a global cache mapping start_ts to commit_ts
use quick_cache::sync::Cache;

const CAPACITY: usize = 1024 * 100;

pub static COMMIT_CACHE: LazyLock<CommitCache> = LazyLock::new(|| CommitCache::new(CAPACITY));

pub struct CommitCache {
    cache: Cache<u64, u64>, // start_ts -> commit_ts
}

impl CommitCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            cache: Cache::new(capacity),
        }
    }

    pub fn insert(&self, start_ts: u64, commit_ts: u64) {
        self.cache.insert(start_ts, commit_ts);
    }

    pub fn remove(&self, start_ts: u64) {
        self.cache.remove(&start_ts);
    }

    pub fn get(&self, start_ts: u64) -> Option<u64> {
        self.cache.get(&start_ts)
    }
}
