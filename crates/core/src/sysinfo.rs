//! 进程级资源采集。
//!
//! M0 只需要 RSS 一个指标，因此不引入 `sysinfo` 这类重依赖，
//! 直接在 Windows 上调 `K32GetProcessMemoryInfo`（Windows 7+ 由 kernel32 导出）。

/// 当前进程的常驻内存（MB）。采集失败时返回 `0.0`。
#[cfg(windows)]
pub fn current_rss_mb() -> f64 {
    use std::mem::size_of;

    /// 逐字段对齐 Win32 的 `PROCESS_MEMORY_COUNTERS`。
    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(
            process: isize,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    // SAFETY: `cb` 精确声明了结构体尺寸，且结构布局与 Win32 头文件一致；
    // 指针指向栈上有效对象，调用期间不会被移动。
    unsafe {
        let mut counters = ProcessMemoryCounters {
            cb: size_of::<ProcessMemoryCounters>() as u32,
            ..Default::default()
        };
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) == 0 {
            return 0.0;
        }
        counters.working_set_size as f64 / (1024.0 * 1024.0)
    }
}

/// 非 Windows 平台在 M0 阶段不做采集。
#[cfg(not(windows))]
pub fn current_rss_mb() -> f64 {
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rss_is_non_negative() {
        assert!(current_rss_mb() >= 0.0);
    }
}
