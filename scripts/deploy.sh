#!/usr/bin/env bash
# =============================================================================
# Kora Protocol — Deployment Script
# Deploys all contracts to Stellar Soroban (testnet or mainnet).
#
# Usage:
#   ./scripts/deploy.sh [testnet|mainnet]
#
# Prerequisites:
#   - stellar CLI installed (https://developers.stellar.org/docs/tools/stellar-cli)
#   - DEPLOYER_SECRET env var set (or use --source flag)
#   - Contracts built: make build
# =============================================================================

set -euo pipefail

NETWORK="${1:-testnet}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WASM_DIR="$ROOT_DIR/target/wasm32-unknown-unknown/release"
DEPLOY_LOG="$ROOT_DIR/deployments/$NETWORK.json"

# ── Network config ────────────────────────────────────────────────────────────

case "$NETWORK" in
  testnet)
    RPC_URL="https://soroban-testnet.stellar.org"
    NETWORK_PASSPHRASE="Test SDF Network ; September 2015"
    ;;
  mainnet)
    RPC_URL="https://soroban-mainnet.stellar.org"
    NETWORK_PASSPHRASE="Public Global Stellar Network ; September 2015"
    ;;
  *)
    echo "Unknown network: $NETWORK. Use 'testnet' or 'mainnet'."
    exit 1
    ;;
esac

SOURCE="${DEPLOYER_SECRET:-}"
if [ -z "$SOURCE" ]; then
  echo "ERROR: Set DEPLOYER_SECRET environment variable."
  exit 1
fi

mkdir -p "$ROOT_DIR/deployments"

# ── Helpers ───────────────────────────────────────────────────────────────────

deploy_contract() {
  local name="$1"
  local wasm="$2"
  echo "  Deploying $name..."
  stellar contract deploy \
    --wasm "$wasm" \
    --source "$SOURCE" \
    --rpc-url "$RPC_URL" \
    --network-passphrase "$NETWORK_PASSPHRASE"
}

invoke() {
  local contract_id="$1"
  local fn="$2"
  shift 2
  stellar contract invoke \
    --id "$contract_id" \
    --source "$SOURCE" \
    --rpc-url "$RPC_URL" \
    --network-passphrase "$NETWORK_PASSPHRASE" \
    -- "$fn" "$@"
}

# ── Deploy ────────────────────────────────────────────────────────────────────

echo "=== Kora Protocol Deployment ==="
echo "Network : $NETWORK"
echo "RPC     : $RPC_URL"
echo ""

ADMIN=$(stellar keys address "$SOURCE" 2>/dev/null || echo "$SOURCE")
echo "Admin   : $ADMIN"
echo ""

echo "--- Deploying contracts ---"

ACCESS_CONTROL_ID=$(deploy_contract "access_control" "$WASM_DIR/kora_access_control.wasm")
echo "  access_control : $ACCESS_CONTROL_ID"

INVOICE_NFT_ID=$(deploy_contract "invoice_nft" "$WASM_DIR/kora_invoice_nft.wasm")
echo "  invoice_nft    : $INVOICE_NFT_ID"

TREASURY_ID=$(deploy_contract "treasury" "$WASM_DIR/kora_treasury.wasm")
echo "  treasury       : $TREASURY_ID"

POOL_ID=$(deploy_contract "financing_pool" "$WASM_DIR/kora_financing_pool.wasm")
echo "  financing_pool : $POOL_ID"

MARKETPLACE_ID=$(deploy_contract "marketplace" "$WASM_DIR/kora_marketplace.wasm")
echo "  marketplace    : $MARKETPLACE_ID"

RISK_REGISTRY_ID=$(deploy_contract "risk_registry" "$WASM_DIR/kora_risk_registry.wasm")
echo "  risk_registry  : $RISK_REGISTRY_ID"

echo ""
echo "--- Initializing contracts ---"

invoke "$ACCESS_CONTROL_ID" initialize --admin "$ADMIN"
echo "  access_control initialized"

invoke "$INVOICE_NFT_ID" initialize --admin "$ADMIN" --access_control "$ACCESS_CONTROL_ID"
echo "  invoice_nft initialized"

invoke "$TREASURY_ID" initialize --admin "$ADMIN" --fee_bps 50
echo "  treasury initialized (fee: 0.5%)"

invoke "$POOL_ID" initialize \
  --admin "$ADMIN" \
  --invoice_nft "$INVOICE_NFT_ID" \
  --treasury "$TREASURY_ID" \
  --late_penalty_bps 200
echo "  financing_pool initialized (late penalty: 2%)"

invoke "$MARKETPLACE_ID" initialize \
  --admin "$ADMIN" \
  --invoice_nft "$INVOICE_NFT_ID" \
  --financing_pool "$POOL_ID" \
  --treasury "$TREASURY_ID" \
  --fee_bps 50
echo "  marketplace initialized"

invoke "$RISK_REGISTRY_ID" initialize --admin "$ADMIN"
echo "  risk_registry initialized"

# ── Write deployment manifest ─────────────────────────────────────────────────

AC_HASH=$(sha256sum "$WASM_DIR/kora_access_control.wasm" | awk '{print $1}')
INVOICE_HASH=$(sha256sum "$WASM_DIR/kora_invoice_nft.wasm" | awk '{print $1}')
TREASURY_HASH=$(sha256sum "$WASM_DIR/kora_treasury.wasm" | awk '{print $1}')
POOL_HASH=$(sha256sum "$WASM_DIR/kora_financing_pool.wasm" | awk '{print $1}')
MARKETPLACE_HASH=$(sha256sum "$WASM_DIR/kora_marketplace.wasm" | awk '{print $1}')
RISK_HASH=$(sha256sum "$WASM_DIR/kora_risk_registry.wasm" | awk '{print $1}')

cat > "$DEPLOY_LOG" <<EOF
{
  "network": "$NETWORK",
  "deployed_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "admin": "$ADMIN",
  "contracts": {
    "access_control": {
      "address": "$ACCESS_CONTROL_ID",
      "wasm_hash": "$AC_HASH"
    },
    "invoice_nft": {
      "address": "$INVOICE_NFT_ID",
      "wasm_hash": "$INVOICE_HASH"
    },
    "treasury": {
      "address": "$TREASURY_ID",
      "wasm_hash": "$TREASURY_HASH"
    },
    "financing_pool": {
      "address": "$POOL_ID",
      "wasm_hash": "$POOL_HASH"
    },
    "marketplace": {
      "address": "$MARKETPLACE_ID",
      "wasm_hash": "$MARKETPLACE_HASH"
    },
    "risk_registry": {
      "address": "$RISK_REGISTRY_ID",
      "wasm_hash": "$RISK_HASH"
    }
  }
}
EOF

echo ""
echo "=== Deployment complete ==="
echo "Manifest saved to: $DEPLOY_LOG"
