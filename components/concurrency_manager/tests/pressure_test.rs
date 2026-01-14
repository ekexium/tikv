// Copyright 2026 TiKV Project Authors. Licensed under Apache-2.0.

//! Pressure Test for ConcurrencyManager Memory Leak Investigation
//!
//! This test simulates TiKV's actual usage patterns to reproduce the memory leak
//! observed in production on ARM machines.
//!
//! # Test Scenarios
//!
//! 1. **scheduler_pattern**: Simulates TiKV scheduler - acquire locks, hold briefly, release
//! 2. **high_churn**: Maximum lock/unlock throughput to stress epoch GC
//! 3. **concurrent_readers**: Simulates read_key_check and global_min_lock_ts calls
//! 4. **idle_after_load**: Load followed by idle period to test GC when QPS=0
//! 5. **long_running**: Extended test to detect slow memory growth
//!
//! # How to Run
//!
//! ```bash
//! # Quick test (default parameters)
//! cargo test -p concurrency_manager --test pressure_test -- --ignored --nocapture 2>&1 | tee pressure_test.log
//!
//! # Extended test for leak detection
//! DURATION_SECS=300 THREADS=16 cargo test -p concurrency_manager --test pressure_test -- --ignored --nocapture 2>&1 | tee pressure_test.log
//!
//! # Test specific scenario
//! cargo test -p concurrency_manager --test pressure_test scheduler_pattern -- --ignored --nocapture
//! ```
//!
//! # Environment Variables
//!
//! - `DURATION_SECS`: Test duration in seconds (default: 60)
//! - `THREADS`: Number of worker threads (default: 8)
//! - `KEYS_PER_BATCH`: Keys per batch operation (default: 1000)
//! - `KEY_SPACE`: Total key space size (default: 1_000_000)
//! - `REPORT_INTERVAL_SECS`: Memory report interval (default: 5)
//!
//! # Interpreting Results
//!
//! Look for:
//! - **Steady growth in `allocated`**: Indicates memory leak
//! - **`allocated` returns to baseline after idle**: Normal behavior
//! - **Large gap between `allocated` and `resident`**: jemalloc retention (normal)
//! - **KeyHandle creates >> drops**: Arc reference leak (H1)

use std::{
    env,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use concurrency_manager::{ConcurrencyManager, KeyHandleGuard};
use crossbeam::epoch;
use futures::executor::block_on;
use rand::prelude::*;
use tikv_alloc::fetch_stats;
use txn_types::{Key, Lock, LockType, TimeStamp};

// Configuration from environment
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

// Statistics tracking
struct Stats {
    lock_acquires: AtomicU64,
    lock_releases: AtomicU64,
    read_checks: AtomicU64,
    epoch_flushes: AtomicU64,
}

impl Stats {
    fn new() -> Self {
        Self {
            lock_acquires: AtomicU64::new(0),
            lock_releases: AtomicU64::new(0),
            read_checks: AtomicU64::new(0),
            epoch_flushes: AtomicU64::new(0),
        }
    }

    fn report(&self, label: &str) {
        let acquires = self.lock_acquires.load(Ordering::Relaxed);
        let releases = self.lock_releases.load(Ordering::Relaxed);
        let reads = self.read_checks.load(Ordering::Relaxed);
        let flushes = self.epoch_flushes.load(Ordering::Relaxed);
        println!(
            "[{label}] acquires={acquires} releases={releases} pending={} reads={reads} flushes={flushes}",
            acquires.saturating_sub(releases)
        );
    }
}

fn print_memory(label: &str) {
    if let Ok(Some(stats)) = fetch_stats() {
        let get = |name: &str| stats.iter().find(|(k, _)| *k == name).map(|(_, v)| *v);
        let allocated = get("allocated").unwrap_or(0);
        let active = get("active").unwrap_or(0);
        let resident = get("resident").unwrap_or(0);
        let retained = get("retained").unwrap_or(0);
        println!(
            "[MEMORY {label}] allocated={} active={} resident={} retained={}",
            format_bytes(allocated),
            format_bytes(active),
            format_bytes(resident),
            format_bytes(retained)
        );
    } else {
        println!("[MEMORY {label}] stats unavailable");
    }
}

fn format_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2}GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.2}MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.2}KB", bytes as f64 / 1024.0)
    } else {
        format!("{}B", bytes)
    }
}

