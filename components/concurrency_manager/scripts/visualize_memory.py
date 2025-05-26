#!/usr/bin/env python3
"""
Memory Usage Visualization Tool for Concurrency Manager

This script analyzes and visualizes memory usage data from the memory leak tests.
It can detect trends, create plots, and generate reports.

Usage:
    python3 visualize_memory.py <memory_csv_file>
    python3 visualize_memory.py logs/memory_leak_test/memory_short_20241201_120000.log.csv
"""

import sys
import csv
import argparse
from datetime import datetime
import numpy as np
import matplotlib.pyplot as plt
import matplotlib.dates as mdates
from pathlib import Path
import json

def parse_memory_csv(csv_file):
    """Parse the memory CSV file and return structured data."""
    data = {
        'timestamps': [],
        'total_mb': [],
        'used_mb': [],
        'free_mb': [],
        'cached_mb': [],
        'cpu_percent': [],
        'rust_processes': []
    }
    
    try:
        with open(csv_file, 'r') as f:
            reader = csv.DictReader(f)
            for row in reader:
                try:
                    # Parse timestamp
                    timestamp = datetime.strptime(row['Timestamp'], '%Y-%m-%d %H:%M:%S')
                    data['timestamps'].append(timestamp)
                    
                    # Parse numeric data
                    data['total_mb'].append(float(row['Total_MB']))
                    data['used_mb'].append(float(row['Used_MB']))
                    data['free_mb'].append(float(row['Free_MB']))
                    data['cached_mb'].append(float(row['Cached_MB']))
                    data['cpu_percent'].append(float(row['CPU_%']))
                    data['rust_processes'].append(int(row['Rust_Processes']))
                except (ValueError, KeyError) as e:
                    print(f"Warning: Skipping malformed row: {row}")
                    continue
                    
    except FileNotFoundError:
        print(f"Error: File not found: {csv_file}")
        sys.exit(1)
    except Exception as e:
        print(f"Error reading file: {e}")
        sys.exit(1)
    
    return data

def calculate_memory_stats(data):
    """Calculate memory growth statistics."""
    if len(data['used_mb']) < 2:
        return None
    
    used_mb = np.array(data['used_mb'])
    timestamps = data['timestamps']
    
    # Calculate total growth
    initial_memory = used_mb[0]
    final_memory = used_mb[-1]
    total_growth = final_memory - initial_memory
    
    # Calculate time duration
    duration_seconds = (timestamps[-1] - timestamps[0]).total_seconds()
    duration_hours = duration_seconds / 3600.0
    
    # Calculate growth rates
    mb_per_hour = total_growth / duration_hours if duration_hours > 0 else 0
    mb_per_day = mb_per_hour * 24
    gb_per_day = mb_per_day / 1024
    
    # Linear regression for trend analysis
    time_points = np.array([(t - timestamps[0]).total_seconds() for t in timestamps])
    slope, intercept = np.polyfit(time_points, used_mb, 1)
    r_squared = np.corrcoef(time_points, used_mb)[0, 1] ** 2
    
    # Calculate variance and stability
    memory_variance = np.var(used_mb)
    memory_std = np.std(used_mb)
    coefficient_of_variation = memory_std / np.mean(used_mb) if np.mean(used_mb) > 0 else 0
    
    return {
        'initial_memory_mb': initial_memory,
        'final_memory_mb': final_memory,
        'total_growth_mb': total_growth,
        'duration_hours': duration_hours,
        'growth_mb_per_hour': mb_per_hour,
        'growth_mb_per_day': mb_per_day,
        'growth_gb_per_day': gb_per_day,
        'linear_slope': slope,
        'r_squared': r_squared,
        'memory_variance': memory_variance,
        'memory_std': memory_std,
        'coefficient_of_variation': coefficient_of_variation,
        'is_concerning': abs(mb_per_day) > 100  # > 100 MB/day is concerning
    }

def detect_anomalies(data, window_size=5, threshold=2.0):
    """Detect memory usage anomalies using moving average and standard deviation."""
    used_mb = np.array(data['used_mb'])
    anomalies = []
    
    if len(used_mb) < window_size:
        return anomalies
    
    for i in range(window_size, len(used_mb)):
        window = used_mb[i-window_size:i]
        mean = np.mean(window)
        std = np.std(window)
        
        if std > 0 and abs(used_mb[i] - mean) > threshold * std:
            anomalies.append({
                'index': i,
                'timestamp': data['timestamps'][i],
                'memory_mb': used_mb[i],
                'expected_mb': mean,
                'deviation_sigma': abs(used_mb[i] - mean) / std
            })
    
    return anomalies

