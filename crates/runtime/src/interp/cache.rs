//! Per-code side tables (HotSpot "MethodData"/constant-pool-cache analogue).
//! Tier 0 keeps simple global-load caches keyed by dict versions; type profiles
//! for the JIT tiers are added here.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};

pub struct GlobalCacheEntry {
    pub globals_version: AtomicU64,
    pub builtins_version: AtomicU64,
    /// borrowed pointer to the value (valid while versions match)
    pub value: AtomicUsize,
}

pub struct CodeCaches {
    pub globals: Box<[GlobalCacheEntry]>,
}

impl CodeCaches {
    pub fn new(ninstr: usize) -> CodeCaches {
        let _ = ninstr;
        CodeCaches { globals: Box::new([]) }
    }
}

impl GlobalCacheEntry {
    pub fn get(&self, gv: u64, bv: u64) -> Option<usize> {
        if self.globals_version.load(Acquire) == gv && self.builtins_version.load(Acquire) == bv {
            let v = self.value.load(Acquire);
            if v != 0 {
                return Some(v);
            }
        }
        None
    }
    pub fn set(&self, gv: u64, bv: u64, v: usize) {
        self.value.store(v, Release);
        self.globals_version.store(gv, Release);
        self.builtins_version.store(bv, Release);
    }
}