fn make_lock(primary: &[u8], ts: u64) -> Lock {
    Lock::new(
        LockType::Put,
        primary.to_vec(),
        TimeStamp::new(ts),
        1000,
        None,
        TimeStamp::new(ts),
        1,
        TimeStamp::new(ts + 10),
        false,
    )
}

/// Simulates TiKV scheduler pattern:
/// - Acquire lock on key
/// - Hold for short duration (simulating command processing)
/// - Release lock
///
/// This is the primary pattern used by prewrite/commit commands.
/// Each thread uses sequential non-overlapping keys to avoid deadlocks
/// (similar to how transactions operate on unique keys in TiKV).
#[test]
#[ignore]
fn scheduler_pattern() {
    let duration = Duration::from_secs(env_u64("DURATION_SECS", 60));
    let threads = env_usize("THREADS", 8);
    let keys_per_batch = env_usize("KEYS_PER_BATCH", 100);
    let report_interval = Duration::from_secs(env_u64("REPORT_INTERVAL_SECS", 5));

    println!("=== Scheduler Pattern Test ===");
    println!(
        "duration={}s threads={} keys_per_batch={}",
        duration.as_secs(),
        threads,
        keys_per_batch,
    );

    let cm = Arc::new(ConcurrencyManager::new(1.into()));
    let stats = Arc::new(Stats::new());
    let stop = Arc::new(AtomicBool::new(false));

    print_memory("before");

    // Spawn worker threads - each uses sequential keys with thread-specific offset
    let mut handles = Vec::new();
    for tid in 0..threads {
        let cm = cm.clone();
        let stats = stats.clone();
        let stop = stop.clone();

        let handle = thread::spawn(move || {
            // Each thread gets its own key counter to avoid any overlap
            let mut key_counter: u64 = (tid as u64) << 48; // High bits for thread ID
            let mut ts = tid as u64 * 1_000_000;

            while !stop.load(Ordering::Relaxed) {
                // Acquire batch of locks (simulating prewrite with multiple keys)
                let mut guards: Vec<KeyHandleGuard> = Vec::with_capacity(keys_per_batch);

                for _ in 0..keys_per_batch {
                    // Use sequential keys - each key is unique
                    key_counter += 1;
                    let key = Key::from_raw(&key_counter.to_be_bytes());
                    let guard = block_on(cm.lock_key(&key));
                    ts += 1;
                    let lock = make_lock(b"primary", ts);
                    guard.with_lock(|l| *l = Some(lock));
                    guards.push(guard);
                    stats.lock_acquires.fetch_add(1, Ordering::Relaxed);
                }

                // Simulate command processing time
                thread::sleep(Duration::from_micros(100));

                // Release all locks (simulating on_write_finished)
                let count = guards.len() as u64;
                drop(guards);
                stats.lock_releases.fetch_add(count, Ordering::Relaxed);

                // Occasional epoch flush
                if key_counter % 100 == 0 {
                    epoch::pin().flush();
                    stats.epoch_flushes.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        handles.push(handle);
    }

    // Reporter thread
    let stats_clone = stats.clone();
    let stop_clone = stop.clone();
    let reporter = thread::spawn(move || {
        let start = Instant::now();
        let mut report_num = 0;
        while !stop_clone.load(Ordering::Relaxed) {
            thread::sleep(report_interval);
            report_num += 1;
            let elapsed = start.elapsed().as_secs();
            print_memory(&format!("t={elapsed}s"));
            stats_clone.report(&format!("t={elapsed}s"));
        }
    });

    // Run for duration
    thread::sleep(duration);
    stop.store(true, Ordering::Release);

    // Wait for threads
    for h in handles {
        h.join().unwrap();
    }
    reporter.join().unwrap();

    print_memory("after_stop");
    stats.report("final");

    // Flush epoch and check memory recovery
    println!("\n=== Post-test GC ===");
    for i in 0..10 {
        for _ in 0..100 {
            epoch::pin().flush();
        }
        thread::sleep(Duration::from_millis(100));
        print_memory(&format!("flush_round_{i}"));
    }
}

/// High churn test - maximum lock/unlock throughput
/// Tests if epoch GC can keep up with high garbage creation rate
/// Each thread uses sequential unique keys
#[test]
#[ignore]
fn high_churn() {
    let duration = Duration::from_secs(env_u64("DURATION_SECS", 60));
    let threads = env_usize("THREADS", 8);
    let report_interval = Duration::from_secs(env_u64("REPORT_INTERVAL_SECS", 5));

    println!("=== High Churn Test ===");
    println!(
        "duration={}s threads={}",
        duration.as_secs(),
        threads,
    );

    let cm = Arc::new(ConcurrencyManager::new(1.into()));
    let stats = Arc::new(Stats::new());
    let stop = Arc::new(AtomicBool::new(false));

    print_memory("before");

    let mut handles = Vec::new();
    for tid in 0..threads {
        let cm = cm.clone();
        let stats = stats.clone();
        let stop = stop.clone();

        let handle = thread::spawn(move || {
            let mut key_counter: u64 = (tid as u64) << 48;
            let mut ts = tid as u64 * 1_000_000;

            while !stop.load(Ordering::Relaxed) {
                key_counter += 1;
                let key = Key::from_raw(&key_counter.to_be_bytes());
                let guard = block_on(cm.lock_key(&key));
                ts += 1;
                let lock = make_lock(b"p", ts);
                guard.with_lock(|l| *l = Some(lock));
                stats.lock_acquires.fetch_add(1, Ordering::Relaxed);

                // Immediate release - no hold time
                drop(guard);
                stats.lock_releases.fetch_add(1, Ordering::Relaxed);
            }
        });
        handles.push(handle);
    }

    // Reporter
    let stats_clone = stats.clone();
    let stop_clone = stop.clone();
    let reporter = thread::spawn(move || {
        let start = Instant::now();
        while !stop_clone.load(Ordering::Relaxed) {
            thread::sleep(report_interval);
            let elapsed = start.elapsed().as_secs();
            print_memory(&format!("t={elapsed}s"));
            stats_clone.report(&format!("t={elapsed}s"));
        }
    });

    thread::sleep(duration);
    stop.store(true, Ordering::Release);

    for h in handles {
        h.join().unwrap();
    }
    reporter.join().unwrap();

    print_memory("after_stop");
    stats.report("final");

    // Post-test GC
    println!("\n=== Post-test GC ===");
    for i in 0..10 {
        for _ in 0..100 {
            epoch::pin().flush();
        }
        thread::sleep(Duration::from_millis(100));
        print_memory(&format!("flush_round_{i}"));
    }
}

/// Tests concurrent readers calling global_min_lock_ts() and read_key_check()
/// These upgrade Weak to Arc temporarily - testing for potential leaks
#[test]
#[ignore]
fn concurrent_readers() {
    let duration = Duration::from_secs(env_u64("DURATION_SECS", 60));
    let writer_threads = env_usize("THREADS", 4);
    let reader_threads = env_usize("READER_THREADS", 4);
    let report_interval = Duration::from_secs(env_u64("REPORT_INTERVAL_SECS", 5));

    println!("=== Concurrent Readers Test ===");
    println!(
        "duration={}s writers={} readers={}",
        duration.as_secs(),
        writer_threads,
        reader_threads,
    );

    let cm = Arc::new(ConcurrencyManager::new(1.into()));
    let stats = Arc::new(Stats::new());
    let stop = Arc::new(AtomicBool::new(false));

    print_memory("before");

    let mut handles = Vec::new();

    // Writer threads - each with sequential unique keys
    for tid in 0..writer_threads {
        let cm = cm.clone();
        let stats = stats.clone();
        let stop = stop.clone();

        let handle = thread::spawn(move || {
            let mut key_counter: u64 = (tid as u64) << 48;
            let mut ts = tid as u64 * 1_000_000;

            while !stop.load(Ordering::Relaxed) {
                key_counter += 1;
                let key = Key::from_raw(&key_counter.to_be_bytes());
                let guard = block_on(cm.lock_key(&key));
                ts += 1;
                let lock = make_lock(b"p", ts);
                guard.with_lock(|l| *l = Some(lock));
                stats.lock_acquires.fetch_add(1, Ordering::Relaxed);

                // Hold briefly
                thread::sleep(Duration::from_micros(50));

                drop(guard);
                stats.lock_releases.fetch_add(1, Ordering::Relaxed);
            }
        });
        handles.push(handle);
    }

    // Reader threads - call global_min_lock_ts() and read_key_check()
    for tid in 0..reader_threads {
        let cm = cm.clone();
        let stats = stats.clone();
        let stop = stop.clone();

        let handle = thread::spawn(move || {
            let mut rng = StdRng::seed_from_u64((writer_threads + tid) as u64);

            while !stop.load(Ordering::Relaxed) {
                // Simulate resolved-ts calling global_min_lock_ts()
                let _ = cm.global_min_lock_ts();
                stats.read_checks.fetch_add(1, Ordering::Relaxed);

                // Simulate read requests calling read_key_check() on random keys
                for _ in 0..10 {
                    let key_id: u64 = rng.gen();
                    let key = Key::from_raw(&key_id.to_be_bytes());
                    let _ = cm.read_key_check(&key, |_lock| -> Result<(), ()> { Ok(()) });
                    stats.read_checks.fetch_add(1, Ordering::Relaxed);
                }

                thread::sleep(Duration::from_micros(100));
            }
        });
        handles.push(handle);
    }

    // Reporter
    let stats_clone = stats.clone();
    let stop_clone = stop.clone();
    let reporter = thread::spawn(move || {
        let start = Instant::now();
        while !stop_clone.load(Ordering::Relaxed) {
            thread::sleep(report_interval);
            let elapsed = start.elapsed().as_secs();
            print_memory(&format!("t={elapsed}s"));
            stats_clone.report(&format!("t={elapsed}s"));
        }
    });

    thread::sleep(duration);
    stop.store(true, Ordering::Release);

    for h in handles {
        h.join().unwrap();
    }
    reporter.join().unwrap();

    print_memory("after_stop");
    stats.report("final");

    // Post-test GC
    println!("\n=== Post-test GC ===");
    for i in 0..10 {
        for _ in 0..100 {
            epoch::pin().flush();
        }
        thread::sleep(Duration::from_millis(100));
        print_memory(&format!("flush_round_{i}"));
    }
}

/// Tests memory behavior after load stops (simulating leader eviction)
/// Key test: Does memory return to baseline when QPS=0?
#[test]
#[ignore]
fn idle_after_load() {
    let load_duration = Duration::from_secs(env_u64("LOAD_DURATION_SECS", 30));
    let idle_duration = Duration::from_secs(env_u64("IDLE_DURATION_SECS", 60));
    let threads = env_usize("THREADS", 8);

    println!("=== Idle After Load Test ===");
    println!(
        "load_duration={}s idle_duration={}s threads={}",
        load_duration.as_secs(),
        idle_duration.as_secs(),
        threads,
    );

    let cm = Arc::new(ConcurrencyManager::new(1.into()));
    let stats = Arc::new(Stats::new());
    let stop = Arc::new(AtomicBool::new(false));

    print_memory("before_load");

    // Load phase - each thread uses sequential unique keys
    let mut handles = Vec::new();
    for tid in 0..threads {
        let cm = cm.clone();
        let stats = stats.clone();
        let stop = stop.clone();

        let handle = thread::spawn(move || {
            let mut key_counter: u64 = (tid as u64) << 48;
            let mut ts = tid as u64 * 1_000_000;

            while !stop.load(Ordering::Relaxed) {
                key_counter += 1;
                let key = Key::from_raw(&key_counter.to_be_bytes());
                let guard = block_on(cm.lock_key(&key));
                ts += 1;
                let lock = make_lock(b"p", ts);
                guard.with_lock(|l| *l = Some(lock));
                stats.lock_acquires.fetch_add(1, Ordering::Relaxed);

                drop(guard);
                stats.lock_releases.fetch_add(1, Ordering::Relaxed);
            }
        });
        handles.push(handle);
    }

    // Run load
    let start = Instant::now();
    while start.elapsed() < load_duration {
        thread::sleep(Duration::from_secs(5));
        let elapsed = start.elapsed().as_secs();
        print_memory(&format!("load_t={elapsed}s"));
        stats.report(&format!("load_t={elapsed}s"));
    }

    // Stop load
    stop.store(true, Ordering::Release);
    for h in handles {
        h.join().unwrap();
    }

    print_memory("after_load_stop");
    stats.report("after_load_stop");

    // Idle phase - simulate leader eviction (QPS = 0)
    println!("\n=== Idle Phase (simulating leader eviction) ===");
    println!("Checking if memory returns to baseline with no activity...\n");

    let idle_start = Instant::now();
    while idle_start.elapsed() < idle_duration {
        // Periodic epoch flush (simulating background activity)
        for _ in 0..10 {
            epoch::pin().flush();
        }

        thread::sleep(Duration::from_secs(5));
        let elapsed = idle_start.elapsed().as_secs();
        print_memory(&format!("idle_t={elapsed}s"));

        // Check min_lock_ts (should be None when no locks)
        let min_ts = cm.global_min_lock_ts();
        println!("[idle_t={elapsed}s] global_min_lock_ts={:?}", min_ts);
    }

    print_memory("final");
}

/// Long-running test to detect slow memory growth
#[test]
#[ignore]
fn long_running() {
    let duration = Duration::from_secs(env_u64("DURATION_SECS", 300));
    let threads = env_usize("THREADS", 8);
    let report_interval = Duration::from_secs(env_u64("REPORT_INTERVAL_SECS", 10));

    println!("=== Long Running Test ===");
    println!(
        "duration={}s threads={} report_interval={}s",
        duration.as_secs(),
        threads,
        report_interval.as_secs()
    );

    let cm = Arc::new(ConcurrencyManager::new(1.into()));
    let stats = Arc::new(Stats::new());
    let stop = Arc::new(AtomicBool::new(false));

    // Track memory growth
    let mut memory_samples: Vec<(u64, usize)> = Vec::new();

    print_memory("before");

    let mut handles = Vec::new();
    for tid in 0..threads {
        let cm = cm.clone();
        let stats = stats.clone();
        let stop = stop.clone();

        let handle = thread::spawn(move || {
            let mut key_counter: u64 = (tid as u64) << 48;
            let mut ts = tid as u64 * 1_000_000;
            let mut rng = StdRng::seed_from_u64(tid as u64);

            while !stop.load(Ordering::Relaxed) {
                // Mix of batch and single operations
                if rng.gen_bool(0.3) {
                    // Batch (like prewrite)
                    let batch_size = rng.gen_range(10..100);
                    let mut guards: Vec<KeyHandleGuard> = Vec::with_capacity(batch_size);

                    for _ in 0..batch_size {
                        key_counter += 1;
                        let key = Key::from_raw(&key_counter.to_be_bytes());
                        let guard = block_on(cm.lock_key(&key));
                        ts += 1;
                        let lock = make_lock(b"p", ts);
                        guard.with_lock(|l| *l = Some(lock));
                        guards.push(guard);
                        stats.lock_acquires.fetch_add(1, Ordering::Relaxed);
                    }

                    thread::sleep(Duration::from_micros(rng.gen_range(50..200)));

                    let count = guards.len() as u64;
                    drop(guards);
                    stats.lock_releases.fetch_add(count, Ordering::Relaxed);
                } else {
                    // Single key
                    key_counter += 1;
                    let key = Key::from_raw(&key_counter.to_be_bytes());
                    let guard = block_on(cm.lock_key(&key));
                    ts += 1;
                    let lock = make_lock(b"p", ts);
                    guard.with_lock(|l| *l = Some(lock));
                    stats.lock_acquires.fetch_add(1, Ordering::Relaxed);

                    drop(guard);
                    stats.lock_releases.fetch_add(1, Ordering::Relaxed);
                }

                // Occasional read check (like resolved-ts)
                if key_counter % 100 == 0 {
                    let _ = cm.global_min_lock_ts();
                    stats.read_checks.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        handles.push(handle);
    }

    // Reporter with memory tracking
    let stats_clone = stats.clone();
    let stop_clone = stop.clone();
    let reporter = thread::spawn(move || {
        let start = Instant::now();
        let mut samples = Vec::new();

        while !stop_clone.load(Ordering::Relaxed) {
            thread::sleep(report_interval);
            let elapsed = start.elapsed().as_secs();

            if let Ok(Some(mem_stats)) = fetch_stats() {
                let allocated = mem_stats
                    .iter()
                    .find(|(k, _)| *k == "allocated")
                    .map(|(_, v)| *v)
                    .unwrap_or(0);
                samples.push((elapsed, allocated));
                print_memory(&format!("t={elapsed}s"));
                stats_clone.report(&format!("t={elapsed}s"));
            }
        }

        samples
    });

    thread::sleep(duration);
    stop.store(true, Ordering::Release);

    for h in handles {
        h.join().unwrap();
    }
    memory_samples = reporter.join().unwrap();

    print_memory("after_stop");
    stats.report("final");

    // Analyze memory growth
    println!("\n=== Memory Growth Analysis ===");
    if memory_samples.len() >= 2 {
        let first = memory_samples.first().unwrap();
        let last = memory_samples.last().unwrap();
        let growth = last.1 as i64 - first.1 as i64;
        let growth_rate = growth as f64 / (last.0 - first.0) as f64;

        println!(
            "First sample: t={}s allocated={}",
            first.0,
            format_bytes(first.1)
        );
        println!(
            "Last sample: t={}s allocated={}",
            last.0,
            format_bytes(last.1)
        );
        println!(
            "Total growth: {} ({:.2} bytes/sec)",
            format_bytes(growth.unsigned_abs() as usize),
            growth_rate
        );

        if growth > 10 * 1024 * 1024 {
            // > 10MB growth
            println!("WARNING: Significant memory growth detected!");
        } else {
            println!("Memory growth within acceptable range");
        }
    }

    // Post-test GC
    println!("\n=== Post-test GC ===");
    for i in 0..10 {
        for _ in 0..100 {
            epoch::pin().flush();
        }
        thread::sleep(Duration::from_millis(100));
        print_memory(&format!("flush_round_{i}"));
    }
}

/// Run all scenarios sequentially
#[test]
#[ignore]
fn all_scenarios() {
    println!("\n========================================");
    println!("Running all pressure test scenarios");
    println!("========================================\n");

    // Use shorter durations for combined test
    env::set_var("DURATION_SECS", "30");
    env::set_var("LOAD_DURATION_SECS", "15");
    env::set_var("IDLE_DURATION_SECS", "30");

    scheduler_pattern();
    println!("\n");

    high_churn();
    println!("\n");

    concurrent_readers();
    println!("\n");

    idle_after_load();
}
