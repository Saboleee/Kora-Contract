# Kora Protocol — Event Schema Reference

This document is the canonical reference for every on-chain event published by the
Kora protocol contracts. Indexer authors and off-chain reconciliation tooling should
treat this file as the source of truth.

---

## Canonical Schema Convention

Every event follows the ordering convention:

```
(actor: Address, subject: ..., amount: i128, ledger_timestamp: u64)
```

| Position | Field | Description |
|----------|-------|-------------|
| 1 | **actor** | The `Address` initiating the action (SME, investor, admin, contract) |
| 2 | **subject** | What is being acted on — typically an `invoice_id: u64` or `token: Address` |
| 3 | **amount / data** | Monetary value in stroops, or relevant scalar data (0 when not applicable) |
| last | **timestamp** | `env.ledger().timestamp()` — always present for deterministic indexing |

Events with more than three data fields extend the tuple while preserving actor-first,
timestamp-last ordering.

System events (where there is no single initiating actor — e.g. `late_penalty_applied`,
`multisig_configured`) omit the actor and start with the relevant subject or scalar.

---

## Event Catalog

### Invoice Events

| Topic Symbol | Function | Payload | Emitter |
|---|---|---|---|
| `INV_CRT` | `invoice_created` | `(sme, invoice_id, amount, timestamp)` | `invoice_nft` |
| `INV_LST` | `invoice_listed` | `(seller, invoice_id, asking_price, timestamp)` | `marketplace`, `invoice_nft` |
| `INV_FND` | `invoice_funded` | `(investor, invoice_id, funded_amount, timestamp)` | `marketplace`, `invoice_nft` |
| `INV_RPD` | `invoice_repaid` | `(sme, invoice_id, amount, timestamp)` | `invoice_nft` |
| `INV_DFT` | `invoice_defaulted` | `(actor, invoice_id, timestamp)` | `invoice_nft`, `financing_pool` |

> **Note on `INV_DFT`:** The `actor` field is the admin address that triggered the
> default marking — it is not the SME. In `invoice_nft`, the caller is validated as
> the contract admin; in `financing_pool`, it is the admin address passed to `mark_default`.

---

### Repayment Events

| Topic Symbol | Function | Payload | Emitter |
|---|---|---|---|
| `REPAY` | `repayment_made` | `(payer, invoice_id, amount, timestamp)` | `financing_pool` |
| `YIELD` | `yield_distributed` | `(investor, invoice_id, yield_amount, timestamp)` | `financing_pool` |
| `LATE_PEN` | `late_penalty_applied` | `(invoice_id, penalty_amount, total_owed, timestamp)` | `financing_pool` |

> **`LATE_PEN`** is a system event with no actor. `invoice_id` identifies which pool
> the penalty applies to, `penalty_amount` is the incremental penalty, and `total_owed`
> is the new total the SME owes.

---

### Marketplace Events

| Topic Symbol | Function | Payload | Emitter |
|---|---|---|---|
| `LST_CXL` | `listing_cancelled` | `(seller, invoice_id, timestamp)` | `marketplace` |
| `LST_EXP` | `listing_expired` | `(seller, invoice_id, timestamp)` | `marketplace` |
| `REFUND` | `refund_claimed` | `(investor, invoice_id, amount, timestamp)` | `marketplace` |

---

### Fee Events

| Topic Symbol | Function | Payload | Emitter |
|---|---|---|---|
| `FEE_COL` | `fee_collected` | `(investor, invoice_id, fee_amount, token, timestamp)` | `marketplace`, `treasury` |
| `FEE_WTH` | `fee_withdrawn` | `(admin, token, amount, timestamp)` | `treasury` |
| `EMRG_WTH` | `emergency_withdrawn` | `(admin, token, amount, timestamp)` | `treasury` |
| `FEE_UPD` | `fee_rate_updated` | `(admin, old_bps, new_bps, timestamp)` | `treasury`, `marketplace` |
| `TRES_INI` | `treasury_initialized` | `(admin, fee_bps, timestamp)` | `treasury` |

