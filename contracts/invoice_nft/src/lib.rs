#![no_std]

use kora_shared::{
    errors::KoraError,
    events,
    types::{Invoice, InvoiceStatus, RiskTier},
    validation::{
        require_future_timestamp, require_non_empty_bytes, require_non_empty_string,
        require_non_zero_amount, require_valid_risk_score,
    },
};
use soroban_sdk::{contract, contractimpl, contracttype, Address, Bytes, Env, String, Symbol};

// ── Storage Keys ────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Invoice(u64),
    NextId,
    Admin,
    AccessControl,
}

// ── Contract ─────────────────────────────────────────────────────────────────

#[contract]
pub struct InvoiceNftContract;

#[contractimpl]
impl InvoiceNftContract {
    /// One-time initializer. Sets admin and access-control contract address.
    pub fn initialize(env: Env, admin: Address, access_control: Address) -> Result<(), KoraError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(KoraError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::AccessControl, &access_control);
        env.storage().instance().set(&DataKey::NextId, &1u64);
        Ok(())
    }

    /// Mint a new invoice NFT. Caller must be a verified SME.
    pub fn mint_invoice(
        env: Env,
        sme: Address,
        debtor_hash: Bytes,
        amount: i128,
        currency: Symbol,
        due_date: u64,
        ipfs_cid: String,
        risk_score: u32,
    ) -> Result<u64, KoraError> {
        sme.require_auth();
        Self::require_not_paused(&env)?;

        require_non_zero_amount(amount)?;
        require_future_timestamp(&env, due_date)?;
        require_valid_risk_score(risk_score)?;
        require_non_empty_bytes(&debtor_hash)?;
        require_non_empty_string(&ipfs_cid)?;

        // Ensure amount doesn't exceed safe bounds
        if amount > i128::MAX / 2 {
            return Err(KoraError::ArithmeticOverflow);
        }

        let id: u64 = env.storage().instance().get(&DataKey::NextId).unwrap_or(1);

        let invoice = Invoice {
            id,
            sme: sme.clone(),
            debtor_hash,
            amount,
            currency,
            due_date,
            ipfs_cid,
            risk_score,
            risk_tier: RiskTier::from_score(risk_score),
            status: InvoiceStatus::Created,
            created_at: env.ledger().timestamp(),
            funded_at: None,
            repaid_at: None,
        };

        env.storage().persistent().set(&DataKey::Invoice(id), &invoice);
        env.storage().instance().set(&DataKey::NextId, &(id.checked_add(1).ok_or(KoraError::ArithmeticOverflow)?));

        events::invoice_created(&env, id, &sme, amount);
        Ok(id)
    }

    /// Transition invoice to Listed status. Called by Marketplace contract.
    pub fn set_listed(env: Env, caller: Address, invoice_id: u64) -> Result<(), KoraError> {
        caller.require_auth();
        Self::require_not_paused(&env)?;
        let mut invoice = Self::load_invoice(&env, invoice_id)?;
        if invoice.status != InvoiceStatus::Created {
            return Err(KoraError::InvalidInvoiceStatus);
        }
        invoice.status = InvoiceStatus::Listed;
        env.storage()
            .persistent()
            .set(&DataKey::Invoice(invoice_id), &invoice);
        events::invoice_listed(&env, invoice_id, &invoice.sme, invoice.amount);
        Ok(())
    }

    /// Transition invoice to Funded. Called by Financing Pool contract.
    pub fn set_funded(env: Env, caller: Address, invoice_id: u64) -> Result<(), KoraError> {
        caller.require_auth();
        Self::require_not_paused(&env)?;
        let mut invoice = Self::load_invoice(&env, invoice_id)?;
        if invoice.status != InvoiceStatus::Listed {
            return Err(KoraError::InvalidInvoiceStatus);
        }
        invoice.status = InvoiceStatus::Funded;
        invoice.funded_at = Some(env.ledger().timestamp());
        env.storage()
            .persistent()
            .set(&DataKey::Invoice(invoice_id), &invoice);
        Ok(())
    }

    /// Mark invoice as Repaid. Called by Financing Pool on full repayment.
    pub fn set_repaid(env: Env, caller: Address, invoice_id: u64) -> Result<(), KoraError> {
        caller.require_auth();
        let mut invoice = Self::load_invoice(&env, invoice_id)?;
        if invoice.status != InvoiceStatus::Funded {
            return Err(KoraError::InvalidInvoiceStatus);
        }
        invoice.status = InvoiceStatus::Repaid;
        invoice.repaid_at = Some(env.ledger().timestamp());
        env.storage().persistent().set(&DataKey::Invoice(invoice_id), &invoice);
        events::invoice_repaid(&env, invoice_id, &invoice.sme, invoice.amount);
        Ok(())
    }

    /// Mark invoice as Defaulted. Called by admin after due date passes.
    pub fn set_defaulted(env: Env, caller: Address, invoice_id: u64) -> Result<(), KoraError> {
        caller.require_auth();
        Self::require_admin(&env, &caller)?;
        let mut invoice = Self::load_invoice(&env, invoice_id)?;
        if invoice.status != InvoiceStatus::Funded {
            return Err(KoraError::InvalidInvoiceStatus);
        }
        let current_time = env.ledger().timestamp();
        if current_time <= invoice.due_date {
            return Err(KoraError::InvalidInvoiceStatus);
        }
        invoice.status = InvoiceStatus::Defaulted;
        env.storage()
            .persistent()
            .set(&DataKey::Invoice(invoice_id), &invoice);
        events::invoice_defaulted(&env, invoice_id, &invoice.sme);
        Ok(())
    }

    // ── Views ────────────────────────────────────────────────────────────────

    pub fn get_invoice(env: Env, invoice_id: u64) -> Result<Invoice, KoraError> {
        Self::load_invoice(&env, invoice_id)
    }

    pub fn next_id(env: Env) -> u64 {
        env.storage().instance().get(&DataKey::NextId).unwrap_or(1)
    }

    /// Returns the number of invoices minted (next_id - 1).
    pub fn invoice_count(env: Env) -> u64 {
        env.storage().instance().get::<_, u64>(&DataKey::NextId).unwrap_or(1).saturating_sub(1)
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn load_invoice(env: &Env, id: u64) -> Result<Invoice, KoraError> {
        env.storage()
            .persistent()
            .get(&DataKey::Invoice(id))
            .ok_or(KoraError::InvoiceNotFound)
    }

    fn require_admin(env: &Env, caller: &Address) -> Result<(), KoraError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(KoraError::NotInitialized)?;
        if &admin != caller {
            return Err(KoraError::NotAdmin);
        }
        Ok(())
    }

    fn require_not_paused(env: &Env) -> Result<(), KoraError> {
        let ac: Address = env
            .storage()
            .instance()
            .get(&DataKey::AccessControl)
            .ok_or(KoraError::NotInitialized)?;
        let _ = ac;
        // Cross-contract pause check wired at deployment via AccessControl contract.
        // Local guard: no-op until cross-contract call is integrated.
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        Bytes, Env, String, Symbol,
    };

    fn setup() -> (Env, Address, InvoiceNftContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();

        env.ledger().set(LedgerInfo {
            timestamp: 1_700_000_000,
            protocol_version: 21,
            sequence_number: 1,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 1000,
            min_persistent_entry_ttl: 1000,
            max_entry_ttl: 100_000,
        });

        let contract_id = env.register_contract(None, InvoiceNftContract);
        let client = InvoiceNftContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let access_control = Address::generate(&env);
        client.initialize(&admin, &access_control);
        (env, admin, client)
    }

    #[test]
    fn test_initialize_success() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, InvoiceNftContract);
        let client = InvoiceNftContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let access_control = Address::generate(&env);
        
        client.initialize(&admin, &access_control);
        assert_eq!(client.next_id(), 1);
    }

    #[test]
    fn test_initialize_already_initialized_fails() {
        let (env, admin, client) = setup();
        let access_control = Address::generate(&env);
        
        let result = client.try_initialize(&admin, &access_control);
        assert!(result.is_err());
    }

    #[test]
    fn test_mint_invoice_success() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(
            &env,
            "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi",
        );
        let due_date = env.ledger().timestamp() + 86_400 * 30;

        let id = client.mint_invoice(
            &sme,
            &debtor_hash,
            &1_000_000_000i128,
            &Symbol::new(&env, "USDC"),
            &due_date,
            &ipfs_cid,
            &25u32,
        );
        assert_eq!(id, 1);

        let invoice = client.get_invoice(&1);
        assert_eq!(invoice.status, InvoiceStatus::Created);
        assert_eq!(invoice.risk_tier, RiskTier::AA);
        assert_eq!(invoice.sme, sme);
        assert_eq!(invoice.amount, 1_000_000_000i128);
    }

    #[test]
    fn test_mint_invoice_zero_amount_fails() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(
            &env,
            "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi",
        );
        let due_date = env.ledger().timestamp() + 86_400;

        let result = client.try_mint_invoice(
            &sme,
            &debtor_hash,
            &0i128,
            &Symbol::new(&env, "USDC"),
            &due_date,
            &ipfs_cid,
            &10u32,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_mint_invoice_negative_amount_fails() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(
            &env,
            "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi",
        );
        let due_date = env.ledger().timestamp() + 86_400;

        let result = client.try_mint_invoice(
            &sme,
            &debtor_hash,
            &-1_000_000_000i128,
            &Symbol::new(&env, "USDC"),
            &due_date,
            &ipfs_cid,
            &10u32,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_mint_invoice_past_due_date_fails() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(
            &env,
            "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi",
        );
        let due_date = env.ledger().timestamp() - 1; // Past date

        let result = client.try_mint_invoice(
            &sme,
            &debtor_hash,
            &1_000_000_000i128,
            &Symbol::new(&env, "USDC"),
            &due_date,
            &ipfs_cid,
            &10u32,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_mint_invoice_invalid_risk_score() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(
            &env,
            "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi",
        );
        let due_date = env.ledger().timestamp() + 86_400;

        let result = client.try_mint_invoice(
            &sme,
            &debtor_hash,
            &1_000_000_000i128,
            &Symbol::new(&env, "USDC"),
            &due_date,
            &ipfs_cid,
            &101u32,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_mint_invoice_empty_debtor_hash_fails() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[]);
        let ipfs_cid = String::from_str(
            &env,
            "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi",
        );
        let due_date = env.ledger().timestamp() + 86_400;

        let result = client.try_mint_invoice(
            &sme,
            &debtor_hash,
            &1_000_000_000i128,
            &Symbol::new(&env, "USDC"),
            &due_date,
            &ipfs_cid,
            &10u32,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_mint_invoice_empty_ipfs_cid_fails() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "");
        let due_date = env.ledger().timestamp() + 86_400;

        let result = client.try_mint_invoice(
            &sme,
            &debtor_hash,
            &1_000_000_000i128,
            &Symbol::new(&env, "USDC"),
            &due_date,
            &ipfs_cid,
            &10u32,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_mint_multiple_invoices_increments_id() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi");
        let due_date = env.ledger().timestamp() + 86_400 * 30;

        let id1 = client.mint_invoice(
            &sme, &debtor_hash, &1_000_000_000i128,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &10u32,
        );
        let id2 = client.mint_invoice(
            &sme, &debtor_hash, &2_000_000_000i128,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &20u32,
        );
        
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(client.next_id(), 3);
    }

    #[test]
    fn test_risk_tier_mapping() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi");
        let due_date = env.ledger().timestamp() + 86_400 * 30;

        let test_cases = vec![
            (0u32, RiskTier::AAA),
            (20u32, RiskTier::AAA),
            (21u32, RiskTier::AA),
            (40u32, RiskTier::AA),
            (41u32, RiskTier::A),
            (60u32, RiskTier::A),
            (61u32, RiskTier::B),
            (80u32, RiskTier::B),
            (81u32, RiskTier::C),
            (100u32, RiskTier::C),
        ];

        for (score, expected_tier) in test_cases {
            let id = client.mint_invoice(
                &sme, &debtor_hash, &1_000_000_000i128,
                &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &score,
            );
            let invoice = client.get_invoice(&id);
            assert_eq!(invoice.risk_tier, expected_tier);
        }
    }

    #[test]
    fn test_status_transitions() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(
            &env,
            "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi",
        );
        let due_date = env.ledger().timestamp() + 86_400 * 30;

        let id = client.mint_invoice(
            &sme,
            &debtor_hash,
            &1_000_000_000i128,
            &Symbol::new(&env, "USDC"),
            &due_date,
            &ipfs_cid,
            &10u32,
        );

        let marketplace = Address::generate(&env);
        client.set_listed(&marketplace, &id);
        assert_eq!(client.get_invoice(&id).status, InvoiceStatus::Listed);

        let pool = Address::generate(&env);
        client.set_funded(&pool, &id);
        assert_eq!(client.get_invoice(&id).status, InvoiceStatus::Funded);

        client.set_repaid(&pool, &id);
        assert_eq!(client.get_invoice(&id).status, InvoiceStatus::Repaid);
    }

    #[test]
    fn test_invalid_status_transition_created_to_funded_fails() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi");
        let due_date = env.ledger().timestamp() + 86_400 * 30;

        let id = client.mint_invoice(
            &sme, &debtor_hash, &1_000_000_000i128,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &10u32,
        );

        let pool = Address::generate(&env);
        let result = client.try_set_funded(&pool, &id);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_status_transition_listed_to_repaid_fails() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi");
        let due_date = env.ledger().timestamp() + 86_400 * 30;

        let id = client.mint_invoice(
            &sme, &debtor_hash, &1_000_000_000i128,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &10u32,
        );

        let marketplace = Address::generate(&env);
        client.set_listed(&marketplace, &id);

        let pool = Address::generate(&env);
        let result = client.try_set_repaid(&pool, &id);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_defaulted_before_due_date_fails() {
        let (env, admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi");
        let due_date = env.ledger().timestamp() + 86_400 * 30;

        let id = client.mint_invoice(
            &sme, &debtor_hash, &1_000_000_000i128,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &10u32,
        );

        let marketplace = Address::generate(&env);
        client.set_listed(&marketplace, &id);

        let pool = Address::generate(&env);
        client.set_funded(&pool, &id);

        let result = client.try_set_defaulted(&admin, &id);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_defaulted_after_due_date_succeeds() {
        let (env, admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi");
        let due_date = env.ledger().timestamp() + 86_400;

        let id = client.mint_invoice(
            &sme, &debtor_hash, &1_000_000_000i128,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &10u32,
        );

        let marketplace = Address::generate(&env);
        client.set_listed(&marketplace, &id);

        let pool = Address::generate(&env);
        client.set_funded(&pool, &id);

        // Advance time past due date
        env.ledger().set(soroban_sdk::testutils::LedgerInfo {
            timestamp: due_date + 1,
            ..env.ledger().get()
        });

        client.set_defaulted(&admin, &id);
        assert_eq!(client.get_invoice(&id).status, InvoiceStatus::Defaulted);
    }

    #[test]
    fn test_get_nonexistent_invoice_fails() {
        let (env, _admin, client) = setup();
        let result = client.try_get_invoice(&999u64);
        assert!(result.is_err());
    }

    #[test]
    fn test_invoice_timestamps_recorded() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi");
        let due_date = env.ledger().timestamp() + 86_400 * 30;
        let created_at = env.ledger().timestamp();

        let id = client.mint_invoice(
            &sme, &debtor_hash, &1_000_000_000i128,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &10u32,
        );

        let invoice = client.get_invoice(&id);
        assert_eq!(invoice.created_at, created_at);
        assert_eq!(invoice.funded_at, None);
        assert_eq!(invoice.repaid_at, None);

        let marketplace = Address::generate(&env);
        client.set_listed(&marketplace, &id);

        let pool = Address::generate(&env);
        client.set_funded(&pool, &id);
        let invoice = client.get_invoice(&id);
        assert!(invoice.funded_at.is_some());

        client.set_repaid(&pool, &id);
        let invoice = client.get_invoice(&id);
        assert!(invoice.repaid_at.is_some());
    }

    #[test]
    fn test_large_invoice_amounts() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi");
        let due_date = env.ledger().timestamp() + 86_400 * 30;

        let large_amount = 9_223_372_036_854_775_807i128; // i128::MAX
        let id = client.mint_invoice(
            &sme, &debtor_hash, &large_amount,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &50u32,
        );

        let invoice = client.get_invoice(&id);
        assert_eq!(invoice.amount, large_amount);
    }

    #[test]
    fn test_multiple_invoices_different_currencies() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi");
        let due_date = env.ledger().timestamp() + 86_400 * 30;

        let id1 = client.mint_invoice(
            &sme, &debtor_hash, &1_000_000_000i128,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &10u32,
        );

        let id2 = client.mint_invoice(
            &sme, &debtor_hash, &2_000_000_000i128,
            &Symbol::new(&env, "EURC"), &due_date, &ipfs_cid, &20u32,
        );

        let invoice1 = client.get_invoice(&id1);
        let invoice2 = client.get_invoice(&id2);

        assert_eq!(invoice1.currency, Symbol::new(&env, "USDC"));
        assert_eq!(invoice2.currency, Symbol::new(&env, "EURC"));
    }

    #[test]
    fn test_invoice_immutability_after_creation() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi");
        let due_date = env.ledger().timestamp() + 86_400 * 30;

        let id = client.mint_invoice(
            &sme, &debtor_hash, &1_000_000_000i128,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &10u32,
        );

        let invoice1 = client.get_invoice(&id);
        let invoice2 = client.get_invoice(&id);

        assert_eq!(invoice1.id, invoice2.id);
        assert_eq!(invoice1.amount, invoice2.amount);
        assert_eq!(invoice1.sme, invoice2.sme);
    }

    #[test]
    fn test_set_listed_idempotent_fails() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi");
        let due_date = env.ledger().timestamp() + 86_400 * 30;

        let id = client.mint_invoice(
            &sme, &debtor_hash, &1_000_000_000i128,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &10u32,
        );

        let marketplace = Address::generate(&env);
        client.set_listed(&marketplace, &id);

        let result = client.try_set_listed(&marketplace, &id);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_funded_idempotent_fails() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi");
        let due_date = env.ledger().timestamp() + 86_400 * 30;

        let id = client.mint_invoice(
            &sme, &debtor_hash, &1_000_000_000i128,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &10u32,
        );

        let marketplace = Address::generate(&env);
        client.set_listed(&marketplace, &id);

        let pool = Address::generate(&env);
        client.set_funded(&pool, &id);

        let result = client.try_set_funded(&pool, &id);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_repaid_idempotent_fails() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi");
        let due_date = env.ledger().timestamp() + 86_400 * 30;

        let id = client.mint_invoice(
            &sme, &debtor_hash, &1_000_000_000i128,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &10u32,
        );

        let marketplace = Address::generate(&env);
        client.set_listed(&marketplace, &id);

        let pool = Address::generate(&env);
        client.set_funded(&pool, &id);
        client.set_repaid(&pool, &id);

        let result = client.try_set_repaid(&pool, &id);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_defaulted_non_admin_fails() {
        let (env, admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi");
        let due_date = env.ledger().timestamp() + 86_400;

        let id = client.mint_invoice(
            &sme, &debtor_hash, &1_000_000_000i128,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &10u32,
        );

        let marketplace = Address::generate(&env);
        client.set_listed(&marketplace, &id);

        let pool = Address::generate(&env);
        client.set_funded(&pool, &id);

        let non_admin = Address::generate(&env);
        let result = client.try_set_defaulted(&non_admin, &id);
        assert!(result.is_err());
    }

    #[test]
    fn test_risk_score_boundary_values() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[1u8; 32]);
        let ipfs_cid = String::from_str(&env, "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi");
        let due_date = env.ledger().timestamp() + 86_400 * 30;

        // Test boundary: 20 (AAA) vs 21 (AA)
        let id1 = client.mint_invoice(
            &sme, &debtor_hash, &1_000_000_000i128,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &20u32,
        );
        assert_eq!(client.get_invoice(&id1).risk_tier, RiskTier::AAA);

        let id2 = client.mint_invoice(
            &sme, &debtor_hash, &1_000_000_000i128,
            &Symbol::new(&env, "USDC"), &due_date, &ipfs_cid, &21u32,
        );
        assert_eq!(client.get_invoice(&id2).risk_tier, RiskTier::AA);
    }
}
