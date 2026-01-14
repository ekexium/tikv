// Copyright 2025 TiKV Project Authors. Licensed under Apache-2.0.

use std::{
    collections::VecDeque,
    env,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use concurrency_manager::LockTable;
use crossbeam_skiplist::{
    base::{NODE_ALLOCS, NODE_DEALLOCS, NODE_DEFERRED},
    SkipMap,
};
use rand::{rngs::StdRng, RngCore, SeedableRng};
use txn_types::{Key, Lock, LockType};

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u32(name: &str, default: u32) -> u32 {
    env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn default_threads() -> usize {
    thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

fn bytes_to_mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[derive(Default)]
struct JemallocStats {
    allocated: u64,
    active: u64,
    resident: u64,
    retained: u64,
}

fn fetch_jemalloc_stats() -> Option<JemallocStats> {
    let stats = tikv_alloc::fetch_stats().ok().flatten()?;
    let mut out = JemallocStats::default();
    for (name, value) in stats {
        let value = value as u64;
        match name {
            "allocated" => out.allocated = value,
            "active" => out.active = value,
            "resident" => out.resident = value,
            "retained" => out.retained = value,
            _ => {}
        }
    }
    Some(out)
}

fn report_stats(tag: &str, start: Instant, _ops: u64, len: usize) {
    let elapsed = start.elapsed().as_secs_f64();
    let allocs = NODE_ALLOCS.load(Ordering::Relaxed);
    let deallocs = NODE_DEALLOCS.load(Ordering::Relaxed);
    let deferred = NODE_DEFERRED.load(Ordering::Relaxed);
    let leak = allocs.saturating_sub(deallocs);
    if let Some(stats) = fetch_jemalloc_stats() {
        println!(
            "tag={tag} t={elapsed:.0} len={len} allocs={allocs} def={deferred} leak={leak} mb={:.0}",
            bytes_to_mb(stats.allocated),
        );
    } else {
        println!("tag={tag} t={elapsed:.0} len={len} allocs={allocs} def={deferred} leak={leak}");
    }
}

fn fill_key(buf: &mut [u8], key_id: u64, rng: &mut StdRng) {
    let prefix = key_id.to_le_bytes();
    let prefix_len = prefix.len().min(buf.len());
    buf[..prefix_len].copy_from_slice(&prefix[..prefix_len]);
    if buf.len() > prefix_len {
        rng.fill_bytes(&mut buf[prefix_len..]);
    }
}

// cargo test -p concurrency_manager --test pressure --release -- --ignored
// --nocapture
#[test]
#[ignore]
fn stress_lock_table_churn() {
    let threads = env_usize(
        "CM_STRESS_THREADS",
        env_usize("STRESS_THREADS", default_threads()),
    );
    let tasks = env_usize(
        "CM_STRESS_TASKS",
        env_usize("STRESS_TASKS", threads.saturating_mul(4)),
    );
    let ops_per_task = env_u64("CM_STRESS_OPS_PER_TASK", 200_000);
    let key_space = env_u64("CM_STRESS_KEY_SPACE", 1_000_000).max(1);
    let key_len = env_usize("CM_STRESS_KEY_LEN", 32).max(8);
    let read_ratio = env_u32("CM_STRESS_READ_RATIO", 40).min(100);
    let scan_ratio = env_u32("CM_STRESS_SCAN_RATIO", 5).min(100);
    let range_ratio = env_u32("CM_STRESS_RANGE_RATIO", 5).min(100);
    let set_lock_ratio = env_u32("CM_STRESS_SET_LOCK_RATIO", 80).min(100);
    let hold_us = env_u64("CM_STRESS_HOLD_US", 500);
    let stash_guards = env_usize("CM_STRESS_STASH_GUARDS", 64);
    let duration_secs = env_u64(
        "CM_STRESS_DURATION_SECS",
        env_u64("STRESS_DURATION_SECS", 60),
    );
    let report_interval_ms = env_u64(
        "CM_STRESS_REPORT_INTERVAL_MS",
        env_u64("STRESS_REPORT_INTERVAL_MS", 1000),
    );
    let idle_secs = env_u64("CM_STRESS_IDLE_SECS", env_u64("STRESS_IDLE_SECS", 20));
    let idle_gc = env_bool("CM_STRESS_IDLE_GC", env_bool("STRESS_IDLE_GC", false));
    let assert_empty = env_bool("CM_STRESS_ASSERT_EMPTY", false);

    let lock_table = Arc::new(LockTable::default());
    let ops = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let start = Instant::now();

    println!(
        "config test=stress_lock_table_churn threads={threads} tasks={tasks} ops_per_task={ops_per_task} key_space={key_space} key_len={key_len} read_ratio={read_ratio} scan_ratio={scan_ratio} range_ratio={range_ratio} set_lock_ratio={set_lock_ratio} hold_us={hold_us} stash_guards={stash_guards} duration_secs={duration_secs} report_interval_ms={report_interval_ms} idle_secs={idle_secs} idle_gc={idle_gc} assert_empty={assert_empty}"
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let monitor = if report_interval_ms > 0 {
            let lock_table = lock_table.clone();
            let ops = ops.clone();
            let stop = stop.clone();
            Some(tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(report_interval_ms));
                loop {
                    interval.tick().await;
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    report_stats(
                        "run",
                        start,
                        ops.load(Ordering::Relaxed),
                        lock_table.0.len(),
                    );
                }
            }))
        } else {
            None
        };

        let deadline = if duration_secs > 0 {
            Some(Instant::now() + Duration::from_secs(duration_secs))
        } else {
            None
        };

        let mut handles = Vec::with_capacity(tasks);
        for task_id in 0..tasks {
            let lock_table = lock_table.clone();
            let ops = ops.clone();
            let deadline = deadline;
            let handle = tokio::spawn(async move {
                let mut rng = StdRng::seed_from_u64(0xC0FFEE_u64 ^ task_id as u64);
                let mut buf = vec![0u8; key_len];
                let mut stash = VecDeque::new();
                let mut remaining = ops_per_task;
                while deadline.map_or(remaining > 0, |d| Instant::now() < d) {
                    let key_id = rng.next_u64() % key_space;
                    fill_key(&mut buf, key_id, &mut rng);
                    let key = Key::from_raw(&buf);
                    let is_read = rng.next_u32() % 100 < read_ratio;
                    if is_read {
                        let read_roll = rng.next_u32() % 100;
                        if read_roll < scan_ratio {
                            lock_table.for_each(|handle| {
                                let _ = handle.with_lock(|lock| lock.as_ref().map(|l| l.ts));
                            });
                        } else if read_roll < scan_ratio.saturating_add(range_ratio) {
                            let _ =
                                lock_table.check_range(Some(&key), None, |_, _| Ok::<(), ()>(()));
                        } else {
                            let _ = lock_table.get(&key);
                        }
                    } else {
                        let guard = lock_table.lock_key(&key).await;
                        if set_lock_ratio > 0 && rng.next_u32() % 100 < set_lock_ratio {
                            let lock = Lock::new(
                                LockType::Put,
                                buf.to_vec(),
                                10.into(),
                                1000,
                                None,
                                10.into(),
                                1,
                                20.into(),
                                false,
                            );
                            guard.with_lock(|l| {
                                *l = Some(lock);
                            });
                        }
                        if hold_us > 0 {
                            tokio::time::sleep(Duration::from_micros(hold_us)).await;
                        }
                        if stash_guards > 0 {
                            stash.push_back(guard);
                            if stash.len() > stash_guards {
                                stash.pop_front();
                            }
                        } else {
                            drop(guard);
                        }
                    }
                    ops.fetch_add(1, Ordering::Relaxed);
                    if deadline.is_none() {
                        remaining = remaining.saturating_sub(1);
                    }
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.await.unwrap();
        }

        stop.store(true, Ordering::Relaxed);
        if let Some(monitor) = monitor {
            let _ = monitor.await;
        }

        report_stats(
            "workload_done",
            start,
            ops.load(Ordering::Relaxed),
            lock_table.0.len(),
        );

        if idle_secs > 0 {
            let gc_key = Key::from_raw(b"__gc__");
            for _ in 0..idle_secs {
                if idle_gc {
                    let _ = lock_table.0.get(&gc_key);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                report_stats(
                    "idle",
                    start,
                    ops.load(Ordering::Relaxed),
                    lock_table.0.len(),
                );
            }
        }
    });

    let final_len = lock_table.0.len();
    report_stats("done", start, ops.load(Ordering::Relaxed), final_len);
    if assert_empty {
        assert_eq!(final_len, 0, "lock table should be empty after churn");
    }
}

// cargo test -p concurrency_manager --test pressure --release -- --ignored
// --nocapture
#[test]
#[ignore]
fn stress_skipmap_churn() {
    let threads = env_usize(
        "SKIPMAP_STRESS_THREADS",
        env_usize("STRESS_THREADS", default_threads()),
    );
    let ops_per_thread = env_u64("SKIPMAP_STRESS_OPS_PER_THREAD", 1_000_000);
    let key_space = env_u64("SKIPMAP_STRESS_KEY_SPACE", 1_000_000).max(1);
    let insert_ratio = env_u32("SKIPMAP_STRESS_INSERT_RATIO", 50).min(100);
    let duration_secs = env_u64(
        "SKIPMAP_STRESS_DURATION_SECS",
        env_u64("STRESS_DURATION_SECS", 60),
    );
    let report_interval_ms = env_u64(
        "SKIPMAP_STRESS_REPORT_INTERVAL_MS",
        env_u64("STRESS_REPORT_INTERVAL_MS", 1000),
    );
    let idle_secs = env_u64("SKIPMAP_STRESS_IDLE_SECS", env_u64("STRESS_IDLE_SECS", 20));
    let idle_gc = env_bool("SKIPMAP_STRESS_IDLE_GC", env_bool("STRESS_IDLE_GC", false));

    let map = Arc::new(SkipMap::<u64, u64>::new());
    let ops = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let start = Instant::now();

    println!(
        "config test=stress_skipmap_churn threads={threads} ops_per_thread={ops_per_thread} key_space={key_space} insert_ratio={insert_ratio} duration_secs={duration_secs} report_interval_ms={report_interval_ms} idle_secs={idle_secs} idle_gc={idle_gc}"
    );

    let monitor = if report_interval_ms > 0 {
        let map = map.clone();
        let ops = ops.clone();
        let stop = stop.clone();
        Some(thread::spawn(move || {
            let interval = Duration::from_millis(report_interval_ms);
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(interval);
                report_stats("run", start, ops.load(Ordering::Relaxed), map.len());
            }
        }))
    } else {
        None
    };

    let mut handles = Vec::with_capacity(threads);
    let deadline = if duration_secs > 0 {
        Some(Instant::now() + Duration::from_secs(duration_secs))
    } else {
        None
    };
    for thread_id in 0..threads {
        let map = map.clone();
        let ops = ops.clone();
        let deadline = deadline;
        let handle = thread::spawn(move || {
            let mut rng = StdRng::seed_from_u64(0x1234_5678_u64 ^ thread_id as u64);
            let mut remaining = ops_per_thread;
            while deadline.map_or(remaining > 0, |d| Instant::now() < d) {
                let key = rng.next_u64() % key_space;
                let do_insert = rng.next_u32() % 100 < insert_ratio;
                if do_insert {
                    map.insert(key, key ^ 0xDEAD_BEEF);
                } else {
                    let _ = map.remove(&key);
                }
                ops.fetch_add(1, Ordering::Relaxed);
                if deadline.is_none() {
                    remaining = remaining.saturating_sub(1);
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    stop.store(true, Ordering::Relaxed);
    if let Some(monitor) = monitor {
        let _ = monitor.join();
    }

    report_stats(
        "workload_done",
        start,
        ops.load(Ordering::Relaxed),
        map.len(),
    );

    if idle_secs > 0 {
        for _ in 0..idle_secs {
            if idle_gc {
                let _ = map.get(&0);
            }
            thread::sleep(Duration::from_secs(1));
            report_stats("idle", start, ops.load(Ordering::Relaxed), map.len());
        }
    }

    report_stats("done", start, ops.load(Ordering::Relaxed), map.len());
}

#[test]
#[ignore]
fn stress_skipmap_range_iter() {
    let threads = env_usize("STRESS_THREADS", default_threads());
    let duration_secs = env_u64("STRESS_DURATION_SECS", 120);
    let idle_secs = env_u64("STRESS_IDLE_SECS", 10);
    let idle_gc = env_bool("STRESS_IDLE_GC", true);
    let key_space = 10000_u64;

    let map = Arc::new(SkipMap::<u64, u64>::new());
    let ops = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let start = Instant::now();

    println!(
        "config test=stress_skipmap_range_iter threads={threads} key_space={key_space} duration_secs={duration_secs} idle_secs={idle_secs}"
    );

    let monitor = {
        let map = map.clone();
        let ops = ops.clone();
        let stop = stop.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs(1));
                report_stats("run", start, ops.load(Ordering::Relaxed), map.len());
            }
        })
    };

    let deadline = Instant::now() + Duration::from_secs(duration_secs);
    let mut handles = Vec::with_capacity(threads);
    for thread_id in 0..threads {
        let map = map.clone();
        let ops = ops.clone();
        let handle = thread::spawn(move || {
            let mut rng = StdRng::seed_from_u64(thread_id as u64);
            while Instant::now() < deadline {
                for i in 0..key_space {
                    map.insert(i, i);
                }
                let start_key = rng.next_u64() % key_space;
                let end_key = (start_key + 100).min(key_space);
                let mut count = 0u64;
                for entry in map.range(start_key..end_key) {
                    count = count.wrapping_add(*entry.value());
                }
                std::hint::black_box(count);
                for i in 0..key_space {
                    map.remove(&i);
                }
                ops.fetch_add(1, Ordering::Relaxed);
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    stop.store(true, Ordering::Relaxed);
    let _ = monitor.join();

    report_stats(
        "workload_done",
        start,
        ops.load(Ordering::Relaxed),
        map.len(),
    );

    if idle_secs > 0 {
        for _ in 0..idle_secs {
            if idle_gc {
                let _ = map.get(&0);
            }
            thread::sleep(Duration::from_secs(1));
            report_stats("idle", start, ops.load(Ordering::Relaxed), map.len());
        }
    }

    report_stats("done", start, ops.load(Ordering::Relaxed), map.len());
}