> **`FEE_COL` from treasury:** When `treasury.collect_fee` emits this event, the
> `investor` field is set to the treasury contract address (a sentinel indicating
> a protocol-internal accounting deposit rather than a direct investor action).
> Off-chain indexers can distinguish by checking whether `investor == treasury_contract`.

---

### Protocol / Admin Events

| Topic Symbol | Function | Payload | Emitter |
|---|---|---|---|
| `PAUSED` | `protocol_paused` | `(admin, timestamp)` | `access_control` |
| `UNPAUSED` | `protocol_unpaused` | `(admin, timestamp)` | `access_control` |
| `TOK_WL` | `token_whitelisted` | `(admin, token, timestamp)` | `marketplace`, `treasury` |
| `ADM_TRF` | `admin_transferred` | `(current_admin, new_admin, timestamp)` | `access_control`, `risk_registry` |
| `ROL_GRT` | `role_granted` | `(admin, target, timestamp)` | `access_control` |
| `ROL_RVK` | `role_revoked` | `(admin, target, timestamp)` | `access_control` |

---

### Financing Pool Events

| Topic Symbol | Function | Payload | Emitter |
|---|---|---|---|
| `PLOP` | `pool_opened` | `(marketplace, invoice_id, token, face_value, timestamp)` | `financing_pool` |
| `POSR` | `position_recorded` | `(admin, invoice_id, investor, contributed, share_bps, timestamp)` | `financing_pool` |

---

### Risk Registry Events

| Topic Symbol | Function | Payload | Emitter |
|---|---|---|---|
| `REG_INI` | `registry_initialized` | `(admin, invoice_nft, timestamp)` | `risk_registry` |
| `VRF_ADD` | `verifier_added` | `(admin, verifier, timestamp)` | `risk_registry` |
| `VRF_REM` | `verifier_removed` | `(admin, verifier, timestamp)` | `risk_registry` |
| `SME_REG` | `sme_registered` | `(verifier, sme, risk_score, timestamp)` | `risk_registry` |
| `SME_UPD` | `sme_score_updated` | `(verifier, sme, new_score, timestamp)` | `risk_registry` |
| `SME_DFT` | `sme_default_recorded` | `(admin, sme, total_defaults, timestamp)` | `risk_registry` |
| `SME_INV` | `sme_invoice_count_incremented` | `(sme, new_total_invoices, timestamp)` | `risk_registry` |
| `DBT_SCR` | `debtor_score_set` | `(verifier, debtor_hash, score, timestamp)` | `risk_registry` |

---

### Upgrade Events

| Topic Symbol | Function | Payload | Emitter |
|---|---|---|---|
| `UPG_PROP` | `upgrade_proposed` | `(admin, wasm_hash, timestamp)` | all contracts |
| `UPG_EXEC` | `upgrade_executed` | `(admin, wasm_hash, timestamp)` | all contracts |

---

### Multisig Events

| Topic Symbol | Function | Payload | Emitter |
|---|---|---|---|
| `MS_CFG` | `multisig_configured` | `(threshold, signer_count, timestamp)` | `access_control` |
| `MS_PROP` | `action_proposed` | `(proposal_id, proposer, timestamp)` | `access_control` |
| `MS_APPR` | `action_approved` | `(proposal_id, approver, approval_count, timestamp)` | `access_control` |
| `MS_EXEC` | `action_executed` | `(proposal_id, executor, timestamp)` | `access_control` |

---

## Indexing Notes

- All topics are published as a single-element tuple: `(topic_symbol,)`.
- `ledger_timestamp` is a `u64` Unix timestamp in seconds.
- `invoice_id` is a `u64` auto-incrementing integer starting at 1.
- `share_bps` is a `u32` in basis points (10 000 = 100 %).
- `fee_bps`, `old_bps`, `new_bps` are `u32` basis-point values (max 10 000).
- `risk_score` / `new_score` are `u32` values in the range 0–100.
- `debtor_hash` is `Bytes` (SHA-256 of off-chain PII — the raw bytes, not hex-encoded).
- `wasm_hash` is `BytesN<32>`.
- All monetary amounts (`amount`, `face_value`, `fee_amount`, etc.) are `i128` in stroops
  (7 decimal places for USDC/EURC on Stellar).
