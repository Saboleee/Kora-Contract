# Kora Protocol — Contract Improvements

This document outlines optimizations, edge case resolutions, and documentation enhancements for all contracts in the Kora Protocol.

## Issue #119: Marketplace Contracts Documentation & Optimization

### Objectives
- Enhance inline documentation for all public functions
- Resolve edge cases in fee calculation and fund release logic
- Optimize storage access patterns
- Add edge case test coverage

### Key Improvements

#### 1. Fee Calculation Robustness
- **Issue**: Fee calculation can silently round down, potentially losing dust amounts
- **Fix**: Add explicit handling for fractional amounts
  ```rust
  let fee = bps_of(amount, fee_bps)?;
  let net = amount.checked_sub(fee).ok_or(KoraError::ArithmeticOverflow)?;
  // Validate that fee + net == amount (no silent rounding loss)
  if fee.checked_add(net)? != amount {
      return Err(KoraError::ArithmeticOverflow);
  }
  ```

#### 2. Listing Lifecycle Edge Cases
- **Issue**: Listing can be cancelled while funding is in progress
- **Fix**: Add state checks to prevent cancellation during active funding
- **Impact**: Prevents loss of investor funds due to race conditions

#### 3. Cross-Contract Call Ordering
- **Issue**: Fee transfer happens before pool update in fund_invoice
- **Fix**: Reorder to checks-effects-interactions pattern:
  1. Validate all inputs
  2. Update all state (listing.funded_amount)
  3. Make token transfers
  4. Make cross-contract calls
- **Security**: Prevents reentrancy even in Soroban's synchronous model

#### 4. Token Whitelist Validation
- **Issue**: No maximum count limit on whitelisted tokens
- **Fix**: Add per-listing token validation in fund_invoice
- **Impact**: Prevents accidental funding of wrong token

#### 5. Improved Error Messages
- **Issue**: Generic error types don't indicate root cause
- **Fix**: Add context-specific error types for:
  - `ListingAlreadyFunded` (attempting to fund a fully-funded listing)
  - `FundingTargetExceeded` (funding amount exceeds asking price)
  - `InvalidFundingDeadline` (deadline not in future)

---

## Issue #114: Financing Pool Contracts Optimization

### Objectives
- Complete audit fixes documentation
- Resolve any remaining arithmetic edge cases
- Optimize yield distribution logic
- Add property-based tests

### Key Improvements

#### 1. Yield Distribution Precision
- **Current**: Share calculation uses `(contributed / total_funded) × 10_000`
- **Issue**: Small positions might lose precision due to integer division
- **Fix**: Add explicit precision handling:
  ```rust
  let share_bps = (position.contributed * 10_000)
      .checked_div(pool.face_value)?;
  let payout = (pool.repaid_amount * share_bps)
      .checked_div(10_000)?;
  ```
- **Test**: Add tests for positions with non-divisible amounts

#### 2. Release Funds Edge Case
- **Issue**: release_funds called twice could double-release
- **Current Fix**: is_closed flag prevents this, but could be clearer
- **Improvement**: Add explicit guard with custom error type
  ```rust
  if pool.is_closed {
      return Err(KoraError::PoolAlreadyClosed);
  }
  ```

#### 3. Repayment Lock Cleanup
- **Current**: RepaymentLock is removed on success
- **Edge Case**: Lock not cleared on certain error paths
- **Fix**: Use guard pattern consistently across all error paths
- **Test**: Add tests for lock cleanup on error

#### 4. Position Recording Atomicity
- **Issue**: record_position updates pool and creates position in separate storage calls
- **Fix**: Use transaction-like semantics by checking both succeed:
  ```rust
  pool.total_funded = pool.total_funded.checked_add(amount)?;
  env.storage().persistent().set(&DataKey::Pool(invoice_id), &pool);
  
  let position = Position { ... };
  env.storage().persistent().set(&DataKey::Position(invoice_id, investor), &position);
  ```

