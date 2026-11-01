//! Runtime CPU feature dispatch (with a test hook to force lower SIMD levels).

use std::sync::atomic::{AtomicU8, Ordering::Relaxed};

static LIMIT: AtomicU8 = AtomicU8::new(2);

/// Limit the SIMD level used by the kernels: 0 = portable code, 1 = AVX2, 2 = AVX-512.
/// Intended for tests and benchmarks; results are identical at every level.
pub fn set_simd_limit(level: u8) {
    LIMIT.store(level, Relaxed);
}

/// Currently allowed SIMD level.
pub fn simd_limit() -> u8 {
    LIMIT.load(Relaxed)
}

#[inline(always)]
pub(crate) fn avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        LIMIT.load(Relaxed) >= 1 && std::arch::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

#[inline(always)]
pub(crate) fn avx512() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        LIMIT.load(Relaxed) >= 2
            && std::arch::is_x86_feature_detected!("avx512f")
            && std::arch::is_x86_feature_detected!("avx512bw")
            && std::arch::is_x86_feature_detected!("avx512vl")
            && std::arch::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}
