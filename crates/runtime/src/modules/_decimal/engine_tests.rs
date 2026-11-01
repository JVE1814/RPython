//! Standalone test root for the decimal arithmetic engine (no runtime dependency):
//!   crates/runtime/src/modules/_decimal/run_engine_tests.sh
//! Not part of the crate module tree.
#![allow(dead_code)]
#[path = "nat.rs"]
mod nat;
#[path = "ntt.rs"]
mod ntt;
#[path = "dec.rs"]
mod dec;
#[path = "ops.rs"]
mod ops;
#[path = "transc.rs"]
mod transc;
#[path = "dectest.rs"]
mod dectest;
