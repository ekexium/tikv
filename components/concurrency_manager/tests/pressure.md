# Pressure tests: LockTable and SkipMap

This document describes how to run the pressure tests in
`components/concurrency_manager/tests/pressure.rs` and how to interpret the
results when investigating memory growth.

## Design goals and how the tests meet them

Goal: reproduce and differentiate memory growth in the LockTable/SkipMap path
on a single machine with evidence that supports or rejects our hypotheses.

- Isolate layers.
  - `stress_lock_table_churn` exercises the full `LockTable`/`KeyHandle` path.
  - `stress_skipmap_churn` uses `SkipMap` directly to isolate epoch/skiplist GC.
- Create high churn with controlled concurrency and duration.
  - `STRESS_THREADS`/`STRESS_TASKS` control parallelism.
  - `*_DURATION_SECS` runs the workload for a fixed window to compare runs.
  - `*_KEY_SPACE`, `*_READ_RATIO`, `*_SET_LOCK_RATIO`, `*_HOLD_US` tune
    contention, read/write mix, and lock hold time.
- Exercise scan paths and long-lived handles.
  - `CM_STRESS_SCAN_RATIO` and `CM_STRESS_RANGE_RATIO` trigger full scans and
    range scans.
  - `CM_STRESS_STASH_GUARDS` keeps a bounded stash of guards to extend lifetimes.
- Provide empirical evidence.
  - Periodic stats report `len` plus jemalloc and RSS metrics.
  - The idle phase (`*_IDLE_SECS`) shows whether memory drains after activity.
  - Optional idle GC tick (`*_IDLE_GC`) tests the "GC idle" hypothesis directly.
  - `CM_STRESS_ASSERT_EMPTY=true` turns a suspected leak into a hard failure.
- Reproducible and comparable.
  - Per-task RNG seeds make runs deterministic for a given configuration.
  - Global overrides let you keep a consistent baseline across tests.

## What the tests do

- `stress_lock_table_churn`:
  - Spawns async tasks on a Tokio runtime.
  - Each task loops over randomly generated keys from a fixed key space.
  - The loop chooses read vs write based on `*_READ_RATIO`.
  - Reads can be point gets, full scans, or range scans based on ratios.
  - Writes call `lock_key`, optionally set a `Lock`, optionally hold the guard,
    and can stash a bounded number of guards to extend lifetimes.
  - Periodically reports map length and memory stats; optionally runs an idle
    phase to observe GC behavior after load.

- `stress_skipmap_churn`:
  - Spawns OS threads that directly operate on a `SkipMap<u64, u64>`.
  - Each loop iteration inserts or removes based on `*_INSERT_RATIO`.
  - Periodically reports map length and memory stats; optionally runs an idle
    phase to observe GC behavior without the ConcurrencyManager layer.

## How to run

LockTable churn (async LockTable + KeyHandle lifecycle):

```
cargo test -p concurrency_manager --test pressure --release -- --ignored stress_lock_table_churn --nocapture
```

SkipMap churn (SkipMap only, no ConcurrencyManager):

```
cargo test -p concurrency_manager --test pressure --release -- --ignored stress_skipmap_churn --nocapture
```

To keep logs small, redirect output:

```
... > lock_table_pressure.log 2>&1
```

## Configuration

Configuration is done via environment variables. Per-test variables override
global ones. Duration mode overrides the per-op loop count.

Each test prints a single `config ...` line at startup with the effective
values used.

### Baseline defaults

When no environment variables are set, the tests default to a mixed workload
intended to resemble real usage while still stressing the lock table and
skiplist:

- `STRESS_DURATION_SECS=60`, `STRESS_IDLE_SECS=20`
- `CM_STRESS_READ_RATIO=40`
- `CM_STRESS_SCAN_RATIO=5`, `CM_STRESS_RANGE_RATIO=5`
- `CM_STRESS_SET_LOCK_RATIO=80`
- `CM_STRESS_HOLD_US=50`
- `CM_STRESS_STASH_GUARDS=64`
- `SKIPMAP_STRESS_INSERT_RATIO=50`

These defaults create a read/write mix with a small fraction of scans, short
lock holds, and a bounded set of long-lived guards. Adjust upward for more
stress or downward for faster iterations.

