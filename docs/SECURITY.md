# Security: Protocol Pause Enforcement Matrix

When the admin calls `access_control.pause()`, state-mutating entrypoints across the
protocol are gated by a cross-contract call to `AccessControlContract.is_paused()`.
Any blocked call returns `KoraError::ProtocolPaused`.

## Enforcement Matrix

| Contract          | Entrypoint          | Blocked when paused | Rationale                                         |
|-------------------|---------------------|---------------------|---------------------------------------------------|
| `invoice_nft`     | `mint_invoice`      | YES                 | No new invoices during emergency                  |
| `invoice_nft`     | `set_listed`        | YES                 | No new listings during emergency                  |
| `invoice_nft`     | `set_funded`        | YES                 | No new funding during emergency                   |
| `invoice_nft`     | `set_repaid`        | NO                  | Repayment finalization must not be blocked        |
| `invoice_nft`     | `set_defaulted`     | NO                  | Admin default marking must not be blocked         |
| `marketplace`     | `list_invoice`      | YES                 | No new listings during emergency                  |
| `marketplace`     | `fund_invoice`      | YES                 | No new capital inflows during emergency           |
| `marketplace`     | `cancel_listing`    | NO                  | Sellers must be able to cancel at any time        |
| `financing_pool`  | `record_position`   | YES                 | No new positions during emergency                 |
| `financing_pool`  | `repay`             | NO (EXEMPT)         | SMEs must always be able to repay; blocking repay harms investors |
| `financing_pool`  | `mark_default`      | YES                 | Default processing during pause requires admin review first |

## Design Decisions

- `repay` is intentionally exempt. Blocking repayment punishes SMEs and investors for
  an admin action they had no control over. The pause is an emergency circuit-breaker
  for new activity, not existing obligations.

- `set_repaid` and `set_defaulted` on `invoice_nft` are not paused because they are
  called internally by the pool contract as part of the repay/default flow, which itself
  may or may not be paused depending on the entry point used.

- `cancel_listing` is not paused so sellers retain the ability to withdraw their invoice
  from a listing at any time.

## Testing

The full enforcement matrix is verified by the integration test
`test_pause_enforcement_matrix` in `contracts/tests/src/lib.rs`. The test:

1. Pauses the protocol via `access_control.pause()`
2. Calls each state-mutating entrypoint listed above
3. Asserts `ProtocolPaused` is returned for all blocked entrypoints
4. Asserts `repay` returns `PoolNotFound` (not `ProtocolPaused`), confirming the exemption
5. Unpauses and confirms `mint_invoice` resumes successfully
