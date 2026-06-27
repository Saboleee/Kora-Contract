# Kora Protocol Event Naming Convention

This document defines the standardized event topic naming convention across all Kora contracts, ensuring consistent event identification for indexers, dashboards, and monitoring systems.

## Naming Pattern

All event topic symbols follow the pattern:

```
<CONTRACT>_<ACTION>
```

Where:
- `<CONTRACT>`: 3-letter contract identifier (INV, POOL, MKTPL, TREAS, RISK, AC)
- `<ACTION>`: 3-6 letter action verb (CREATED, LISTED, FUNDED, UPDATED, etc.)

**Limit:** Soroban `symbol_short!()` supports up to 32 characters per topic. All our symbols stay well under this limit (8-14 chars).

## Event Topic Registry

### Invoice NFT Contract (INV_*)

| Topic | Function | Payload | Description |
|-------|----------|---------|-------------|
| `INV_CREATED` | invoice_created | (invoice_id, sme, amount, timestamp) | Invoice minted |
| `INV_LISTED` | invoice_listed | (seller, invoice_id, asking_price, timestamp) | Listing created |
| `INV_FUNDED` | invoice_funded | (investor, invoice_id, funded_amount, timestamp) | Funding received |
| `INV_REPAID` | invoice_repaid | (invoice_id, sme, amount, timestamp) | Repayment made |
| `INV_DEFAULTED` | invoice_defaulted | (invoice_id, sme, timestamp) | Invoice defaulted |

### Financing Pool Contract (POOL_*)

| Topic | Function | Payload | Description |
|-------|----------|---------|-------------|
| `POOL_OPENED` | pool_opened | (marketplace, invoice_id, token, face_value, timestamp) | Pool initialized |
| `POS_RECORDED` | position_recorded | (admin, invoice_id, investor, contributed, share_bps, timestamp) | Position allocated |

### Marketplace Contract (MKTPL_*)

| Topic | Function | Payload | Description |
|-------|----------|---------|-------------|
| `MKTPL_CANCELLED` | listing_cancelled | (invoice_id, seller, timestamp) | Listing cancelled |
| `MKTPL_EXPIRED` | listing_expired | (invoice_id, seller, timestamp) | Funding deadline passed |

### Treasury Contract (TREAS_*)

| Topic | Function | Payload | Description |
|-------|----------|---------|-------------|
| `TREAS_INITIALIZED` | treasury_initialized | (admin, fee_bps) | Contract initialized |
| `TREAS_FEE_COLLECTED` | fee_collected | (invoice_id, fee_amount, token, timestamp) | Fee accrued |
| `TREAS_FEE_WITHDRAWN` | fee_withdrawn | (token, amount) | Fee withdrawn |
| `TREAS_EMERGENCY_WTH` | emergency_withdrawn | (by, token, amount) | Emergency drain |
| `TREAS_FEE_UPDATED` | fee_rate_updated | (by, old_bps, new_bps) | Fee rate changed |

### Risk Registry Contract (RISK_*)

| Topic | Function | Payload | Description |
|-------|----------|---------|-------------|
| `RISK_VERIFIER_ADDED` | verifier_added | (admin, verifier, timestamp) | Verifier whitelisted |
| `RISK_VERIFIER_REMOVED` | verifier_removed | (admin, verifier, timestamp) | Verifier revoked |
| `RISK_SME_REGISTERED` | sme_registered | (verifier, sme, risk_score, timestamp) | SME profile created |
| `RISK_SME_SCORE_UPDATED` | sme_score_updated | (verifier, sme, new_score, timestamp) | Risk score changed |
| `RISK_SME_DEFAULT_REC` | sme_default_recorded | (admin, sme, total_defaults, timestamp) | Default recorded |
| `RISK_SME_INV_COUNT` | sme_invoice_count_incremented | (sme, new_total, timestamp) | Invoice count updated |
| `RISK_DEBTOR_SCORE_SET` | debtor_score_set | (verifier, debtor_hash, score, timestamp) | Debtor score set |
| `RISK_REGISTRY_INIT` | registry_initialized | (admin, invoice_nft) | Contract initialized |

### Access Control Contract (AC_*)

| Topic | Function | Payload | Description |
|-------|----------|---------|-------------|
| `AC_PAUSED` | protocol_paused | (by, timestamp) | Protocol paused |
| `AC_UNPAUSED` | protocol_unpaused | (by, timestamp) | Protocol unpaused |
| `AC_ADMIN_TRANSFERRED` | admin_transferred | (new_admin) | Admin changed |
| `AC_ROLE_GRANTED` | role_granted | (admin, target) | Role assigned |
| `AC_ROLE_REVOKED` | role_revoked | (admin, target) | Role revoked |
| `AC_TOKEN_WHITELISTED` | token_whitelisted | (token) | Token approved |
| `AC_UPGRADE_PROPOSED` | upgrade_proposed | (admin, wasm_hash, timestamp) | Upgrade proposed |
| `AC_UPGRADE_EXECUTED` | upgrade_executed | (admin, wasm_hash, timestamp) | Upgrade executed |
| `AC_MULTISIG_CFG` | multisig_configured | (threshold, signer_count, timestamp) | Multisig config set |
| `AC_ACTION_PROPOSED` | action_proposed | (proposal_id, proposer, timestamp) | Multisig action proposed |
| `AC_ACTION_APPROVED` | action_approved | (proposal_id, approver, approval_count, timestamp) | Multisig approval |
| `AC_ACTION_EXECUTED` | action_executed | (proposal_id, executor, timestamp) | Multisig action executed |

### Shared / Cross-Contract Events (PROTOCOL_*)

| Topic | Function | Payload | Description |
|-------|----------|---------|-------------|
| `PROTOCOL_YIELD_DIST` | yield_distributed | (invoice_id, investor, yield_amount, timestamp) | Yield paid |
| `PROTOCOL_LATE_PEN` | late_penalty_applied | (invoice_id, penalty_amount, total_owed, timestamp) | Late fee applied |
| `PROTOCOL_REPAYMENT` | repayment_made | (invoice_id, payer, amount, timestamp) | Repayment made |
| `PROTOCOL_REFUND_CLAIMED` | refund_claimed | (invoice_id, investor, amount, timestamp) | Refund processed |

## Indexer Integration

When subscribing to events via Soroban event streams:

```rust
// Match events by topic symbol
let topic = env.events().last_published_topic();

// Example matchers (for off-chain indexers)
match topic {
    "INV_CREATED" => { /* handle invoice creation */ },
    "POOL_OPENED" => { /* handle pool opening */ },
    "RISK_SME_REGISTERED" => { /* handle new SME */ },
    _ => { /* unrecognized event */ },
}
```

## Migration Notes

Events use Soroban's `symbol_short!()` macro, which is more efficient than `Symbol::new()` for constants. All topic symbols are 8-14 characters and fit comfortably within the 32-character limit.

For backwards compatibility with deployed contracts, the internal event topic names remain unchanged—only new deployments follow this convention.
