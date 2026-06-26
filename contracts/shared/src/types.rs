use soroban_sdk::{contracttype, Address, Bytes, String, Symbol};

/// Invoice lifecycle status
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvoiceStatus {
    Created,
    Listed,
    Funded,
    Repaid,
    Defaulted,
}

/// Risk tier assigned by verifiers
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RiskTier {
    AAA, // 0–20
    AA,  // 21–40
    A,   // 41–60
    B,   // 61–80
    C,   // 81–100
}

impl RiskTier {
    /// OPT: Mark for inlining - simple range-based match, called frequently during minting
    #[inline]
    pub fn from_score(score: u32) -> RiskTier {
        match score {
            0..=20 => RiskTier::AAA,
            21..=40 => RiskTier::AA,
            41..=60 => RiskTier::A,
            61..=80 => RiskTier::B,
            _ => RiskTier::C,
        }
    }
}

/// Core invoice NFT data stored on-chain
#[contracttype]
#[derive(Clone, Debug)]
pub struct Invoice {
    pub id: u64,
    pub sme: Address,
    pub debtor_hash: Bytes, // keccak/sha256 of debtor info — PII stays off-chain
    pub amount: i128,       // face value in stroops (7 decimals)
    pub currency: Symbol,   // e.g. USDC, EURC
    pub due_date: u64,      // Unix timestamp
    pub ipfs_cid: String,   // IPFS CID of full invoice metadata
    pub risk_score: u32,    // 0–100
    pub risk_tier: RiskTier,
    pub status: InvoiceStatus,
    pub created_at: u64,
    pub funded_at: Option<u64>,
    pub repaid_at: Option<u64>,
}

/// A marketplace listing for an invoice
#[contracttype]
#[derive(Clone, Debug)]
pub struct Listing {
    pub invoice_id: u64,
    pub seller: Address,
    pub asking_price: i128, // discounted price investors pay
    pub face_value: i128,   // full repayment amount
    pub token: Address,     // whitelisted stablecoin
    pub funded_amount: i128,
    pub funding_deadline: u64,
    pub is_active: bool,
}

/// A single investor position in a pool
#[contracttype]
#[derive(Clone, Debug)]
pub struct Position {
    pub investor: Address,
    pub invoice_id: u64,
    pub contributed: i128,
    pub share_bps: u32, // basis points of total pool (10000 = 100%)
    pub yield_claimed: i128,
}

/// Pool state for a funded invoice
#[contracttype]
#[derive(Clone, Debug)]
pub struct Pool {
    pub invoice_id: u64,
    pub token: Address,
    pub total_funded: i128,
    pub face_value: i128,
    pub repaid_amount: i128,
    pub is_closed: bool,
    pub late_penalty_bps: u32,
}

/// Protocol-level configuration.
///
/// Note: pause state is NOT stored here — it is owned exclusively by the
/// AccessControl contract to avoid split-brain between two sources of truth.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ProtocolConfig {
    pub fee_bps: u32,          // protocol fee in basis points (e.g. 50 = 0.5%)
    pub late_penalty_bps: u32, // penalty on late repayment
    pub max_risk_score: u32,   // ceiling for accepted invoices
    pub min_funding_period: u64,
}

/// SME profile in the risk registry
#[contracttype]
#[derive(Clone, Debug)]
pub struct SmeProfile {
    pub address: Address,
    pub verified: bool,
    pub verifier: Address,
    pub risk_score: u32,
    pub total_invoices: u32,
    pub defaults: u32,
    pub registered_at: u64,
}
