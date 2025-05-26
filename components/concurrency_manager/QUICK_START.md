# Quick Start Guide - Memory Leak Testing

## 🚀 立即开始

### 1. 运行快速验证测试（5分钟）

```bash
# 进入 TiKV 根目录
cd /data/code/tikv

# 运行5分钟快速测试
./components/concurrency_manager/scripts/memory_leak_test.sh short
```

### 2. 查看测试结果

测试完成后会自动显示分析结果：

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
```

### 3. 可视化分析（可选）

如果安装了Python依赖：

```bash
# 安装依赖
pip3 install matplotlib numpy

# 可视化最新的测试结果
python3 components/concurrency_manager/scripts/visualize_memory.py \
  logs/memory_leak_test/memory_short_*.csv
```

## 🔍 针对生产问题的建议测试策略

基于你观察到的每天1GB增长，建议按以下顺序进行：

### 阶段1: 快速验证（5-10分钟）
```bash
./components/concurrency_manager/scripts/memory_leak_test.sh short
```
**目的**: 验证测试环境和工具正常工作

### 阶段2: 中等强度测试（1小时）
```bash
./components/concurrency_manager/scripts/memory_leak_test.sh medium
```
**目的**: 检测明显的内存泄漏模式

### 阶段3: 深度分析（6小时）
```bash
nohup ./components/concurrency_manager/scripts/memory_leak_test.sh long > long_test.log 2>&1 &
```
**目的**: 模拟接近生产环境的长时间运行

### 阶段4: 生产模拟（24小时）
```bash
nohup ./components/concurrency_manager/scripts/memory_leak_test.sh production > production_test.log 2>&1 &
```
**目的**: 完整复现生产环境内存增长模式

## 📊 结果解读

### 正常指标
- 增长率 < 0.1 GB/day
- R² < 0.7 (趋势不强)
- 内存使用相对稳定

### 可疑指标
- 增长率 > 0.5 GB/day
- R² > 0.8 (强上升趋势)
- 持续的内存上升

### 确认泄漏
- 增长率 > 1.0 GB/day
- 强线性增长趋势
- 大量异常点

## 🛠️ 故障排除

### 构建失败
```bash
# 清理并重新构建
cargo clean
cargo build --package concurrency_manager --release --features jemalloc
```

### jemalloc未启用
```bash
# 检查依赖
grep -r "jemalloc" components/tikv_alloc/Cargo.toml
```

### 权限问题
```bash
chmod +x components/concurrency_manager/scripts/memory_leak_test.sh
chmod +x components/concurrency_manager/scripts/visualize_memory.py
```

## 🔬 高级分析

### 自定义测试参数
编辑 `components/concurrency_manager/tests/memory_leak_stress.rs`：

```rust
let config = StressTestConfig {
    duration_seconds: 7200,        // 2小时
    concurrent_threads: 20,        // 20个并发线程
    operations_per_second: 1500,   // 1500 ops/sec
    key_range: 1_000_000,         // 100万个key
    // ... 其他参数
};
```

### 监控实时进度
```bash
# 监控测试日志
tail -f logs/memory_leak_test/test_*.log

# 监控系统内存
watch -n 30 'free -h'

# 监控进程内存
watch -n 30 'ps aux | grep concurrency_manager'
```

### 导出数据分析
```bash
# 生成CSV数据
ls logs/memory_leak_test/*.csv

# 使用外部工具分析
# 例如: Excel, R, Python pandas, etc.
```

## 📋 检查清单

运行测试前确认：

- [ ] 在TiKV根目录
- [ ] 有足够磁盘空间（至少1GB）
- [ ] jemalloc功能可用
- [ ] 脚本有执行权限
- [ ] 没有其他高负载进程

测试完成后检查：

- [ ] 测试是否成功完成
- [ ] 内存增长率是否超出阈值
- [ ] 是否有异常内存尖峰
- [ ] 趋势线是否显示持续增长
- [ ] 保存了日志文件供后续分析

## 🚨 紧急情况

如果在测试中发现严重的内存泄漏：

1. **立即停止测试**
```bash
pkill -f concurrency_manager
pkill -f memory_leak_test
```

2. **保存关键数据**
```bash
cp -r logs/memory_leak_test /tmp/emergency_backup
```

3. **分析内存快照**
```bash
# 如果测试仍在运行，获取内存信息
cat /proc/meminfo
ps aux --sort=-%mem | head -20
```

4. **清理资源**
```bash
# 强制垃圾回收（如果适用）
sync && echo 3 > /proc/sys/vm/drop_caches
```

## 新增测试场景：Key 模式分析

### 新的测试类型

我们新增了三种专门的测试场景来检测不同 key 访问模式下的内存泄漏：

#### 1. 热点 vs 新 Key 混合测试 (`hot_vs_new`)
模拟生产环境中的真实场景：
- **70%** 操作访问固定的热点 key (key_000000 到 key_000999)
- **30%** 操作访问永远递增的新 key (key_010000+)

```bash
# 运行30分钟的混合测试
./components/concurrency_manager/scripts/memory_leak_test.sh hot_vs_new
```

**用途**: 检测 concurrency_manager 是否正确清理不再使用的 key，以及是否对新 key 有内存泄漏

#### 2. 纯新 Key 测试 (`pure_new`)
极端场景：所有 key 都是新的，永不重复
- 每个操作都使用全新的 key：pure_new_0000000001, pure_new_0000000002...
- 测试 key 空间无限增长的情况

```bash
# 运行20分钟的纯新key测试
./components/concurrency_manager/scripts/memory_leak_test.sh pure_new
```

**用途**: 检测 key 空间无限增长时的内存管理，验证是否存在 key 相关的内存泄漏

#### 3. 纯热点 Key 测试 (`hot_only`)
基准测试：只使用固定的100个 key，不断重复
- 所有操作都访问 key_00000000 到 key_00000099
- 提供内存使用的基准线

```bash
# 运行10分钟的纯热点key测试
./components/concurrency_manager/scripts/memory_leak_test.sh hot_only
```

**用途**: 作为对比基准，验证在 key 空间固定时的内存表现

### 测试对比分析

建议按以下顺序运行测试，进行对比分析：

```bash
# 1. 先运行基准测试 (10分钟)
./components/concurrency_manager/scripts/memory_leak_test.sh hot_only

# 2. 运行混合场景测试 (30分钟)  
./components/concurrency_manager/scripts/memory_leak_test.sh hot_vs_new

# 3. 运行极端场景测试 (20分钟)
./components/concurrency_manager/scripts/memory_leak_test.sh pure_new
```

### 预期结果

**正常情况下**:
- `hot_only`: 内存使用稳定，无增长
- `hot_vs_new`: 轻微内存增长，但增长率 < 10MB/小时
- `pure_new`: 一定程度的内存增长，但应该有合理的上限

**潜在内存泄漏的表现**:
- `hot_only` vs `pure_new` 有显著差异 → key 管理问题
- `hot_vs_new` 内存增长率 > 100MB/小时 → 新 key 清理问题  
- 所有测试都显示持续增长 → 基础组件内存泄漏

### 实际生产场景映射

- **`hot_vs_new`**: 最接近真实生产环境，有热点 key (用户活跃数据) 和新 key (新写入数据)
- **`pure_new`**: 模拟数据写入密集型场景，如日志写入、时序数据
- **`hot_only`**: 模拟读取密集型场景，如缓存访问

开始第一个测试：`./components/concurrency_manager/scripts/memory_leak_test.sh short` 