def create_memory_plot(data, stats, anomalies, output_file):
    """Create a comprehensive memory usage plot."""
    fig, ((ax1, ax2), (ax3, ax4)) = plt.subplots(2, 2, figsize=(15, 10))
    fig.suptitle('Memory Usage Analysis', fontsize=16)
    
    timestamps = data['timestamps']
    
    # Plot 1: Memory usage over time
    ax1.plot(timestamps, data['used_mb'], label='Used Memory', color='red', linewidth=2)
    ax1.plot(timestamps, data['free_mb'], label='Free Memory', color='green', alpha=0.7)
    ax1.plot(timestamps, data['cached_mb'], label='Cached Memory', color='blue', alpha=0.7)
    
    # Add trend line
    if stats:
        time_points = np.array([(t - timestamps[0]).total_seconds() for t in timestamps])
        trend_line = stats['linear_slope'] * time_points + stats['initial_memory_mb']
        ax1.plot(timestamps, trend_line, '--', color='black', alpha=0.8, 
                label=f'Trend (R²={stats["r_squared"]:.3f})')
    
    # Mark anomalies
    if anomalies:
        anomaly_times = [a['timestamp'] for a in anomalies]
        anomaly_values = [a['memory_mb'] for a in anomalies]
        ax1.scatter(anomaly_times, anomaly_values, color='orange', s=50, 
                   label=f'Anomalies ({len(anomalies)})', zorder=5)
    
    ax1.set_title('Memory Usage Over Time')
    ax1.set_xlabel('Time')
    ax1.set_ylabel('Memory (MB)')
    ax1.legend()
    ax1.grid(True, alpha=0.3)
    ax1.xaxis.set_major_formatter(mdates.DateFormatter('%H:%M:%S'))
    plt.setp(ax1.xaxis.get_majorticklabels(), rotation=45)
    
    # Plot 2: Memory growth rate
    if len(data['used_mb']) > 1:
        growth_rates = []
        growth_times = []
        for i in range(1, len(data['used_mb'])):
            dt = (timestamps[i] - timestamps[i-1]).total_seconds() / 3600.0  # hours
            if dt > 0:
                rate = (data['used_mb'][i] - data['used_mb'][i-1]) / dt  # MB/hour
                growth_rates.append(rate)
                growth_times.append(timestamps[i])
        
        ax2.plot(growth_times, growth_rates, color='purple', linewidth=1.5)
        ax2.axhline(y=0, color='black', linestyle='-', alpha=0.3)
        ax2.axhline(y=100/24, color='red', linestyle='--', alpha=0.7, label='Warning Threshold')
        ax2.set_title('Memory Growth Rate')
        ax2.set_xlabel('Time')
        ax2.set_ylabel('Growth Rate (MB/hour)')
        ax2.legend()
        ax2.grid(True, alpha=0.3)
        ax2.xaxis.set_major_formatter(mdates.DateFormatter('%H:%M:%S'))
        plt.setp(ax2.xaxis.get_majorticklabels(), rotation=45)
    
    # Plot 3: CPU usage correlation
    ax3.plot(timestamps, data['cpu_percent'], color='orange', linewidth=1.5, label='CPU Usage')
    ax3_twin = ax3.twinx()
    ax3_twin.plot(timestamps, data['used_mb'], color='red', alpha=0.7, label='Memory Usage')
    ax3.set_title('CPU vs Memory Usage')
    ax3.set_xlabel('Time')
    ax3.set_ylabel('CPU Usage (%)', color='orange')
    ax3_twin.set_ylabel('Memory (MB)', color='red')
    ax3.grid(True, alpha=0.3)
    ax3.xaxis.set_major_formatter(mdates.DateFormatter('%H:%M:%S'))
    plt.setp(ax3.xaxis.get_majorticklabels(), rotation=45)
    
    # Plot 4: Process count and memory efficiency
    ax4.plot(timestamps, data['rust_processes'], color='green', linewidth=2, label='Rust Processes')
    if data['rust_processes'] and max(data['rust_processes']) > 0:
        memory_per_process = [m/p if p > 0 else 0 for m, p in zip(data['used_mb'], data['rust_processes'])]
        ax4_twin = ax4.twinx()
        ax4_twin.plot(timestamps, memory_per_process, color='blue', alpha=0.7, label='Memory per Process')
        ax4_twin.set_ylabel('Memory per Process (MB)', color='blue')
    
    ax4.set_title('Process Count and Memory Efficiency')
    ax4.set_xlabel('Time')
    ax4.set_ylabel('Process Count', color='green')
    ax4.grid(True, alpha=0.3)
    ax4.xaxis.set_major_formatter(mdates.DateFormatter('%H:%M:%S'))
    plt.setp(ax4.xaxis.get_majorticklabels(), rotation=45)
    
    plt.tight_layout()
    plt.savefig(output_file, dpi=300, bbox_inches='tight')
    print(f"Memory usage plot saved to: {output_file}")

