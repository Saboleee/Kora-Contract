# Kora Protocol — Audit Log

This file transcribes the embedded audit findings that were documented inline in
`contracts/financing_pool/src/lib.rs` and `contracts/risk_registry/src/lib.rs`
during the internal code-review pass. Findings have since been resolved; this
document records what was found, the severity, and how each issue was fixed so
the history is traceable without reading git blame.

See [CONTRIBUTING.md](CONTRIBUTING.md) for how to report new security issues.

---

## Financing Pool (`contracts/financing_pool`)

Source: doc-block removed after remediation (commit `b834849` and `75a2367`).

| # | Finding | Severity | Status |
|---|---------|----------|--------|
| FP-01 | Unsafe `unwrap()` calls on storage reads (lines ~63, ~188, ~266) | Medium | Fixed |
| FP-02 | CEI violation: token transfer occurred before state update in `repay()` | High | Fixed |
| FP-03 | Missing reentrancy guard in `repay()` — no lock to prevent concurrent transfers | High | Fixed |
| FP-04 | Reentrancy guard used wrong error type (`ProtocolPaused` instead of a dedicated error) | Low | Fixed |
| FP-05 | Missing pause checks in `release_funds`, `record_position`, `repay`, `mark_default` | Medium | Fixed |
| FP-06 | Duplicate amount validation — same check performed twice | Low | Fixed |
| FP-07 | Silent arithmetic failure: `unwrap_or(0)` in yield calculation hid underflow | Medium | Fixed |
| FP-08 | Missing initialization check in `release_funds` — pool could be double-created | Medium | Fixed |
| FP-09 | Incomplete event emissions — `release_funds` and `record_position` emitted no events | Low | Fixed |
| FP-10 | No upper-bound validation on amounts (overflow risk on `i128` arithmetic) | Medium | Fixed |
| FP-11 | No cross-contract validation that the `marketplace` caller is the registered marketplace | Low | Deferred to v2 |
| FP-12 | Pool `token` field initialized as a placeholder instead of the value passed by marketplace | Medium | Fixed |

### Detail

**FP-01 — Unsafe unwrap().**
Storage reads in `release_funds`, `record_position`, and `repay` called `.unwrap()` directly.
On a cold contract address (not yet initialized) this would trap the transaction with no
meaningful error. Fixed by replacing every storage read with `.ok_or(KoraError::NotInitialized)?`.

**FP-02 — CEI violation in repay().**
`pool.repaid_amount` was updated *after* the `token::Client::transfer` call, violating the
Checks-Effects-Interactions pattern. A reentering call during the transfer could exploit the
stale state. Fixed by moving all state writes before the cross-contract transfer.

**FP-03 — Missing reentrancy guard in repay().**
Even with the CEI fix, `repay()` lacked a storage-backed lock. Added `RepaymentLock(u64)`
persistent storage entry that is set before and cleared after the function body, causing any
re-entrant `repay()` call on the same invoice to return `Unauthorized`.

**FP-04 — Wrong error type for reentrancy guard.**
The guard initially returned `KoraError::ProtocolPaused` when triggered. Fixed by using the
dedicated `KoraError::Reentrancy` variant (added in `kora_shared::errors`).

**FP-05 — Missing pause checks.**
`release_funds`, `record_position`, `repay`, and `mark_default` did not call
`require_not_paused` before mutating state. A paused protocol would still process these calls.
Fixed by adding `Self::require_not_paused(&env)?` at the top of each function.

**FP-06 — Duplicate amount validation.**
`record_position` checked `contributed <= 0` twice in sequence. Removed the duplicate.

**FP-07 — Silent arithmetic failure.**
Yield-available calculation used `saturating_sub(...).unwrap_or(0)`, which silently returned
zero on underflow. Fixed by using `checked_sub` and propagating an
`KoraError::ArithmeticUnderflow` error.

**FP-08 — Missing initialization check in release_funds.**
`release_funds` did not verify that a pool for the given `invoice_id` didn't already exist,
allowing a second call to overwrite live pool state (including wiping `total_funded`).
Fixed by returning `KoraError::PoolAlreadyClosed` if the pool key is already present.

**FP-09 — Incomplete event emissions.**
State transitions in `release_funds` (pool opened) and `record_position` (position recorded)
emitted no events, making on-chain indexing and audit-trail reconstruction impossible.
Fixed by adding `events::pool_opened` and `events::position_recorded` calls.

**FP-10 — No upper-bound validation on amounts.**
`i128` arithmetic on large unchecked inputs can overflow. Added `MAX_AMOUNT = i128::MAX / 2`
constant and validated all user-supplied amounts against it. Later centralized into
`kora_shared::validation` (see issue #230).

**FP-11 — No cross-contract validation of marketplace caller.**
`release_funds` trusts `marketplace.require_auth()` but does not check that `marketplace`
matches the stored marketplace address (because no such address is stored). Documented as
a known gap; a registry of authorized callers is planned for v2.

**FP-12 — Pool token initialized as placeholder.**
The pool `token` field was hardcoded to a placeholder instead of using the `token: Address`
argument passed by the marketplace. Fixed by storing `token.clone()` in the `Pool` struct.

---

## Risk Registry (`contracts/risk_registry`)

Source: doc-block removed after remediation (commit `3074b19`).

| # | Finding | Severity | Status |
|---|---------|----------|--------|
| RR-01 | `increment_invoice_count()` modified SME profile without emitting an event | Medium | Fixed |
| RR-02 | Empty-bytes validation used `EmptyString` error (semantically wrong type) | Low | Fixed |

### Detail

**RR-01 — Missing event on invoice count increment.**
`increment_invoice_count` updated `SmeProfile.total_invoices` in persistent storage without
emitting any event. Indexers watching the event log would miss the state change, making
audit trails incomplete. Fixed by adding an `events::sme_invoice_count_incremented` call
immediately after the storage write.

**RR-02 — Wrong error variant for empty debtor hash.**
`set_debtor_score` validated that `debtor_hash` was non-empty but returned
`KoraError::EmptyString` on failure — a type intended for `String` fields, not `Bytes`.
Fixed by switching to `KoraError::EmptyBytes`, and later upgraded to
`KoraError::InvalidLength` when exact 32-byte enforcement was added (see issue #229).

---

## How to report new findings

See the **Security Vulnerabilities** section in [CONTRIBUTING.md](CONTRIBUTING.md).
Do not open public GitHub issues for exploitable vulnerabilities — use the private
contact listed there.
