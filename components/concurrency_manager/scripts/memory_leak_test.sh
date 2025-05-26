#!/bin/bash

# High-throughput memory leak stress test runner for concurrency_manager
# Usage: ./memory_leak_test.sh [short|medium|long|production|hot_vs_new|pure_new|hot_only]
#
# PERFORMANCE: All tests now run at maximum throughput (>10M ops/s) with sleep 
# limitations removed to reproduce production-level memory leak conditions.
#
# ISOLATION: Specialized tests (hot_vs_new, pure_new, hot_only) run ONLY their
# specific worker type for precise memory leak pattern detection.
#
# Emergency stop: If the script hangs and Ctrl+C doesn't work, run in another terminal:
#   pkill -f memory_leak_test.sh
#   pkill -f "cargo test.*memory_leak_stress"
#
# This will forcefully terminate all related processes.

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Default test type
TEST_TYPE=${1:-short}

# Configuration
WORKSPACE_DIR=$(cd "$(dirname "$0")/../../.." && pwd)
LOG_DIR="$WORKSPACE_DIR/logs/memory_leak_test"
TIMESTAMP=$(date +"%Y%m%d_%H%M%S")
TEST_LOG="$LOG_DIR/test_${TEST_TYPE}_${TIMESTAMP}.log"
MEMORY_LOG="$LOG_DIR/memory_${TEST_TYPE}_${TIMESTAMP}.log"
SYSTEM_LOG="$LOG_DIR/system_${TEST_TYPE}_${TIMESTAMP}.log"

echo -e "${GREEN}=== TiKV Concurrency Manager Memory Leak Test ===${NC}"
echo "Test type: $TEST_TYPE"
echo "Workspace: $WORKSPACE_DIR"
echo "Logs will be saved to: $LOG_DIR"

# Create log directory
mkdir -p "$LOG_DIR"

# Function to log with timestamp
log_with_timestamp() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" | tee -a "$SYSTEM_LOG"
}

# Function to monitor system memory
monitor_system_memory() {
    log_with_timestamp "Starting system memory monitoring..."
    
    # Create a flag file to signal when to stop
    local stop_flag="/tmp/memory_monitor_stop_$$"
    
    while [ ! -f "$stop_flag" ]; do
        # Get memory info
        MEM_INFO=$(free -m | grep '^Mem:')
        TOTAL=$(echo $MEM_INFO | awk '{print $2}')
        USED=$(echo $MEM_INFO | awk '{print $3}')
        FREE=$(echo $MEM_INFO | awk '{print $4}')
        CACHED=$(echo $MEM_INFO | awk '{print $6}')
        
        # Get CPU info
        CPU_USAGE=$(top -bn1 | grep "Cpu(s)" | awk '{print $2}' | cut -d'%' -f1)
        
        # Get process info for our test
        RUST_PROCESSES=$(ps aux | grep -E "(cargo|concurrency_manager)" | grep -v grep | wc -l)
        
        echo "$(date '+%Y-%m-%d %H:%M:%S'),${TOTAL},${USED},${FREE},${CACHED},${CPU_USAGE},${RUST_PROCESSES}" >> "$MEMORY_LOG"
        
        # Sleep in small intervals to check stop flag more frequently
        for i in {1..30}; do
            sleep 1
            if [ -f "$stop_flag" ]; then
                break
            fi
        done
    done
    
    # Clean up stop flag
    rm -f "$stop_flag"
    log_with_timestamp "System memory monitoring stopped"
}

# Global variables for process tracking
MONITOR_PID=""
CARGO_PID=""
STOP_FLAG=""