#### 5. MAX_AMOUNT Validation
- **Improvement**: Add explicit constant and validation
  ```rust
  const MAX_AMOUNT: i128 = 1_000_000_000_000_000i128; // 1 trillion
  
  if amount > MAX_AMOUNT {
      return Err(KoraError::InvalidAmount);
  }
  ```

---

## Issue #110: Invoice NFT Contracts Optimization

### Objectives
- Optimize migration logic
- Add comprehensive status transition tests
- Document state machine guarantees
- Add immutability proofs

### Key Improvements

#### 1. Migration Logic Enhancement
- **Current**: migrate() is idempotent but minimal
- **Improvement**: Add version-based migration path for future upgrades:
  ```rust
  match current_version {
      0 => { /* initialize to v1 */ }
      1 => { /* no-op, already v1 */ }
      _ => Err(KoraError::InvalidMigrationVersion)
  }
  ```

#### 2. Status Transition Validation
- **Current**: Each transition checks only immediate previous state
- **Improvement**: Add comprehensive transition table:
  ```
  Created → Listed (by marketplace)
  Listed → Funded (by financing_pool)
  Funded → Repaid (by financing_pool)
  Funded → Defaulted (by admin, after due_date)
  ```
- **Test**: Add parameterized tests for all valid/invalid transitions

#### 3. Immutability Enforcement
- **Current**: Invoice is persistent, but fields are not explicitly marked
- **Improvement**: Add comment documenting which fields are immutable:
  ```
  // Immutable fields (set at creation, never modified):
  // - id, sme, debtor_hash, amount, currency, due_date, ipfs_cid, created_at
  //
  // Mutable fields (status, funded_at, repaid_at)
  ```
- **Test**: Add test_invoice_immutability_after_status_change

#### 4. Overflow Prevention
- **Current**: ID counter uses checked_add
- **Enhancement**: Add graceful handling for ID overflow:
  ```rust
  let next_id: u64 = env.storage().instance().get(&DataKey::NextId).unwrap_or(1);
  if next_id == u64::MAX {
      return Err(KoraError::ContractCapacityExceeded);
  }
  ```

#### 5. TTL Management Documentation
- **Improvement**: Add detailed comments on TTL requirements:
  ```
  // Persistent storage entries (Invoices) require manual TTL extension
  // Recommended: Keeper bot extends TTL daily to maintain ~30-day window
  // See docs/ARCHITECTURE.md for TTL constants and management strategy
  ```

---

## Issue #118: Access Control Contracts Edge Cases

### Objectives
- Resolve role transfer edge cases
- Enhance pause/unpause state validation
- Add comprehensive authorization tests
- Document security properties

### Key Improvements

#### 1. Role Transition Edge Cases
- **Current**: grant_role prevents granting to admin, but edge case exists
- **Issue**: Admin could previously grant themselves a different role
- **Fix**: Prevent any role assignment that conflicts with admin status:
  ```rust
  if target == admin {
      return Err(KoraError::CannotModifyAdminRole);
  }
  ```

#### 2. Transfer Admin Validation
- **Current**: Checks for existing role conflicts
- **Enhancement**: Add explicit validation:
  ```rust
  if current_admin == new_admin {
      return Err(KoraError::InvalidAddress);
  }
  if new_admin is_zero_address() {
      return Err(KoraError::InvalidAddress);
  }
  ```

#### 3. Pause State Consistency
- **Current**: Pause/unpause use instance storage
- **Edge Case**: Pause state might not persist across contract upgrades
- **Fix**: Document this limitation and add comments:
  ```rust
  // Pause flag stored in instance storage (survives ledger archival but not contract upgrade)
  // Protocol upgrade requires re-initialization of pause state
  ```

#### 4. Role Revocation Edge Cases
- **Current**: revoke_role prevents revoking admin
- **Enhancement**: Add safeguard preventing admin self-revocation:
  ```rust
  if current_role == Role::Admin && target == admin {
      return Err(KoraError::CannotRevokeAdmin);
  }
  ```

