#!/usr/bin/env bash
# =============================================================================
# Kora Protocol — State Drift Detector
#
# Fetches invoice state from invoice_nft, marketplace, and financing_pool
# for a given invoice_id and reports any inconsistencies.
#
# Usage:
#   ./scripts/check_state_drift.sh <invoice_id> [testnet|mainnet]
#
# Prerequisites:
#   - stellar CLI installed
#   - Contract IDs set via env vars:
#       INVOICE_NFT_ID, MARKETPLACE_ID, FINANCING_POOL_ID
#   - Network configured in stellar CLI
# =============================================================================

set -euo pipefail

if [ $# -lt 1 ]; then
  echo "Usage: $0 <invoice_id> [testnet|mainnet]"
  exit 1
fi

INVOICE_ID="$1"
NETWORK="${2:-testnet}"

: "${INVOICE_NFT_ID:?Set INVOICE_NFT_ID env var to the invoice_nft contract address}"
: "${MARKETPLACE_ID:?Set MARKETPLACE_ID env var to the marketplace contract address}"
: "${FINANCING_POOL_ID:?Set FINANCING_POOL_ID env var to the financing_pool contract address}"

MISMATCHES=0

echo "=== Kora State Drift Check ==="
echo "Invoice ID: $INVOICE_ID"
echo "Network:    $NETWORK"
echo ""

# --- Fetch invoice NFT state ---
echo "--- invoice_nft.get_invoice($INVOICE_ID) ---"
INVOICE_RAW=$(stellar contract invoke \
  --network "$NETWORK" \
  --id "$INVOICE_NFT_ID" \
  -- get_invoice \
  --invoice_id "$INVOICE_ID" 2>&1) || {
  echo "  ERROR: Could not fetch invoice from invoice_nft"
  echo "  $INVOICE_RAW"
  INVOICE_STATUS="NOT_FOUND"
}

if [ "${INVOICE_STATUS:-}" != "NOT_FOUND" ]; then
  INVOICE_STATUS=$(echo "$INVOICE_RAW" | grep -oP '"status"\s*:\s*"\K[^"]+' || echo "UNKNOWN")
  echo "  status: $INVOICE_STATUS"
  echo "  raw: $INVOICE_RAW"
fi
echo ""

# --- Fetch marketplace listing state ---
echo "--- marketplace.get_listing($INVOICE_ID) ---"
LISTING_RAW=$(stellar contract invoke \
  --network "$NETWORK" \
  --id "$MARKETPLACE_ID" \
  -- get_listing \
  --invoice_id "$INVOICE_ID" 2>&1) || {
  echo "  INFO: No listing found in marketplace (may be expected)"
  LISTING_RAW=""
  LISTING_ACTIVE="NONE"
}

if [ -n "$LISTING_RAW" ] && [ "${LISTING_ACTIVE:-}" != "NONE" ]; then
  LISTING_ACTIVE=$(echo "$LISTING_RAW" | grep -oP '"is_active"\s*:\s*\K(true|false)' || echo "UNKNOWN")
  LISTING_FUNDED=$(echo "$LISTING_RAW" | grep -oP '"funded_amount"\s*:\s*\K[0-9]+' || echo "0")
  LISTING_ASKING=$(echo "$LISTING_RAW" | grep -oP '"asking_price"\s*:\s*\K[0-9]+' || echo "0")
  echo "  is_active:     $LISTING_ACTIVE"
  echo "  funded_amount: $LISTING_FUNDED"
  echo "  asking_price:  $LISTING_ASKING"
fi
echo ""

# --- Fetch financing pool state ---
echo "--- financing_pool.get_pool($INVOICE_ID) ---"
POOL_RAW=$(stellar contract invoke \
  --network "$NETWORK" \
  --id "$FINANCING_POOL_ID" \
  -- get_pool \
  --invoice_id "$INVOICE_ID" 2>&1) || {
  echo "  INFO: No pool found in financing_pool (may be expected)"
  POOL_RAW=""
  POOL_EXISTS="false"
}

if [ -n "$POOL_RAW" ] && [ "${POOL_EXISTS:-}" != "false" ]; then
  POOL_EXISTS="true"
  POOL_CLOSED=$(echo "$POOL_RAW" | grep -oP '"is_closed"\s*:\s*\K(true|false)' || echo "UNKNOWN")
  POOL_REPAID=$(echo "$POOL_RAW" | grep -oP '"repaid_amount"\s*:\s*\K[0-9]+' || echo "0")
  echo "  is_closed:     $POOL_CLOSED"
  echo "  repaid_amount: $POOL_REPAID"
else
  POOL_EXISTS="false"
  POOL_CLOSED="N/A"
fi
echo ""

# --- Cross-contract consistency checks ---
echo "=== Drift Analysis ==="

# Check 1: Listing says active but invoice is already Funded/Repaid/Defaulted
if [ "${LISTING_ACTIVE:-NONE}" = "true" ] && \
   { [ "$INVOICE_STATUS" = "Funded" ] || [ "$INVOICE_STATUS" = "Repaid" ] || [ "$INVOICE_STATUS" = "Defaulted" ]; }; then
  echo "MISMATCH: Listing is_active=true but invoice status=$INVOICE_STATUS"
  MISMATCHES=$((MISMATCHES + 1))
fi

# Check 2: Pool exists but invoice status is still Created or Listed
if [ "$POOL_EXISTS" = "true" ] && \
   { [ "$INVOICE_STATUS" = "Created" ] || [ "$INVOICE_STATUS" = "Listed" ]; }; then
  echo "MISMATCH: Pool exists but invoice status=$INVOICE_STATUS (expected Funded/Repaid/Defaulted)"
  MISMATCHES=$((MISMATCHES + 1))
fi

# Check 3: Invoice is Funded but no pool exists
if [ "$INVOICE_STATUS" = "Funded" ] && [ "$POOL_EXISTS" = "false" ]; then
  echo "MISMATCH: Invoice status=Funded but no pool found in financing_pool"
  MISMATCHES=$((MISMATCHES + 1))
fi

# Check 4: Invoice is Repaid but pool is not closed
if [ "$INVOICE_STATUS" = "Repaid" ] && [ "$POOL_CLOSED" = "false" ]; then
  echo "MISMATCH: Invoice status=Repaid but pool is_closed=false"
  MISMATCHES=$((MISMATCHES + 1))
fi

# Check 5: Pool is closed but invoice is still Funded (not Repaid/Defaulted)
if [ "$POOL_CLOSED" = "true" ] && [ "$INVOICE_STATUS" = "Funded" ]; then
  echo "MISMATCH: Pool is_closed=true but invoice status=Funded (expected Repaid or Defaulted)"
  MISMATCHES=$((MISMATCHES + 1))
fi

# Check 6: Listing inactive (fully funded) but no pool exists
if [ "${LISTING_ACTIVE:-NONE}" = "false" ] && [ "$POOL_EXISTS" = "false" ] && \
   [ "$INVOICE_STATUS" = "Funded" ]; then
  echo "MISMATCH: Listing deactivated and invoice=Funded but no pool exists"
  MISMATCHES=$((MISMATCHES + 1))
fi

if [ "$MISMATCHES" -eq 0 ]; then
  echo "OK: No state drift detected for invoice $INVOICE_ID"
else
  echo ""
  echo "ALERT: $MISMATCHES mismatch(es) detected for invoice $INVOICE_ID"
fi

exit "$MISMATCHES"