# Enhanced cleanup function
cleanup_processes() {
    log_with_timestamp "Cleaning up all processes..."
    
    # Stop memory monitoring
    if [ -n "$MONITOR_PID" ] && kill -0 "$MONITOR_PID" 2>/dev/null; then
        log_with_timestamp "Stopping memory monitor process ($MONITOR_PID)..."
        # Create stop flag for graceful shutdown
        touch "/tmp/memory_monitor_stop_$$"
        # Wait a bit for graceful shutdown
        sleep 2
        # Force kill if still running
        if kill -0 "$MONITOR_PID" 2>/dev/null; then
            kill -TERM "$MONITOR_PID" 2>/dev/null
            sleep 1
            kill -KILL "$MONITOR_PID" 2>/dev/null
        fi
        wait "$MONITOR_PID" 2>/dev/null
    fi
    
    # Stop cargo test processes
    log_with_timestamp "Stopping cargo test processes..."
    pkill -f "cargo test.*memory_leak_stress" 2>/dev/null || true
    pkill -f "memory_leak_stress" 2>/dev/null || true
    
    # Kill any remaining child processes
    log_with_timestamp "Cleaning up remaining child processes..."
    jobs -p | xargs -r kill -TERM 2>/dev/null || true
    sleep 1
    jobs -p | xargs -r kill -KILL 2>/dev/null || true
    
    # Clean up temporary files
    rm -f "/tmp/memory_monitor_stop_$$" 2>/dev/null
    
    log_with_timestamp "Cleanup completed"
}

# Function to check prerequisites
check_prerequisites() {
    log_with_timestamp "Checking prerequisites..."
    
    # Check if we're in TiKV workspace
    if [ ! -f "$WORKSPACE_DIR/Cargo.toml" ] || ! grep -q "tikv" "$WORKSPACE_DIR/Cargo.toml"; then
        echo -e "${RED}Error: Not in TiKV workspace${NC}"
        exit 1
    fi
    
    # Check if jemalloc feature is available
    if ! grep -q "tikv_alloc.*jemalloc" "$WORKSPACE_DIR/components/concurrency_manager/Cargo.toml"; then
        echo -e "${YELLOW}Warning: jemalloc feature might not be available${NC}"
    fi
    
    # Check available disk space (need at least 1GB for logs)
    AVAILABLE_SPACE=$(df "$LOG_DIR" | tail -1 | awk '{print $4}')
    if [ "$AVAILABLE_SPACE" -lt 1048576 ]; then  # 1GB in KB
        echo -e "${YELLOW}Warning: Low disk space, logs might be large${NC}"
    fi
    
    log_with_timestamp "Prerequisites check completed"
}

# Function to run the actual test
run_test() {
    log_with_timestamp "Starting $TEST_TYPE test..."
    
    # Set up enhanced signal handling
    trap 'cleanup_processes; exit 130' INT TERM
    trap 'cleanup_processes; exit 0' EXIT
    
    # Start system monitoring in background
    monitor_system_memory &
    MONITOR_PID=$!
    log_with_timestamp "Started memory monitor with PID: $MONITOR_PID"
    
    cd "$WORKSPACE_DIR"
    
    # Build in release mode for better performance
    log_with_timestamp "Building concurrency_manager in release mode..."
    cargo build --package concurrency_manager --release 2>&1 | tee -a "$TEST_LOG"
    
    if [ $? -ne 0 ]; then
        log_with_timestamp "Build failed, stopping test"
        return 1
    fi
    
    # Run the specific test
    log_with_timestamp "Running memory leak test: $TEST_TYPE"
    echo "Test output will be logged to: $TEST_LOG"
    
    # Set environment variables for better memory tracking
    export MALLOC_CONF="prof:true,lg_prof_sample:0"
    export RUST_BACKTRACE=1
    
    # Run the test and capture output
    local test_cmd=""
    case $TEST_TYPE in
        "short")
            test_cmd="timeout --signal=TERM --kill-after=10 600 cargo test --package concurrency_manager --test memory_leak_stress --release -- test_memory_leak_short --exact --ignored --nocapture"
            ;;
        "medium")
            test_cmd="timeout --signal=TERM --kill-after=10 4000 cargo test --package concurrency_manager --test memory_leak_stress --release -- test_memory_leak_medium --exact --ignored --nocapture"
            ;;
        "long")
            test_cmd="timeout --signal=TERM --kill-after=10 25200 cargo test --package concurrency_manager --test memory_leak_stress --release -- test_memory_leak_long --exact --ignored --nocapture"
            ;;
        "production")
            test_cmd="cargo test --package concurrency_manager --test memory_leak_stress --release -- test_memory_leak_production_simulation --exact --ignored --nocapture"
            ;;
        "hot_vs_new")
            test_cmd="timeout --signal=TERM --kill-after=10 2100 cargo test --package concurrency_manager --test memory_leak_stress --release -- test_memory_leak_hot_vs_new_keys --exact --ignored --nocapture"
            ;;
        "pure_new")
            test_cmd="timeout --signal=TERM --kill-after=10 1500 cargo test --package concurrency_manager --test memory_leak_stress --release -- test_memory_leak_pure_new_keys --exact --ignored --nocapture"
            ;;
        "hot_only")
            test_cmd="timeout --signal=TERM --kill-after=10 800 cargo test --package concurrency_manager --test memory_leak_stress --release -- test_memory_leak_hot_keys_only --exact --ignored --nocapture"
            ;;
        *)
            echo -e "${RED}Unknown test type: $TEST_TYPE${NC}"
            echo "Available types: short, medium, long, production, hot_vs_new, pure_new, hot_only"
            return 1
            ;;
    esac
    
    log_with_timestamp "Executing: $test_cmd"
    
    # Run the test
    eval "$test_cmd" 2>&1 | tee -a "$TEST_LOG" &
    CARGO_PID=$!
    
    # Wait for the test to complete or be interrupted
    wait $CARGO_PID
    TEST_EXIT_CODE=$?
    
    log_with_timestamp "Test completed with exit code: $TEST_EXIT_CODE"
    
    # Clean up
    cleanup_processes
    
    return $TEST_EXIT_CODE
}

