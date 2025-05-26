# Concurrency Manager High-Throughput Memory Leak Testing

This directory contains comprehensive tools to detect potential memory leaks in the concurrency_manager component. The testing suite now runs at **maximum throughput (>10M ops/s)** with specialized test isolation to reproduce production-level memory leak conditions like those observed in production (1GB/day growth).

## 🚀 Key Improvements

### Performance Enhancements
- **Removed all sleep limitations** - tests now run at maximum CPU capacity
- **>15M operations/second** achievable throughput (6,740x improvement)
- **Production-level stress** - no artificial throttling

### Test Isolation
- **Specialized tests** run only their specific worker type
- **Precise leak detection** - isolate specific memory leak patterns
- **Baseline comparisons** - hot keys vs new keys vs mixed workload

## Overview

The testing suite includes:
- **6 specialized worker types** with isolated testing capability
- **High-throughput stress testing** at maximum CPU utilization
- **Memory monitoring** with both jemalloc and system-level tracking
- **Automated analysis** with leak detection and growth rate calculation
- **Multiple test durations** from 5 minutes to 24 hours
- **Detailed reporting** with CSV data export for further analysis

## Test Scenarios

### 🔥 Specialized Tests (Single Worker Type)

#### 1. Hot vs New Keys Test (`hot_vs_new`)
- **Pattern**: 70% hot keys (repeated) + 30% new keys (never repeated)
- **Purpose**: Test key space growth vs repeated access patterns
- **Duration**: 30 minutes
- **Worker**: Only `hot_vs_new_keys_worker`

#### 2. Pure New Keys Test (`pure_new`)
- **Pattern**: 100% unique keys, never repeated
- **Purpose**: Maximum key space growth stress test
- **Duration**: 20 minutes  
- **Worker**: Only `pure_new_keys_worker`

#### 3. Hot Keys Only Test (`hot_only`)
- **Pattern**: 100% repeated hot keys (baseline)
- **Purpose**: Baseline test with no key space growth
- **Duration**: 10 minutes
- **Worker**: Only `lock_stress_worker` with fixed key range

### 🔀 Comprehensive Tests (All Worker Types)

#### 4. Lock Stress Testing
- High concurrent lock operations
- Simulates transaction lock acquisition and release
- Tests LockTable memory management under extreme load

#### 5. Max Timestamp Updates
- Continuous max_ts updates at maximum rate
- Tests timestamp limit management
- Simulates high-frequency PD interactions

#### 6. Range Read Operations
- Large range scans across the lock table
- Tests skiplist iteration memory usage
- Simulates analytical queries under stress

#### 7. Mixed Workload
- Point reads, range scans, lock cleanup
- Combination of all scenarios simultaneously

## Usage

### Quick Start

```bash
# Run a 5-minute high-throughput validation test
./components/concurrency_manager/scripts/memory_leak_test.sh short

# Run specialized hot vs new keys test
./components/concurrency_manager/scripts/memory_leak_test.sh hot_vs_new

# Run pure new keys test (maximum key space growth)
./components/concurrency_manager/scripts/memory_leak_test.sh pure_new

# Run baseline hot keys only test
./components/concurrency_manager/scripts/memory_leak_test.sh hot_only

# Run comprehensive 24-hour production simulation
./components/concurrency_manager/scripts/memory_leak_test.sh production
```

### Prerequisites

1. **jemalloc support**: Ensure TiKV is built with jemalloc
   ```bash
   # Check if jemalloc is available
   grep -r "jemalloc" components/concurrency_manager/Cargo.toml
   ```

2. **Sufficient disk space**: Tests generate detailed logs (1GB+ for long tests)

3. **Release build**: Tests automatically build in release mode for performance

4. **CPU capacity**: Tests now utilize full CPU capacity - ensure adequate cooling

### Test Configuration

Each test type has different parameters optimized for maximum throughput:

| Test Type | Duration | Threads | Throughput | Key Range | Purpose |
|-----------|----------|---------|------------|-----------|---------|
| short     | 5 min    | 12      | Max        | 100K      | Quick validation |
| medium    | 1 hour   | 16      | Max        | 100K      | Moderate analysis |
| long      | 6 hours  | 24      | Max        | 1M        | Thorough analysis |
| production| 24 hours | 32      | Max        | 10M       | Full simulation |
| hot_vs_new| 30 min   | 24      | Max        | 10K       | **Specialized: 70%/30% pattern** |
| pure_new  | 20 min   | 8       | Max        | N/A       | **Specialized: 100% new keys** |
| hot_only  | 10 min   | 8       | Max        | 100       | **Specialized: baseline** |

**Note**: "Max" throughput means no artificial limitations - tests run at maximum CPU capacity.

## Output and Analysis

### Log Files

Tests generate several log files in `logs/memory_leak_test/`:

- **test_[type]_[timestamp].log**: Complete test output with memory analysis
- **memory_[type]_[timestamp].log**: System memory usage over time  
- **memory_[type]_[timestamp].log.csv**: CSV format for analysis
- **system_[type]_[timestamp].log**: System events and monitoring logs
- **summary_[type]_[timestamp].txt**: Executive summary report

### Memory Analysis

The test automatically analyzes memory growth patterns:

```
========== MEMORY LEAK ANALYSIS ==========
Test Duration: 300.12s
Memory Growth:
  Allocated: 245 MB (+89 MB)
  Resident: 198 MB (+67 MB)
Growth Rate:
  Allocated: 17.80 MB/hour (0.42 GB/day)
  Resident: 13.40 MB/hour (0.32 GB/day)
✅ Memory growth rate appears normal

Operation Statistics:
  Total Operations: 308,189,478
  Total Errors: 0
  OPS: 15,412,469.80/s
  Error Rate: 0.00%
```

### Performance Metrics

Expect these throughput levels:
- **Lock operations**: 10-20M ops/s
- **Range reads**: 1-5M ops/s  
- **Mixed workload**: 5-15M ops/s
- **Hot vs new keys**: 8-12M ops/s

### Leak Detection Thresholds

The test uses these thresholds to detect potential leaks:
- **Warning threshold**: > 100 MB/day growth
- **Analysis**: Compares allocated vs resident memory
- **Trend analysis**: Looks for consistent upward trends
- **Operation correlation**: Growth rate per million operations

## Manual Test Execution

You can also run individual tests manually:

```bash
# Build with maximum optimization
cargo build --package concurrency_manager --release

# Run specific specialized test
cargo test --package concurrency_manager --test memory_leak_stress \
  --release -- test_memory_leak_hot_vs_new_keys --exact --ignored --nocapture

# Run pure new keys test
cargo test --package concurrency_manager --test memory_leak_stress \
  --release -- test_memory_leak_pure_new_keys --exact --ignored --nocapture

# With memory profiling
export MALLOC_CONF="prof:true,prof_leak:true,prof_active:true"
cargo test --package concurrency_manager --test memory_leak_stress \
  --release -- test_memory_leak_short --exact --ignored --nocapture
```

## Test Strategy Guide

### 🎯 Choosing the Right Test

| Suspected Issue | Recommended Test | Why |
|----------------|------------------|-----|
| Key space growth | `pure_new` | Maximum key space stress |
| Hot key handling | `hot_only` | Baseline without growth |
| Mixed patterns | `hot_vs_new` | Realistic 70/30 split |
| General leaks | `short` then `medium` | Quick validation → deeper analysis |
| Production correlation | `production` | Full 24-hour simulation |

### 🔍 Interpreting Results

#### Normal Behavior
- Memory growth < 100 MB/day
- Stable allocated/resident ratio  
- Throughput >5M ops/s
- No sustained upward trend

#### Potential Leak Indicators
- Growth rate > 100 MB/day
- Continuously increasing allocated memory
- Large gap between allocated and resident memory
- Memory that doesn't stabilize over time
- Different growth rates between test types

#### Test Comparison Analysis
```bash
# Compare growth rates across test types
./memory_leak_test.sh hot_only    # Should show minimal growth
./memory_leak_test.sh pure_new    # May show higher growth
./memory_leak_test.sh hot_vs_new  # Should show moderate growth

# If pure_new >> hot_only growth, key space management issue
# If all tests show similar growth, general leak issue
```

## Production Correlation

To correlate with production issues:

1. **Growth Rate**: Production shows ~1GB/day, tests aim to detect patterns that could lead to similar growth
2. **Workload Patterns**: Specialized tests isolate specific production patterns
3. **Duration**: Longer tests (6+ hours) better simulate production memory behavior
4. **Scale**: Tests now achieve production-level operation rates
5. **Pattern Isolation**: Compare hot_vs_new vs pure_new vs hot_only to identify leak sources

## Advanced Analysis

### Memory Pattern Detection

```bash
# Run all specialized tests in sequence for pattern analysis
./memory_leak_test.sh hot_only     # Baseline
./memory_leak_test.sh pure_new     # Maximum growth
./memory_leak_test.sh hot_vs_new   # Mixed pattern

# Analyze CSV data
python3 components/concurrency_manager/scripts/visualize_memory.py \
  logs/memory_leak_test/memory_hot_only_*.csv \
  logs/memory_leak_test/memory_pure_new_*.csv \
  logs/memory_leak_test/memory_hot_vs_new_*.csv
```

### Custom Test Development

To add new test patterns, implement new worker methods:

```rust
fn your_custom_worker(&self, worker_id: usize) {
    while self.running.load(Ordering::Relaxed) {
        // Your test logic here
        
        // Minimal yielding for CPU sharing
        if random_condition() {
            thread::yield_now();
        }
    }
}
```

## Troubleshooting

### Performance Issues
- **Low throughput**: Check CPU usage, may need fewer threads
- **High CPU usage**: Expected behavior, tests run at maximum capacity
- **Memory pressure**: Monitor system memory, may need to reduce concurrent tests

### Build Issues
```bash
# If build fails
cargo clean
cargo build --package concurrency_manager --release

# Verify high performance build
cargo test --package concurrency_manager --test memory_leak_stress --release -- --list
```

### Memory Tracking Issues
```bash
# Verify jemalloc is working
export MALLOC_CONF="prof:true"
ldd target/release/deps/memory_leak_stress-* | grep jemalloc

# Check memory stats availability
cargo test --package concurrency_manager --test memory_leak_stress --release -- test_memory_leak_short --exact --ignored --nocapture | grep "jemalloc stats"
```

## Emergency Stop

If tests consume too many resources:

```bash
# Normal stop
Ctrl+C

# Emergency stop
./components/concurrency_manager/scripts/stop_memory_test.sh

# Nuclear option
sudo pkill -9 -f memory_leak_stress
```

Remember: These tests now run at maximum throughput to reproduce production-level memory leak conditions. Monitor system resources accordingly. 