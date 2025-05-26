#!/bin/bash

# Emergency stop script for memory leak tests
# Run this if memory_leak_test.sh hangs and won't respond to Ctrl+C

echo "🛑 强制停止所有内存泄漏测试进程..."

# Kill memory test script
echo "停止测试脚本..."
pkill -f "memory_leak_test.sh" 2>/dev/null && echo "✅ 测试脚本已停止" || echo "ℹ️ 未找到运行中的测试脚本"

# Kill cargo test processes
echo "停止 cargo test 进程..."
pkill -f "cargo test.*memory_leak_stress" 2>/dev/null && echo "✅ cargo test 进程已停止" || echo "ℹ️ 未找到运行中的 cargo test"

# Kill concurrency_manager test processes
echo "停止 concurrency_manager 测试进程..."
pkill -f "memory_leak_stress" 2>/dev/null && echo "✅ 测试进程已停止" || echo "ℹ️ 未找到运行中的测试进程"

# Kill any remaining cargo processes
echo "停止所有 cargo 进程..."
pkill cargo 2>/dev/null && echo "✅ cargo 进程已停止" || echo "ℹ️ 未找到运行中的 cargo 进程"

# Clean up temporary files
echo "清理临时文件..."
rm -f /tmp/memory_monitor_stop_* 2>/dev/null && echo "✅ 临时文件已清理" || echo "ℹ️ 未找到临时文件"

echo ""
echo "🔍 检查剩余进程："
echo "内存测试相关进程："
ps aux | grep -E "(memory_leak|cargo.*test)" | grep -v grep | grep -v "stop_memory_test.sh" || echo "   无"

echo ""
echo "✅ 清理完成！"
echo "如果进程仍在运行，你可以尝试："
echo "   sudo killall -9 cargo"
echo "   sudo pkill -9 -f memory_leak" 