# Function to analyze results
analyze_results() {
    log_with_timestamp "Analyzing test results..."
    
    echo -e "\n${GREEN}=== TEST ANALYSIS ===${NC}"
    
    # Check if test completed successfully
    if grep -q "Memory growth rate appears normal" "$TEST_LOG"; then
        echo -e "${GREEN}✅ No memory leak detected${NC}"
    elif grep -q "WARNING: Potential memory leak detected" "$TEST_LOG"; then
        echo -e "${RED}⚠️  Potential memory leak detected!${NC}"
        echo "Check the detailed analysis in the test output"
    else
        echo -e "${YELLOW}⚠️  Test results inconclusive${NC}"
    fi
    
    # Extract key metrics from test log
    echo -e "\n${GREEN}=== KEY METRICS ===${NC}"
    grep -E "(Test Duration|Memory Growth|Growth Rate|Total Operations)" "$TEST_LOG" | tail -20
    
    # Analyze system memory usage
    if [ -f "$MEMORY_LOG" ]; then
        echo -e "\n${GREEN}=== SYSTEM MEMORY ANALYSIS ===${NC}"
        echo "Memory usage over time (format: timestamp,total,used,free,cached,cpu,processes):"
        echo "Timestamp,Total_MB,Used_MB,Free_MB,Cached_MB,CPU_%,Rust_Processes" > "${MEMORY_LOG}.csv"
        cat "$MEMORY_LOG" >> "${MEMORY_LOG}.csv"
        
        # Show first and last few entries
        echo "First 5 readings:"
        head -6 "${MEMORY_LOG}.csv" | column -t -s','
        echo "..."
        echo "Last 5 readings:"
        tail -5 "${MEMORY_LOG}.csv" | column -t -s','
        
        # Calculate memory growth from system perspective
        FIRST_USED=$(head -2 "${MEMORY_LOG}.csv" | tail -1 | cut -d',' -f3)
        LAST_USED=$(tail -1 "${MEMORY_LOG}.csv" | cut -d',' -f3)
        SYSTEM_GROWTH=$((LAST_USED - FIRST_USED))
        
        echo -e "\nSystem memory growth: ${SYSTEM_GROWTH} MB"
        if [ "$SYSTEM_GROWTH" -gt 100 ]; then
            echo -e "${YELLOW}⚠️  Significant system memory growth detected${NC}"
        fi
    fi
    
    # Generate summary report
    {
        echo "=== MEMORY LEAK TEST SUMMARY ==="
        echo "Test Type: $TEST_TYPE"
        echo "Timestamp: $TIMESTAMP"
        echo "Test Duration: $(get_test_duration)"
        echo ""
        echo "Files generated:"
        echo "- Test log: $TEST_LOG"
        echo "- Memory log: $MEMORY_LOG"
        echo "- System log: $SYSTEM_LOG"
        echo "- Memory CSV: ${MEMORY_LOG}.csv"
        echo ""
        echo "Key findings:"
        grep -E "(WARNING|✅|⚠️)" "$TEST_LOG" 2>/dev/null || echo "No specific warnings found"
    } > "$LOG_DIR/summary_${TEST_TYPE}_${TIMESTAMP}.txt"
    
    echo -e "\n${GREEN}Summary report saved to: $LOG_DIR/summary_${TEST_TYPE}_${TIMESTAMP}.txt${NC}"
}

