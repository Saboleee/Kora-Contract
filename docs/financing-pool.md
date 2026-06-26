# Financing Pool Contract

The `financing_pool` contract is the custodian of investor funds within the Kora Protocol. It holds tokens contributed by investors, tracks each investor's position, distributes repayments proportionally, and handles defaults.

---

## Overview

When an invoice is fully funded on the marketplace, the marketplace calls `release_funds` on the financing pool. From that point, the pool:

1. Creates a `Pool` record tied to the invoice.
2. Tracks each investor's contributed amount and share (in basis points).
3. Accepts repayment from the SME.
4. Distributes principal + yield to investors proportionally on full repayment.
5. Handles partial distribution in the event of a default.

---

## Storage Layout

| Key | Tier | Type | Description |
|-----|------|------|-------------|
| `Admin` | instance | `Address` | Contract admin |
| `InvoiceNft` | instance | `Address` | Invoice NFT contract address |
| `Treasury` | instance | `Address` | Treasury contract address |
| `AccessControl` | instance | `Address` | Access control contract address |
| `LatePenaltyBps` | instance | `u32` | Late repayment penalty in basis points |
| `Pool(u64)` | persistent | `Pool` | Pool state keyed by invoice ID |
| `Positions(u64)` | persistent | `Map<Address, Position>` | Investor positions keyed by invoice ID |
| `RepaymentLock(u64)` | persistent | `bool` | Reentrancy guard for repayments |

---

## Data Structures

### `Pool`

```rust
pub struct Pool {
    pub invoice_id: u64,
    pub token: Address,        // whitelisted stablecoin
    pub total_funded: i128,    // total contributions received
    pub face_value: i128,      // full repayment amount from invoice
    pub repaid_amount: i128,   // cumulative amount repaid so far
    pub is_closed: bool,       // true once fully repaid or defaulted
    pub late_penalty_bps: u32, // penalty applied to late repayments
}
```

### `Position`

```rust
pub struct Position {
    pub investor: Address,
    pub invoice_id: u64,
    pub contributed: i128, // amount contributed by this investor
    pub share_bps: u32,    // proportional share in basis points (10000 = 100%)
    pub yield_claimed: i128,
}
```

---

## Entry Points

### `initialize`

```rust
pub fn initialize(
    env: Env,
    admin: Address,
    invoice_nft: Address,
    treasury: Address,
    access_control: Address,
    late_penalty_bps: u32,
) -> Result<(), KoraError>
```

One-time initializer. Stores all contract references and configuration.

**Errors:**
- `AlreadyInitialized` — called more than once.
- `InvalidFeeRate` — `late_penalty_bps > 10_000`.

---

### `release_funds`

```rust
pub fn release_funds(
    env: Env,
    marketplace: Address,
    invoice_id: u64,
    token: Address,
) -> Result<(), KoraError>
```

Called by the marketplace when an invoice is fully funded. Creates the `Pool` record and transitions the invoice NFT to `Funded` status.

**Auth:** `marketplace.require_auth()`

**Errors:**
- `PoolAlreadyClosed` — pool already exists for this invoice.
- `InvalidAddress` — token is the contract itself.
- `NotInitialized` — contract not initialized.
- `InvalidAmount` — invoice amount is zero, negative, or exceeds `MAX_AMOUNT`.

---

### `record_position`

```rust
pub fn record_position(
    env: Env,
    caller: Address,
    invoice_id: u64,
    investor: Address,
    contributed: i128,
    total_pool: i128,
) -> Result<(), KoraError>
```

Records or updates an investor's position. Called by the admin (marketplace) after a fund contribution.

The investor's `share_bps` is computed as:

```
share_bps = (contributed × 10_000) / total_pool
```

**Auth:** `caller.require_auth()` + admin check.

**Errors:**
- `NotAdmin` — caller is not the admin.
- `InvalidAmount` — any of: amounts ≤ 0, `contributed > total_pool`, either exceeds `MAX_AMOUNT`.
- `ArithmeticOverflow` — share calculation overflows.

---

### `repay`

```rust
pub fn repay(
    env: Env,
    payer: Address,
    invoice_id: u64,
    token: Address,
    amount: i128,
) -> Result<(), KoraError>
```

SME repays the invoice. Follows the Checks-Effects-Interactions pattern: state is updated before any token transfer. When `repaid_amount >= face_value`, the pool is closed, yield is distributed to all investors, and the invoice NFT is marked `Repaid`.

