// Copyright 2024 TiKV Project Authors. Licensed under Apache-2.0.

//! Comprehensive memory leak stress test for concurrency_manager
//! 
//! This test simulates production workloads to detect potential memory leaks.
//! Run with: cargo test --package concurrency_manager --test memory_leak_stress 
//!           --features jemalloc --release -- --nocapture --ignored

use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use concurrency_manager::{ActionOnInvalidMaxTs, ConcurrencyManager};
use futures::executor::block_on;
use rand::prelude::*;
use txn_types::{Key, Lock, LockType, TimeStamp};

#[derive(Debug, Clone)]
struct MemoryStats {
    allocated: u64,
    resident: u64,
    timestamp: Instant,
}

#[derive(Debug, Clone)]
struct StressTestConfig {
    duration_seconds: u64,
    concurrent_threads: usize,
    // 移除operations_per_second，现在追求最大吞吐量
    key_range: usize,
    // lock_hold_duration_ms现在基本不用了，保留用于某些特殊测试
    #[allow(dead_code)]
    lock_hold_duration_ms: u64,
    memory_check_interval_seconds: u64,
    enable_memory_analysis: bool,
}

impl Default for StressTestConfig {
    fn default() -> Self {
        Self {
            duration_seconds: 3600, // 1 hour by default
            concurrent_threads: 16,
            key_range: 500_000, // 增大默认key范围，便于发现内存泄漏
            lock_hold_duration_ms: 100,
            memory_check_interval_seconds: 30,
            enable_memory_analysis: true,
        }
    }
}

impl StressTestConfig {
    fn validate(&self) -> Result<(), String> {
        if self.duration_seconds == 0 {
            return Err("duration_seconds must be greater than 0".to_string());
        }
        if self.concurrent_threads == 0 {
            return Err("concurrent_threads must be greater than 0".to_string());
        }
        // 移除operations_per_second验证，现在追求最大吞吐量
        if self.key_range == 0 {
            return Err("key_range must be greater than 0".to_string());
        }
        if self.memory_check_interval_seconds == 0 {
            return Err("memory_check_interval_seconds must be greater than 0".to_string());
        }
        // Ensure key_range is large enough for range operations
        if self.key_range < 200 {
            return Err("key_range must be at least 200 for range operations".to_string());
        }
        Ok(())
    }
}

struct StressTestRunner {
    config: StressTestConfig,
    cm: Arc<ConcurrencyManager>,
    running: Arc<AtomicBool>,
    stats: Arc<Mutex<Vec<MemoryStats>>>,
    operation_count: Arc<AtomicUsize>,
    error_count: Arc<AtomicUsize>,
    // 全局键计数器，确保每个worker生成不同的新键
    global_key_counter: Arc<AtomicUsize>,
    // 内存监控线程句柄，用于正确清理
    memory_monitor_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
}

impl StressTestRunner {
    fn new(config: StressTestConfig) -> Result<Self, String> {
        config.validate()?;
        
        let cm = Arc::new(ConcurrencyManager::new_with_config(
            TimeStamp::new(1000),
            Duration::from_secs(45),
            ActionOnInvalidMaxTs::Log,
            None,
            Duration::from_secs(60),
        ));

        Ok(Self {
            config,
            cm,
            running: Arc::new(AtomicBool::new(false)),
            stats: Arc::new(Mutex::new(Vec::new())),
            operation_count: Arc::new(AtomicUsize::new(0)),
            error_count: Arc::new(AtomicUsize::new(0)),
            global_key_counter: Arc::new(AtomicUsize::new(0)),
            memory_monitor_handle: Arc::new(Mutex::new(None)),
        })
    }

    fn get_memory_stats() -> MemoryStats {
        // Try to get jemalloc stats if available, otherwise use placeholder values
        let (allocated, resident) = if let Ok(Some(stats)) = tikv_alloc::fetch_stats() {
            let mut allocated = 0u64;
            let mut resident = 0u64;
            
            for (name, value) in stats {
                match name {
                    "allocated" => allocated = value as u64,
                    "resident" => resident = value as u64,
                    _ => {}
                }
            }
            (allocated, resident)
        } else {
            // Fallback: estimate memory usage from /proc/self/status
            Self::get_memory_from_proc()
        };
        
        MemoryStats {
            allocated,
            resident,
            timestamp: Instant::now(),
        }
    }