#### 5. Cross-Role Authorization Consistency
- **Issue**: Different contracts check roles independently
- **Improvement**: Add documentation of expected role behavior:
  ```rust
  /// Role-based access control:
  /// - Admin: Full protocol control (pause, fees, roles, emergency ops)
  /// - Operator: Keeper operations (future use)
  /// - Verifier: Risk scoring and SME registration (risk_registry)
  /// - None: No privileged access
  ```

---

## Common Improvements Across All Contracts

### 1. Comprehensive Documentation
- **Add**: Detailed doc comments for all public functions
- **Format**: Include parameters, returns, errors, and security notes
- **Example**:
  ```rust
  /// Process a repayment for an invoice.
  ///
  /// Updates the pool's repaid amount and distributes yield to investors.
  /// Once fully repaid, marks the invoice as Repaid and closes the pool.
  ///
  /// # Parameters
  /// - `payer` — The SME making the repayment (must sign transaction)
  /// - `invoice_id` — ID of the invoice being repaid
  /// - `token` — Token address (must match pool's token)
  /// - `amount` — Repayment amount in base units
  ///
  /// # Returns
  /// - `Ok(())` if repayment succeeds
  /// - `Err(KoraError::PoolNotFound)` if pool doesn't exist
  /// - `Err(KoraError::RepaymentAlreadyMade)` if pool is already closed
  /// - `Err(KoraError::InvalidAmount)` if amount ≤ 0
  ///
  /// # Security
  /// - Requires `payer` authentication via `require_auth()`
  /// - Protected from reentrancy via RepaymentLock
  /// - Uses checks-effects-interactions pattern
  /// - Repayment succeeds even if protocol is paused
  pub fn repay(env: Env, payer: Address, invoice_id: u64, ...) -> Result<(), KoraError>
  ```

### 2. Enhanced Test Coverage
- **Goal**: 95%+ coverage for new code
- **Add**: Tests for:
  - All error paths
  - Boundary values (0, MAX_VALUE, MIN_VALUE)
  - State transitions
  - Concurrent operations (where applicable)
  - Fee calculations with various basis points

### 3. Arithmetic Validation
- **Pattern**: Use `checked_*` for all arithmetic
- **Validation**: Ensure no silent rounding or loss of precision
- **Tests**: Add fuzzing tests for arithmetic edge cases

### 4. Cross-Contract Safety
- **Pattern**: Always validate caller contract address
- **Documentation**: Explicitly note which contracts can call which functions
- **Future**: Add optional marketplace address storage in financing_pool for v2

### 5. Storage Efficiency
- **Review**: Minimize storage reads for hot paths
- **Cache**: Load full config once per function vs. individual fields
- **TTL**: Ensure all persistent keys have proper TTL management

---

## Testing Strategy

### Unit Tests
- Individual function correctness
- Error path coverage
- Boundary value testing

### Integration Tests
- Cross-contract interactions
- End-to-end workflows
- Fee collection flows

### Property-Based Tests
- Arithmetic properties (commutativity, associativity where applicable)
- Invariant preservation (total invested = pool balance, etc.)

### Snapshot Tests
- Test snapshots in `/contracts/*/test_snapshots/` ensure exact behavior
- Run: `cargo test --all`
- Review diffs on test changes

---

## Implementation Priority

1. **Phase 1** (Critical): Fix any remaining arithmetic edge cases
2. **Phase 2** (High): Add comprehensive documentation
3. **Phase 3** (Medium): Optimize hot paths
4. **Phase 4** (Low): Add stretch tests for corner cases

---

## Validation Checklist

Before submitting each PR:
- [ ] All public functions have comprehensive doc comments
- [ ] No unsafe arithmetic operations (all use checked_*)
- [ ] All error paths are tested
- [ ] Test coverage ≥ 95% for modified code
- [ ] `make fmt` runs without warnings
- [ ] `make lint` passes with no clippy warnings
- [ ] `make test` passes all tests
- [ ] PR description includes `Closes #{issue_number}`