**Auth:** `payer.require_auth()`

**Reentrancy:** Protected by a per-invoice `RepaymentLock` stored in persistent storage.

**Errors:**
- `InvalidAmount` — amount ≤ 0 or exceeds `MAX_AMOUNT`.
- `Unauthorized` — reentrancy guard is active.
- `PoolNotFound` — no pool exists for this invoice.
- `RepaymentAlreadyMade` — pool is already closed.
- `ArithmeticOverflow` — repaid amount addition overflows.

---

### `mark_default`

```rust
pub fn mark_default(
    env: Env,
    admin: Address,
    invoice_id: u64,
    token: Address,
) -> Result<(), KoraError>
```

Admin-only. Marks an invoice as defaulted and distributes any partial recovery to investors proportionally. Transitions the invoice NFT to `Defaulted`.

**Auth:** `admin.require_auth()` + admin check.

**Errors:**
- `NotAdmin` — caller is not admin.
- `Unauthorized` — reentrancy guard is active.
- `PoolNotFound` — no pool exists for this invoice.
- `PoolAlreadyClosed` — pool is already closed.

---

### `get_pool`

```rust
pub fn get_pool(env: Env, invoice_id: u64) -> Result<Pool, KoraError>
```

Returns the `Pool` struct for the given invoice. Read-only, no auth required.

**Errors:** `PoolNotFound`

---

### `get_positions`

```rust
pub fn get_positions(env: Env, invoice_id: u64) -> Vec<Position>
```

Returns all investor positions for the given invoice. Returns an empty vec if none exist. Read-only, no auth required.

---

## Yield Distribution

Yield is distributed when `repaid_amount >= face_value`. Each investor receives:

```
payout     = (total_repaid × share_bps) / 10_000
yield      = payout - contributed
```

Safe arithmetic (`checked_mul`, `checked_div`, `checked_sub`) is used throughout. Any overflow returns `KoraError::ArithmeticOverflow`.

---

## Security Properties

- **Checks-Effects-Interactions:** In `repay`, all state changes (`pool.repaid_amount`, `pool.is_closed`) are written to storage **before** any token transfer occurs.
- **Reentrancy guard:** A per-invoice `RepaymentLock` key prevents concurrent reentrant repayment calls.
- **Safe arithmetic:** All financial math uses `checked_*` operations. No silent overflows or `unwrap()` on arithmetic results.
- **Bounds validation:** All input amounts are validated against `MAX_AMOUNT = i128::MAX / 2` to prevent overflow in downstream calculations.
- **No unauthorized storage access:** Cross-contract calls use `require_auth()` so only authorized callers can trigger state transitions.

---

## Example Flow

```
1. marketplace.fund_invoice() → pool fully funded
2. marketplace calls financing_pool.release_funds(marketplace, invoice_id, token)
   → Pool { invoice_id, face_value: 10_000 USDC, ... } created
   → invoice_nft.set_funded() called

3. admin calls financing_pool.record_position(admin, invoice_id, investor_A, 6_000, 10_000)
   → Position { share_bps: 6000 } stored for investor_A

4. admin calls financing_pool.record_position(admin, invoice_id, investor_B, 4_000, 10_000)
   → Position { share_bps: 4000 } stored for investor_B

5. sme calls financing_pool.repay(sme, invoice_id, token, 10_000)
   → pool.repaid_amount = 10_000, pool.is_closed = true
   → distribute_yield():
       investor_A receives 10_000 × 6000 / 10_000 = 6_000 USDC
       investor_B receives 10_000 × 4000 / 10_000 = 4_000 USDC
   → invoice_nft.set_repaid() called
```

---

## Error Reference

| Error | Code | Meaning |
|-------|------|---------|
| `AlreadyInitialized` | 94 | `initialize` called twice |
| `NotInitialized` | 96 | Storage keys not set |
| `InvalidAmount` | 14 | Amount out of valid range |
| `InvalidAddress` | 92 | Token address is the contract itself |
| `NotAdmin` | 2 | Caller is not the admin |
| `Unauthorized` | 1 | Reentrancy guard triggered |
| `PoolNotFound` | 30 | No pool for this invoice ID |
| `PoolAlreadyClosed` | 31 | Pool is already closed |
| `RepaymentAlreadyMade` | 32 | Pool closed before repay call |
| `ArithmeticOverflow` | 90 | Checked arithmetic returned `None` |
| `InvalidFeeRate` | 40 | `late_penalty_bps > 10_000` |
