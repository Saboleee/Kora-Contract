#![no_std]

//! # Kora Shared Library
//!
//! Common types, errors, events, and validation utilities for the Kora Protocol.
//!
//! ## Modules
//! - `types`      — Core on-chain data structures (Invoice, Listing, Pool, etc.)
//! - `errors`     — Protocol-wide error enum (`KoraError`)
//! - `events`     — Standardized event emission helpers
//! - `validation` — Input validation and safe arithmetic helpers
//! - `reentrancy` — RAII reentrancy guard

pub mod errors;
pub mod events;
pub mod reentrancy;
pub mod types;
pub mod validation;
