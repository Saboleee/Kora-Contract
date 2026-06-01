#![no_std]

//! # Kora Shared Library — Audit Findings
//!
//! ## Summary of Findings and Fixes
//!
//! ### 1. Missing Doc Comments on Validation Helpers (validation.rs)
//! - **Issue:** All public validation functions lacked doc comments
//! - **Fix:** AUDIT FIX: Added comprehensive /// doc comments to every public function
//! - **Severity:** Medium — Documentation completeness
//!
//! ### 2. Incorrect Error Type for Empty Bytes (validation.rs:39-43)
//! - **Issue:** `require_non_empty_bytes()` returned `EmptyString` error (semantically wrong for bytes)
//! - **Fix:** AUDIT FIX: Changed to return dedicated `EmptyBytes` error for semantic clarity
//! - **Severity:** Low — Error categorization/semantics
//!
//! All arithmetic operations use checked methods. All type definitions are well-documented.
//! Error enum is comprehensive and specific.

pub mod errors;
pub mod events;
pub mod reentrancy;
pub mod types;
pub mod validation;