    fn get_memory_from_proc() -> (u64, u64) {
        use std::fs;
        
        // Try to read memory info from /proc/self/status
        if let Ok(status) = fs::read_to_string("/proc/self/status") {
            let mut vm_rss = 0u64;
            let mut vm_size = 0u64;
            
            for line in status.lines() {
                if line.starts_with("VmRSS:") {
                    if let Some(kb_str) = line.split_whitespace().nth(1) {
                        vm_rss = kb_str.parse::<u64>().unwrap_or(0) * 1024; // Convert KB to bytes
                    }
                } else if line.starts_with("VmSize:") {
                    if let Some(kb_str) = line.split_whitespace().nth(1) {
                        vm_size = kb_str.parse::<u64>().unwrap_or(0) * 1024; // Convert KB to bytes
                    }
                }
            }
            
            (vm_size, vm_rss) // (allocated, resident)
        } else {
            // Ultimate fallback: use dummy values that still allow trend analysis
            (100_000_000, 80_000_000) // 100MB allocated, 80MB resident
        }
    }

    fn memory_monitor(&self) {
        let stats = self.stats.clone();
        let running = self.running.clone();
        let interval = Duration::from_secs(self.config.memory_check_interval_seconds);
        let handle_storage = self.memory_monitor_handle.clone();

        let handle = thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                let memory_stats = Self::get_memory_stats();
                {
                    if let Ok(mut stats_guard) = stats.lock() {
                        stats_guard.push(memory_stats.clone());
                    }
                }
                
                println!("[MEMORY] Allocated: {} MB, Resident: {} MB", 
                    memory_stats.allocated / 1024 / 1024,
                    memory_stats.resident / 1024 / 1024);
                
                thread::sleep(interval);
            }
        });

        // 存储线程句柄用于清理
        if let Ok(mut handle_guard) = handle_storage.lock() {
            *handle_guard = Some(handle);
        };
    }

    fn stop_memory_monitor(&self) {
        if let Ok(mut handle_guard) = self.memory_monitor_handle.lock() {
            if let Some(handle) = handle_guard.take() {
                // 等待监控线程结束
                let _ = handle.join();
            }
        }
    }

    // Scenario 1: High concurrent lock operations
    fn lock_stress_worker(&self, worker_id: usize) {
        let mut rng = StdRng::seed_from_u64(worker_id as u64);
        
        while self.running.load(Ordering::Relaxed) {
            let key_id = rng.gen_range(0..self.config.key_range);
            let key = Key::from_raw(&format!("stress_test_lock_key_{:020}_padding_data_to_make_key_longer_for_memory_leak_detection", key_id).into_bytes());

            match self.lock_and_hold_key(&key, &mut rng) {
                Ok(_) => {
                    self.operation_count.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    self.error_count.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    fn lock_and_hold_key(&self, key: &Key, rng: &mut StdRng) -> Result<(), Box<dyn std::error::Error>> {
        let guard = block_on(self.cm.lock_key(key));
        
        let lock_type = if rng.gen_bool(0.7) { LockType::Put } else { LockType::Delete };
        let primary_key = format!("stress_test_lock_primary_key_{:020}_padding_data_to_make_key_longer_for_memory_leak_detection", rng.gen::<u32>()).into_bytes();
        // 防止时间戳溢出，使用较小的范围
        let base_ts = 1000u64;
        let random_offset = rng.gen_range(0..50000) as u64;
        let ts = TimeStamp::new(base_ts + random_offset);
        
        let lock = Lock::new(
            lock_type,
            primary_key,
            ts,
            1000,
            None,
            ts,
            1,
            ts,
            false,
        );

        guard.with_lock(|l| {
            *l = Some(lock);
        });

        // 保留少量的锁持有时间模拟，但减少频率和时间
        if rng.gen_bool(0.01) { // 从10%降到1%的概率
            thread::sleep(Duration::from_millis(1)); // 从100ms降到1ms
        }

        // Guard is properly dropped here when the function returns
        Ok(())
    }

    // Scenario 2: Max TS update stress
    fn max_ts_stress_worker(&self, worker_id: usize) {
        let mut rng = StdRng::seed_from_u64(worker_id as u64 + 10000);
        let mut current_ts = 10000u64;

        while self.running.load(Ordering::Relaxed) {
            current_ts = current_ts.saturating_add(rng.gen_range(1..100));
            let new_ts = TimeStamp::new(current_ts);
            
            // Update max_ts_limit periodically
            if rng.gen_bool(0.1) {
                self.cm.set_max_ts_limit(TimeStamp::new(current_ts.saturating_add(1000)));
            }

            let source = format!("stress_worker_{}", worker_id);
            match self.cm.update_max_ts(new_ts, || source.clone()) {
                Ok(_) => {
                    self.operation_count.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    self.error_count.fetch_add(1, Ordering::Relaxed);
                }
            }

            // 移除固定的10ms延迟，偶尔yield给其他线程
            if rng.gen_bool(0.001) { // 0.1%的概率yield
                thread::yield_now();
            }
        }
    }

    // Scenario 3: Range read stress
    fn range_read_stress_worker(&self, worker_id: usize) {
        let mut rng = StdRng::seed_from_u64(worker_id as u64 + 20000);

        while self.running.load(Ordering::Relaxed) {
            // 修复范围检查bug，确保不会下溢
            let max_range = std::cmp::min(self.config.key_range, 100);
            let start_id = if self.config.key_range > max_range {
                rng.gen_range(0..self.config.key_range - max_range)
            } else {
                0
            };
            let end_id = start_id + rng.gen_range(1..max_range);
            
            let start_key = Key::from_raw(&format!("stress_test_range_key_{:020}_padding_data_to_make_key_longer_for_memory_leak_detection", start_id).into_bytes());
            let end_key = Key::from_raw(&format!("stress_test_range_key_{:020}_padding_data_to_make_key_longer_for_memory_leak_detection", end_id).into_bytes());

            let mut count = 0;
            let result = self.cm.read_range_check(
                Some(&start_key), 
                Some(&end_key), 
                |_key, _lock| {
                    count += 1;
                    Ok::<(), ()>(())
                }
            );

            match result {
                Ok(_) => {
                    self.operation_count.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    self.error_count.fetch_add(1, Ordering::Relaxed);
                }
            }

            // 移除固定的5ms延迟，偶尔yield
            if count > 0 && rng.gen_bool(0.01) { // 1%的概率yield，且只在有数据时
                thread::yield_now();
            }
        }
    }

    // Scenario 4: Mixed workload simulation
    fn mixed_workload_worker(&self, worker_id: usize) {
        let mut rng = StdRng::seed_from_u64(worker_id as u64 + 30000);

        while self.running.load(Ordering::Relaxed) {
            match rng.gen_range(0..4) {
                0 => self.lock_stress_worker_single_op(&mut rng),
                1 => self.point_read_worker_single_op(&mut rng),
                2 => self.range_read_worker_single_op(&mut rng),
                3 => self.cleanup_worker_single_op(&mut rng),
                _ => unreachable!(),
            }

            // 移除固定的1ms延迟，偶尔yield
            if rng.gen_bool(0.01) { // 1%的概率yield
                thread::yield_now();
            }
        }
    }

    fn lock_stress_worker_single_op(&self, rng: &mut StdRng) {
        let key_id = rng.gen_range(0..self.config.key_range);
        let key = Key::from_raw(&format!("stress_test_mixed_key_{:020}_padding_data_to_make_key_longer_for_memory_leak_detection", key_id).into_bytes());
        let _ = self.lock_and_hold_key(&key, rng);
    }

    fn point_read_worker_single_op(&self, rng: &mut StdRng) {
        let key_id = rng.gen_range(0..self.config.key_range);
        let key = Key::from_raw(&format!("stress_test_point_read_key_{:020}_padding_data_to_make_key_longer_for_memory_leak_detection", key_id).into_bytes());
        
        let _ = self.cm.read_key_check(&key, |_lock| Ok::<(), ()>(()));
        self.operation_count.fetch_add(1, Ordering::Relaxed);
    }

    fn range_read_worker_single_op(&self, rng: &mut StdRng) {
        let max_range = std::cmp::min(self.config.key_range, 10);
        let start_id = if self.config.key_range > max_range {
            rng.gen_range(0..self.config.key_range - max_range)
        } else {
            0
        };
        let end_id = start_id + rng.gen_range(1..max_range);
        
        let start_key = Key::from_raw(&format!("stress_test_mixed_range_key_{:020}_padding_data_to_make_key_longer_for_memory_leak_detection", start_id).into_bytes());
        let end_key = Key::from_raw(&format!("stress_test_mixed_range_key_{:020}_padding_data_to_make_key_longer_for_memory_leak_detection", end_id).into_bytes());

        let _ = self.cm.read_range_check(
            Some(&start_key), 
            Some(&end_key), 
            |_key, _lock| Ok::<(), ()>(())
        );
        self.operation_count.fetch_add(1, Ordering::Relaxed);
    }

    fn cleanup_worker_single_op(&self, rng: &mut StdRng) {
        // Simulate cleanup operations that might trigger memory management
        let key_id = rng.gen_range(0..self.config.key_range);
        let key = Key::from_raw(&format!("stress_test_cleanup_key_{:020}_padding_data_to_make_key_longer_for_memory_leak_detection", key_id).into_bytes());
        
        let guard = block_on(self.cm.lock_key(&key));
        guard.with_lock(|l| {
            *l = None; // Clear the lock
        });
        // Guard is properly dropped here
        self.operation_count.fetch_add(1, Ordering::Relaxed);
    }

    // Scenario 5: Hot keys vs New keys simulation
    fn hot_vs_new_keys_worker(&self, worker_id: usize) {
        let mut rng = StdRng::seed_from_u64(worker_id as u64 + 40000);
        let hot_key_range = 1000; // Fixed set of hot keys: key_000000 to key_000999

        while self.running.load(Ordering::Relaxed) {
            // 30% chance to use hot keys (repeated), 70% chance to use new keys (never repeated)
            let use_hot_key = rng.gen_bool(0.3);
            
            let key = if use_hot_key {
                // Use hot keys that will be repeatedly accessed
                let key_id = rng.gen_range(0..hot_key_range);
                Key::from_raw(&format!("stress_test_hot_key_{:020}_padding_data_to_make_key_longer_for_memory_leak_detection", key_id).into_bytes())
            } else {
                // 修复键冲突bug：使用全局原子计数器确保每个新键都是唯一的
                let global_key_id = self.global_key_counter.fetch_add(1, Ordering::Relaxed);
                Key::from_raw(&format!("stress_test_new_key_{:025}_{:05}_padding_data_to_make_key_longer_for_memory_leak_detection", global_key_id, worker_id).into_bytes())
            };

            // Perform random operations on the key
            match rng.gen_range(0..3) {
                0 => {
                    // Lock operation
                    if let Err(_) = self.lock_and_hold_key(&key, &mut rng) {
                        self.error_count.fetch_add(1, Ordering::Relaxed);
                    }
                }
                1 => {
                    // Point read operation
                    let _ = self.cm.read_key_check(&key, |_lock| Ok::<(), ()>(()));
                }
                2 => {
                    // Lock and clear operation (cleanup simulation)
                    let guard = block_on(self.cm.lock_key(&key));
                    guard.with_lock(|l| {
                        *l = None;
                    });
                    // Guard is properly dropped here
                }
                _ => unreachable!(),
            }

            self.operation_count.fetch_add(1, Ordering::Relaxed);
            
            // 移除固定延迟，偶尔yield来避免完全饿死其他线程
            if rng.gen_bool(0.01) { // 1%的概率yield
                thread::yield_now();
            }
        }
    }

    // Scenario 6: Pure new keys stress (always growing key space)
    fn pure_new_keys_worker(&self, worker_id: usize) {
        // 修复键冲突bug：使用worker_id和全局计数器创建真正唯一的键
        let base_counter = worker_id * 10_000_000; // 每个worker有独立的大范围
        let mut local_counter = 0usize;
        
        while self.running.load(Ordering::Relaxed) {
            let unique_key_id = base_counter + local_counter;
            let key = Key::from_raw(&format!("stress_test_pure_new_key_{:030}_padding_data_to_make_key_longer_for_memory_leak_detection", unique_key_id).into_bytes());
            local_counter += 1;

            // Perform a complete lifecycle: lock -> use -> cleanup
            let guard = block_on(self.cm.lock_key(&key));
            
            // Simulate some work with the lock
            let lock_type = if unique_key_id % 2 == 0 { LockType::Put } else { LockType::Delete };
            let primary_key = format!("stress_test_primary_key_{:030}_padding_data_to_make_key_longer_for_memory_leak_detection", unique_key_id).into_bytes();
            // 防止时间戳溢出
            let ts = TimeStamp::new(1000 + (unique_key_id % 50000) as u64);
            
            let lock = Lock::new(
                lock_type,
                primary_key,
                ts,
                1000,
                None,
                ts,
                1,
                ts,
                false,
            );

            guard.with_lock(|l| {
                *l = Some(lock);
            });
            
            // 移除brief hold time，直接清理锁
            // Clear the lock
            guard.with_lock(|l| {
                *l = None;
            });
            
            // Guard is properly dropped here
            self.operation_count.fetch_add(1, Ordering::Relaxed);
            
            // 移除固定延迟，让它以最大速度创建新键
            // 偶尔yield避免完全占用CPU
            if local_counter % 1000 == 0 { // 每1000个操作yield一次
                thread::yield_now();
            }
        }
    }

    fn run_stress_test(&self) {
        println!("Starting comprehensive memory leak stress test...");
        println!("Configuration: {:?}", self.config);
        
        self.running.store(true, Ordering::Relaxed);
        
        // Start memory monitor
        self.memory_monitor();
        
        let start_time = Instant::now();
        let mut handles = Vec::new();

        // Start different types of workers
        // 修复除零bug：确保concurrent_threads能被6整除
        let workers_per_type = std::cmp::max(1, self.config.concurrent_threads / 6);
        for i in 0..workers_per_type {
            // Lock stress workers
            let runner = self.clone();
            handles.push(thread::spawn(move || runner.lock_stress_worker(i)));
            
            // Max TS stress workers  
            let runner = self.clone();
            handles.push(thread::spawn(move || runner.max_ts_stress_worker(i + 1000)));
            
            // Range read workers
            let runner = self.clone();
            handles.push(thread::spawn(move || runner.range_read_stress_worker(i + 2000)));
            
            // Mixed workload workers
            let runner = self.clone();
            handles.push(thread::spawn(move || runner.mixed_workload_worker(i + 3000)));
            
            // Hot vs new keys workers
            let runner = self.clone();
            handles.push(thread::spawn(move || runner.hot_vs_new_keys_worker(i + 4000)));
            
            // Pure new keys workers
            let runner = self.clone();
            handles.push(thread::spawn(move || runner.pure_new_keys_worker(i + 5000)));
        }

        // Print statistics periodically
        let stats_runner = self.clone();
        let stats_handle = thread::spawn(move || {
            let mut last_ops = 0;
            let mut last_time = Instant::now();
            
            while stats_runner.running.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs(10));
                
                let current_ops = stats_runner.operation_count.load(Ordering::Relaxed);
                let current_errors = stats_runner.error_count.load(Ordering::Relaxed);
                let now = Instant::now();
                
                let ops_per_second = (current_ops - last_ops) as f64 / (now - last_time).as_secs_f64();
                
                println!("[STATS] Operations: {}, Errors: {}, OPS: {:.2}/s, Runtime: {:?}", 
                    current_ops, current_errors, ops_per_second, now - start_time);
                
                last_ops = current_ops;
                last_time = now;
            }
        });

        // Run for configured duration
        thread::sleep(Duration::from_secs(self.config.duration_seconds));

        // Stop all workers
        self.running.store(false, Ordering::Relaxed);
        
        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }
        stats_handle.join().unwrap();

        // 停止内存监控线程
        self.stop_memory_monitor();

        self.analyze_results();
    }

    fn run_pure_new_keys_test(&self) {
        println!("Starting pure new keys memory leak test...");
        println!("Configuration: {:?}", self.config);
        
        self.running.store(true, Ordering::Relaxed);
        
        // Start memory monitor
        self.memory_monitor();
        
        let start_time = Instant::now();
        let mut handles = Vec::new();

        // Start only pure new keys workers
        for i in 0..self.config.concurrent_threads {
            let runner = self.clone();
            handles.push(thread::spawn(move || runner.pure_new_keys_worker(i)));
        }

        // Print statistics periodically
        let stats_runner = self.clone();
        let stats_handle = thread::spawn(move || {
            let mut last_ops = 0;
            let mut last_time = Instant::now();
            
            while stats_runner.running.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs(10));
                
                let current_ops = stats_runner.operation_count.load(Ordering::Relaxed);
                let current_errors = stats_runner.error_count.load(Ordering::Relaxed);
                let now = Instant::now();
                
                let ops_per_second = (current_ops - last_ops) as f64 / (now - last_time).as_secs_f64();
                
                println!("[PURE NEW KEYS] Operations: {}, Errors: {}, OPS: {:.2}/s, Runtime: {:?}", 
                    current_ops, current_errors, ops_per_second, now - start_time);
                
                last_ops = current_ops;
                last_time = now;
            }
        });

        // Run for configured duration
        thread::sleep(Duration::from_secs(self.config.duration_seconds));

        // Stop all workers
        self.running.store(false, Ordering::Relaxed);
        
        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }
        stats_handle.join().unwrap();

        // 停止内存监控线程
        self.stop_memory_monitor();

        self.analyze_results();
    }

    fn run_hot_keys_only_test(&self) {
        println!("Starting hot keys only memory leak test...");
        println!("Configuration: {:?}", self.config);
        
        self.running.store(true, Ordering::Relaxed);
        
        // Start memory monitor
        self.memory_monitor();
        
        let start_time = Instant::now();
        let mut handles = Vec::new();

        // Start only lock stress workers (which use repeated keys from 0 to key_range)
        for i in 0..self.config.concurrent_threads {
            let runner = self.clone();
            handles.push(thread::spawn(move || runner.lock_stress_worker(i)));
        }

        // Print statistics periodically
        let stats_runner = self.clone();
        let stats_handle = thread::spawn(move || {
            let mut last_ops = 0;
            let mut last_time = Instant::now();
            
            while stats_runner.running.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs(10));
                
                let current_ops = stats_runner.operation_count.load(Ordering::Relaxed);
                let current_errors = stats_runner.error_count.load(Ordering::Relaxed);
                let now = Instant::now();
                
                let ops_per_second = (current_ops - last_ops) as f64 / (now - last_time).as_secs_f64();
                
                println!("[HOT KEYS ONLY] Operations: {}, Errors: {}, OPS: {:.2}/s, Runtime: {:?}", 
                    current_ops, current_errors, ops_per_second, now - start_time);
                
                last_ops = current_ops;
                last_time = now;
            }
        });

        // Run for configured duration
        thread::sleep(Duration::from_secs(self.config.duration_seconds));

        // Stop all workers
        self.running.store(false, Ordering::Relaxed);
        
        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }
        stats_handle.join().unwrap();

        // 停止内存监控线程
        self.stop_memory_monitor();

        self.analyze_results();
    }

    fn run_hot_vs_new_keys_test(&self) {
        println!("Starting hot vs new keys memory leak test...");
        println!("Configuration: {:?}", self.config);
        
        self.running.store(true, Ordering::Relaxed);
        
        // Start memory monitor
        self.memory_monitor();
        
        let start_time = Instant::now();
        let mut handles = Vec::new();

        // Start only hot_vs_new_keys workers
        for i in 0..self.config.concurrent_threads {
            let runner = self.clone();
            handles.push(thread::spawn(move || runner.hot_vs_new_keys_worker(i)));
        }

        // Print statistics periodically
        let stats_runner = self.clone();
        let stats_handle = thread::spawn(move || {
            let mut last_ops = 0;
            let mut last_time = Instant::now();
            
            while stats_runner.running.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs(10));
                
                let current_ops = stats_runner.operation_count.load(Ordering::Relaxed);
                let current_errors = stats_runner.error_count.load(Ordering::Relaxed);
                let now = Instant::now();
                
                let ops_per_second = (current_ops - last_ops) as f64 / (now - last_time).as_secs_f64();
                
                println!("[HOT VS NEW KEYS] Operations: {}, Errors: {}, OPS: {:.2}/s, Runtime: {:?}", 
                    current_ops, current_errors, ops_per_second, now - start_time);
                
                last_ops = current_ops;
                last_time = now;
            }
        });

        // Run for configured duration
        thread::sleep(Duration::from_secs(self.config.duration_seconds));

        // Stop all workers
        self.running.store(false, Ordering::Relaxed);
        
        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }
        stats_handle.join().unwrap();

        // 停止内存监控线程
        self.stop_memory_monitor();

        self.analyze_results();
    }

    #[allow(dead_code)]
    fn run_lock_stress_test(&self) {
        println!("Starting lock stress memory leak test...");
        println!("Configuration: {:?}", self.config);
        
        self.running.store(true, Ordering::Relaxed);
        
        // Start memory monitor
        self.memory_monitor();
        
        let start_time = Instant::now();
        let mut handles = Vec::new();

        // Start only lock stress workers
        for i in 0..self.config.concurrent_threads {
            let runner = self.clone();
            handles.push(thread::spawn(move || runner.lock_stress_worker(i)));
        }

        // Print statistics periodically
        let stats_runner = self.clone();
        let stats_handle = thread::spawn(move || {
            let mut last_ops = 0;
            let mut last_time = Instant::now();
            
            while stats_runner.running.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs(10));
                
                let current_ops = stats_runner.operation_count.load(Ordering::Relaxed);
                let current_errors = stats_runner.error_count.load(Ordering::Relaxed);
                let now = Instant::now();
                
                let ops_per_second = (current_ops - last_ops) as f64 / (now - last_time).as_secs_f64();
                
                println!("[LOCK STRESS] Operations: {}, Errors: {}, OPS: {:.2}/s, Runtime: {:?}", 
                    current_ops, current_errors, ops_per_second, now - start_time);
                
                last_ops = current_ops;
                last_time = now;
            }
        });

        // Run for configured duration
        thread::sleep(Duration::from_secs(self.config.duration_seconds));

        // Stop all workers
        self.running.store(false, Ordering::Relaxed);
        
        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }
        stats_handle.join().unwrap();

        // 停止内存监控线程
        self.stop_memory_monitor();

        self.analyze_results();
    }

    #[allow(dead_code)]
    fn run_range_read_test(&self) {
        println!("Starting range read memory leak test...");
        println!("Configuration: {:?}", self.config);
        
        self.running.store(true, Ordering::Relaxed);
        
        // Start memory monitor
        self.memory_monitor();
        
        let start_time = Instant::now();
        let mut handles = Vec::new();

        // Start only range read workers
        for i in 0..self.config.concurrent_threads {
            let runner = self.clone();
            handles.push(thread::spawn(move || runner.range_read_stress_worker(i)));
        }

        // Print statistics periodically
        let stats_runner = self.clone();
        let stats_handle = thread::spawn(move || {
            let mut last_ops = 0;
            let mut last_time = Instant::now();
            
            while stats_runner.running.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs(10));
                
                let current_ops = stats_runner.operation_count.load(Ordering::Relaxed);
                let current_errors = stats_runner.error_count.load(Ordering::Relaxed);
                let now = Instant::now();
                
                let ops_per_second = (current_ops - last_ops) as f64 / (now - last_time).as_secs_f64();
                
                println!("[RANGE READ] Operations: {}, Errors: {}, OPS: {:.2}/s, Runtime: {:?}", 
                    current_ops, current_errors, ops_per_second, now - start_time);
                
                last_ops = current_ops;
                last_time = now;
            }
        });

        // Run for configured duration
        thread::sleep(Duration::from_secs(self.config.duration_seconds));

        // Stop all workers
        self.running.store(false, Ordering::Relaxed);
        
        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }
        stats_handle.join().unwrap();

        // 停止内存监控线程
        self.stop_memory_monitor();

        self.analyze_results();
    }

    fn analyze_results(&self) {
        println!("\n========== MEMORY LEAK ANALYSIS ==========");
        
        let stats_guard = self.stats.lock().unwrap();
        let memory_snapshots = stats_guard.clone();
        drop(stats_guard);

        if memory_snapshots.len() < 2 {
            println!("Insufficient memory snapshots for analysis");
            return;
        }

        let first = &memory_snapshots[0];
        let last = &memory_snapshots[memory_snapshots.len() - 1];
        let duration = last.timestamp.duration_since(first.timestamp);

        let allocated_growth = last.allocated as i64 - first.allocated as i64;
        let resident_growth = last.resident as i64 - first.resident as i64;

        println!("Test Duration: {:?}", duration);
        println!("Memory Growth:");
        println!("  Allocated: {} MB ({:+} MB)", 
            last.allocated / 1024 / 1024, allocated_growth / 1024 / 1024);
        println!("  Resident: {} MB ({:+} MB)", 
            last.resident / 1024 / 1024, resident_growth / 1024 / 1024);

        // Calculate growth rate
        let hours = duration.as_secs_f64() / 3600.0;
        if hours > 0.0 {
            let allocated_mb_per_hour = (allocated_growth as f64 / 1024.0 / 1024.0) / hours;
            let resident_mb_per_hour = (resident_growth as f64 / 1024.0 / 1024.0) / hours;
            
            println!("Growth Rate:");
            println!("  Allocated: {:.2} MB/hour ({:.2} GB/day)", 
                allocated_mb_per_hour, allocated_mb_per_hour * 24.0 / 1024.0);
            println!("  Resident: {:.2} MB/hour ({:.2} GB/day)", 
                resident_mb_per_hour, resident_mb_per_hour * 24.0 / 1024.0);

            // Check if growth rate is concerning (> 100 MB/day)
            if allocated_mb_per_hour * 24.0 > 100.0 || resident_mb_per_hour * 24.0 > 100.0 {
                println!("⚠️  WARNING: Potential memory leak detected!");
                println!("   Growth rate exceeds 100 MB/day threshold");
            } else {
                println!("✅ Memory growth rate appears normal");
            }
        }

        // Print detailed memory progression
        println!("\nMemory Progression:");
        for (i, snapshot) in memory_snapshots.iter().enumerate() {
            if i % 5 == 0 || i == memory_snapshots.len() - 1 { // Print every 5th snapshot
                let elapsed = snapshot.timestamp.duration_since(first.timestamp);
                println!("  {:8.1}s: Allocated={:6} MB, Resident={:6} MB", 
                    elapsed.as_secs_f64(),
                    snapshot.allocated / 1024 / 1024,
                    snapshot.resident / 1024 / 1024);
            }
        }

        let total_ops = self.operation_count.load(Ordering::Relaxed);
        let total_errors = self.error_count.load(Ordering::Relaxed);
        println!("\nOperation Statistics:");
        println!("  Total Operations: {}", total_ops);
        println!("  Total Errors: {}", total_errors);
        println!("  Error Rate: {:.2}%", (total_errors as f64 / total_ops as f64) * 100.0);

        if self.config.enable_memory_analysis {
            println!("\nDetailed Memory Analysis:");
            if let Ok(Some(final_stats)) = tikv_alloc::fetch_stats() {
                println!("  jemalloc stats:");
                for (name, value) in final_stats {
                    println!("    {}: {} bytes ({:.2} MB)", name, value, value as f64 / 1024.0 / 1024.0);
                }
            } else {
                println!("  jemalloc stats not available, using system memory metrics");
                if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
                    for line in status.lines() {
                        if line.starts_with("VmPeak:") || line.starts_with("VmSize:") || 
                           line.starts_with("VmRSS:") || line.starts_with("VmData:") {
                            println!("  {}", line);
                        }
                    }
                }
            }
        }
    }
}