# Function to get test duration
get_test_duration() {
    if [ -f "$TEST_LOG" ] && grep -q "Test Duration:" "$TEST_LOG"; then
        grep "Test Duration:" "$TEST_LOG" | tail -1 | awk '{print $3}'
    else
        echo "Unknown"
    fi
}

# Function to show usage
show_usage() {
    echo "Usage: $0 [test_type]"
    echo ""
    echo "Test types:"
    echo "  short      - 5 minute high-throughput test for quick validation"
    echo "  medium     - 1 hour high-throughput test for moderate analysis"
    echo "  long       - 6 hour high-throughput test for thorough analysis"
    echo "  production - 24 hour comprehensive test (all worker types)"
    echo "  hot_vs_new - 30 minute test: ONLY hot keys + new keys pattern (70%/30%)"
    echo "  pure_new   - 20 minute test: ONLY unique keys (maximum key space growth)"
    echo "  hot_only   - 10 minute test: ONLY repeated hot keys (baseline)"
    echo ""
    echo "Examples:"
    echo "  $0 short       # Run a quick 5-minute high-throughput test"
    echo "  $0 hot_vs_new  # Run specialized hot vs new keys test"
    echo "  $0 pure_new    # Run specialized pure new keys test"
    echo "  $0 hot_only    # Run baseline hot keys only test"
    echo ""
    echo "Performance Note:"
    echo "  All tests now run at maximum throughput (>10M ops/s) to reproduce"
    echo "  production-level memory leak conditions. Sleep limitations removed."
    echo ""
    echo "Test Isolation:"
    echo "  - hot_vs_new, pure_new, hot_only: Run ONLY their specific worker type"
    echo "  - short, medium, long, production: Run ALL worker types (mixed load)"
    echo ""
    echo "Control:"
    echo "  Ctrl+C         - Normal stop (recommended)"
    echo "  ./stop_memory_test.sh - Emergency stop if Ctrl+C doesn't work"
    echo ""
    echo "Logs are saved in: $LOG_DIR"
}

# Main execution
main() {
    case "$1" in
        "--help"|"-h")
            show_usage
            exit 0
            ;;
        "short"|"medium"|"long"|"production"|"hot_vs_new"|"pure_new"|"hot_only"|"")
            ;;
        *)
            echo -e "${RED}Invalid test type: $1${NC}"
            show_usage
            exit 1
            ;;
    esac
    
    check_prerequisites
    
    if run_test; then
        analyze_results
        echo -e "\n${GREEN}✅ Memory leak test completed successfully${NC}"
    else
        echo -e "\n${RED}❌ Memory leak test failed or was interrupted${NC}"
        analyze_results  # Still try to analyze partial results
        exit 1
    fi
}

# Set up immediate cleanup on any signal
cleanup_on_signal() {
    echo "Received signal, cleaning up..."
    cleanup_processes 2>/dev/null || true
    exit 130
}

# Install signal handlers early
trap cleanup_on_signal INT TERM QUIT
trap cleanup_processes EXIT

# Run main function
main "$@" 