### Global (apply to all tests)

- `STRESS_THREADS`: worker thread count
- `STRESS_DURATION_SECS`: run until this time limit (0 = disabled)
- `STRESS_REPORT_INTERVAL_MS`: stats logging interval
- `STRESS_IDLE_SECS`: idle observation window after workload
- `STRESS_IDLE_GC`: set to `true` to tick GC during idle
- `STRESS_TASKS`: task count for the LockTable test

### LockTable-specific

- `CM_STRESS_THREADS`
- `CM_STRESS_TASKS`
- `CM_STRESS_OPS_PER_TASK`
- `CM_STRESS_KEY_SPACE`
- `CM_STRESS_KEY_LEN`
- `CM_STRESS_READ_RATIO`
- `CM_STRESS_SCAN_RATIO`
- `CM_STRESS_RANGE_RATIO`
- `CM_STRESS_SET_LOCK_RATIO`
- `CM_STRESS_HOLD_US`
- `CM_STRESS_STASH_GUARDS`
- `CM_STRESS_DURATION_SECS`
- `CM_STRESS_REPORT_INTERVAL_MS`
- `CM_STRESS_IDLE_SECS`
- `CM_STRESS_IDLE_GC`
- `CM_STRESS_ASSERT_EMPTY`

Notes:
- `CM_STRESS_SCAN_RATIO` and `CM_STRESS_RANGE_RATIO` are percentages of read
  operations; range scans start at the current key and scan to the end.
- `CM_STRESS_STASH_GUARDS` is a per-task cap on guards kept alive to extend
  `KeyHandle` lifetimes.

### SkipMap-specific

- `SKIPMAP_STRESS_THREADS`
- `SKIPMAP_STRESS_OPS_PER_THREAD`
- `SKIPMAP_STRESS_KEY_SPACE`
- `SKIPMAP_STRESS_INSERT_RATIO`
- `SKIPMAP_STRESS_DURATION_SECS`
- `SKIPMAP_STRESS_REPORT_INTERVAL_MS`
- `SKIPMAP_STRESS_IDLE_SECS`
- `SKIPMAP_STRESS_IDLE_GC`

## Output fields

Each line contains:

- `tag`: phase (`run`, `workload_done`, `idle`, `done`)
- `elapsed_s`: seconds since start
- `ops` / `ops_s`: total ops and ops per second
- `len`: current map size
- `rss_mb`: process RSS (includes non-jemalloc memory)
- `jemalloc_*_mb`: jemalloc stats (`allocated`, `active`, `resident`, `retained`)
- `jemalloc=NA`: jemalloc stats not available (use `rss_mb` and `len`)

## Interpretation guide

### H1: Arc/KeyHandle leak

Evidence:
- `len` grows over time during `run`, or stays high after `workload_done` and
  `idle`.
- `CM_STRESS_ASSERT_EMPTY=true` fails.

Conclusion:
entries remain in LockTable; likely strong refs survive.

### H2: Epoch GC idle

Evidence:
- With `STRESS_IDLE_GC=false`, `jemalloc_alloc_mb` stays high during idle.
- With `STRESS_IDLE_GC=true`, `jemalloc_alloc_mb` drops during idle.

Conclusion:
GC inactivity explains persistence when the system is idle.

### H3: GC throughput/backlog

Evidence:
- During `run`, `jemalloc_alloc_mb` increases while `len` is stable.
- After workload, memory stabilizes or recovers (not unbounded).

Conclusion:
GC lags under churn, but garbage is eventually reclaimed.

### SkipMap isolation

Evidence:
- SkipMap test shows similar growth pattern without ConcurrencyManager.

Conclusion:
issue is likely in SkipMap or epoch GC, not in KeyHandle usage.

## Recommended experiments

1. Run LockTable test twice with the same duration:
   - `STRESS_IDLE_GC=false`
   - `STRESS_IDLE_GC=true`
2. Run SkipMap test with large `KEY_SPACE` and long duration.
3. Compare `len` and `jemalloc_alloc_mb` trends across runs.

If you share the last few `run`, `workload_done`, and `idle` lines from each
run, we can interpret them precisely.