def generate_report(data, stats, anomalies, output_file):
    """Generate a detailed analysis report."""
    report = {
        'analysis_timestamp': datetime.now().isoformat(),
        'data_points': len(data['timestamps']),
        'time_range': {
            'start': data['timestamps'][0].isoformat() if data['timestamps'] else None,
            'end': data['timestamps'][-1].isoformat() if data['timestamps'] else None
        },
        'memory_statistics': stats,
        'anomalies': [
            {
                'timestamp': a['timestamp'].isoformat(),
                'memory_mb': a['memory_mb'],
                'expected_mb': a['expected_mb'],
                'deviation_sigma': a['deviation_sigma']
            } for a in anomalies
        ],
        'summary': {}
    }
    
    if stats:
        # Generate summary
        if stats['is_concerning']:
            status = "⚠️ CONCERNING"
            message = f"Memory growth rate of {stats['growth_gb_per_day']:.2f} GB/day exceeds threshold"
        elif stats['growth_gb_per_day'] > 0.5:
            status = "🔍 MONITORING RECOMMENDED"
            message = f"Memory growth rate of {stats['growth_gb_per_day']:.2f} GB/day requires monitoring"
        else:
            status = "✅ NORMAL"
            message = f"Memory growth rate of {stats['growth_gb_per_day']:.2f} GB/day appears normal"
        
        report['summary'] = {
            'status': status,
            'message': message,
            'growth_rate_gb_per_day': stats['growth_gb_per_day'],
            'trend_strength': stats['r_squared'],
            'anomaly_count': len(anomalies),
            'memory_stability': 'High' if stats['coefficient_of_variation'] < 0.05 else 
                               'Medium' if stats['coefficient_of_variation'] < 0.15 else 'Low'
        }
    
    # Save report
    with open(output_file, 'w') as f:
        json.dump(report, f, indent=2)
    
    print(f"Analysis report saved to: {output_file}")
    return report

def print_summary(stats, anomalies):
    """Print a human-readable summary to console."""
    print("\n" + "="*60)
    print("MEMORY LEAK ANALYSIS SUMMARY")
    print("="*60)
    
    if not stats:
        print("❌ Insufficient data for analysis")
        return
    
    print(f"📊 Test Duration: {stats['duration_hours']:.2f} hours")
    print(f"💾 Memory Change: {stats['initial_memory_mb']:.1f} MB → {stats['final_memory_mb']:.1f} MB")
    print(f"📈 Total Growth: {stats['total_growth_mb']:+.1f} MB")
    print(f"⏱️  Growth Rate: {stats['growth_mb_per_hour']:+.2f} MB/hour ({stats['growth_gb_per_day']:+.3f} GB/day)")
    print(f"📉 Trend Strength: R² = {stats['r_squared']:.3f}")
    print(f"🔍 Memory Stability: CV = {stats['coefficient_of_variation']:.3f}")
    
    if anomalies:
        print(f"⚠️  Anomalies Detected: {len(anomalies)}")
        for i, anomaly in enumerate(anomalies[:5]):  # Show first 5
            print(f"   {i+1}. {anomaly['timestamp'].strftime('%H:%M:%S')}: "
                  f"{anomaly['memory_mb']:.1f} MB ({anomaly['deviation_sigma']:.1f}σ)")
        if len(anomalies) > 5:
            print(f"   ... and {len(anomalies)-5} more")
    
    print("\n" + "="*60)
    if stats['is_concerning']:
        print("🚨 RESULT: POTENTIAL MEMORY LEAK DETECTED")
        print(f"   Growth rate of {stats['growth_gb_per_day']:.3f} GB/day exceeds threshold")
        print("   Recommend further investigation and longer test runs")
    else:
        print("✅ RESULT: Memory usage appears normal")
        print("   No significant memory leak detected")
    print("="*60)

def main():
    parser = argparse.ArgumentParser(description='Visualize memory usage from concurrency manager tests')
    parser.add_argument('csv_file', help='Path to memory CSV file')
    parser.add_argument('--output-dir', default='.', help='Output directory for plots and reports')
    parser.add_argument('--no-plot', action='store_true', help='Skip plot generation')
    parser.add_argument('--threshold', type=float, default=2.0, help='Anomaly detection threshold (sigma)')
    
    args = parser.parse_args()
    
    # Parse data
    print(f"📊 Analyzing memory data from: {args.csv_file}")
    data = parse_memory_csv(args.csv_file)
    
    if not data['timestamps']:
        print("❌ No valid data found in CSV file")
        sys.exit(1)
    
    print(f"✅ Loaded {len(data['timestamps'])} data points")
    
    # Calculate statistics
    stats = calculate_memory_stats(data)
    
    # Detect anomalies
    anomalies = detect_anomalies(data, threshold=args.threshold)
    
    # Generate output file names
    base_name = Path(args.csv_file).stem
    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    
    plot_file = output_dir / f"{base_name}_analysis.png"
    report_file = output_dir / f"{base_name}_report.json"
    
    # Create plot
    if not args.no_plot:
        try:
            create_memory_plot(data, stats, anomalies, plot_file)
        except ImportError:
            print("⚠️  matplotlib not available, skipping plot generation")
        except Exception as e:
            print(f"⚠️  Error creating plot: {e}")
    
    # Generate report
    report = generate_report(data, stats, anomalies, report_file)
    
    # Print summary
    print_summary(stats, anomalies)
    
    return 0 if not stats or not stats['is_concerning'] else 1

if __name__ == '__main__':
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("\n❌ Analysis interrupted by user")
        sys.exit(1)
    except Exception as e:
        print(f"❌ Unexpected error: {e}")
        sys.exit(1) 