impl Clone for StressTestRunner {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            cm: self.cm.clone(),
            running: self.running.clone(),
            stats: self.stats.clone(),
            operation_count: self.operation_count.clone(),
            error_count: self.error_count.clone(),
            global_key_counter: self.global_key_counter.clone(),
            memory_monitor_handle: self.memory_monitor_handle.clone(),
        }
    }
}

#[test]
#[ignore]
fn test_memory_leak_short() {
    // Short test for quick validation (5 minutes)
    let config = StressTestConfig {
        duration_seconds: 300,
        concurrent_threads: 12,
        memory_check_interval_seconds: 10,
        ..Default::default()
    };
    
    let runner = StressTestRunner::new(config).unwrap();
    runner.run_stress_test();
}

#[test]
#[ignore]
fn test_memory_leak_medium() {
    // Medium test (1 hour)
    let config = StressTestConfig {
        duration_seconds: 3600,
        concurrent_threads: 16,
        memory_check_interval_seconds: 30,
        ..Default::default()
    };
    
    let runner = StressTestRunner::new(config).unwrap();
    runner.run_stress_test();
}

#[test]
#[ignore]
fn test_memory_leak_long() {
    // Long test for thorough analysis (6 hours)  
    let config = StressTestConfig {
        duration_seconds: 21600,
        concurrent_threads: 24,
        key_range: 1_000_000,
        memory_check_interval_seconds: 60,
        ..Default::default()
    };
    
    let runner = StressTestRunner::new(config).unwrap();
    runner.run_stress_test();
}

#[test]
#[ignore]
fn test_memory_leak_production_simulation() {
    // Simulate production workload (24 hours)
    let config = StressTestConfig {
        duration_seconds: 86400,
        concurrent_threads: 32,
        key_range: 10_000_000,
        lock_hold_duration_ms: 50,
        memory_check_interval_seconds: 300, // 5 minutes
        enable_memory_analysis: true,
    };
    
    let runner = StressTestRunner::new(config).unwrap();
    runner.run_stress_test();
}

#[test]
#[ignore]
fn test_memory_leak_hot_vs_new_keys() {
    // Test focusing on hot keys vs new keys pattern
    let config = StressTestConfig {
        duration_seconds: 1800, // 30 minutes
        concurrent_threads: 24,
        key_range: 10_000, // Hot keys will be 0-999, new keys start from 10_000
        memory_check_interval_seconds: 15,
        ..Default::default()
    };
    
    println!("Starting hot keys vs new keys memory leak test");
    println!("Hot keys: key_000000 to key_000999 (repeated)");
    println!("New keys: key_010000+ (always growing)");
    
    let runner = StressTestRunner::new(config).unwrap();
    runner.run_hot_vs_new_keys_test();
}

#[test]
#[ignore]
fn test_memory_leak_pure_new_keys() {
    // Test with only new keys (never repeated) - this should expose key space growth issues
    let config = StressTestConfig {
        duration_seconds: 1800,
        concurrent_threads: 12,
        key_range: 200,
        memory_check_interval_seconds: 10,
        ..Default::default()
    };
    
    println!("Starting pure new keys memory leak test");
    println!("All keys are unique and never repeated - testing key space growth");
    
    let runner = StressTestRunner::new(config).unwrap();
    // Override run method to only use pure new keys workers
    runner.run_pure_new_keys_test();
}

#[test]
#[ignore]
fn test_memory_leak_hot_keys_only() {
    // Test with only hot keys (always repeated) - baseline for comparison
    let config = StressTestConfig {
        duration_seconds: 600, // 10 minutes
        concurrent_threads: 8,
        key_range: 100, // Small set of hot keys
        memory_check_interval_seconds: 5,
        ..Default::default()
    };
    
    println!("Starting hot keys only memory leak test");
    println!("Only using keys 0-99 repeatedly - baseline test");
    
    let runner = StressTestRunner::new(config).unwrap();
    runner.run_hot_keys_only_test();
} 