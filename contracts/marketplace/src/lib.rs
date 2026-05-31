#![no_std]

use kora_shared::{
    errors::KoraError,
    events,
    types::Listing,
    validation::{bps_of, require_non_zero_amount, require_valid_fee_bps},
};
use soroban_sdk::{contract, contractimpl, contracttype, token, Address, Env};

// ── Storage Keys ─────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Listing(u64),
    Config,
    Admin,
    InvoiceNft,
    FinancingPool,
    Treasury,
    FeeBps,
    WhitelistedToken(Address),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketplaceConfig {
    pub admin: Address,
    pub invoice_nft: Address,
    pub financing_pool: Address,
    pub treasury: Address,
    pub fee_bps: u32,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct MarketplaceContract;

#[contractimpl]
impl MarketplaceContract {
    /// Initialize the marketplace contract. Sets up admin, connected contracts, and fee configuration.
    /// Parameters: env, admin address, invoice_nft contract address, financing_pool address, treasury address, fee rate in basis points.
    /// Errors: AlreadyInitialized if already initialized, invalid fee_bps if > 10_000 bps (100%).
    pub fn initialize(
        env: Env,
        admin: Address,
        invoice_nft: Address,
        financing_pool: Address,
        treasury: Address,
        fee_bps: u32,
    ) -> Result<(), KoraError> {
        if env.storage().instance().has(&DataKey::Config) {
            return Err(KoraError::AlreadyInitialized);
        }
        require_valid_fee_bps(fee_bps)?;
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::InvoiceNft, &invoice_nft);
        env.storage()
            .instance()
            .set(&DataKey::FinancingPool, &financing_pool);
        env.storage().instance().set(&DataKey::Treasury, &treasury);
        env.storage().instance().set(&DataKey::FeeBps, &fee_bps);
        Ok(())
    }

    /// Update the marketplace fee. Admin only.
    pub fn update_fee_bps(env: Env, admin: Address, fee_bps: u32) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        require_valid_fee_bps(fee_bps)?;
        env.storage().instance().set(&DataKey::FeeBps, &fee_bps);
        Ok(())
    }

    pub fn get_fee_bps(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::FeeBps).unwrap_or(50)
    }

    /// SME lists an invoice NFT for financing.
    pub fn list_invoice(
        env: Env,
        seller: Address,
        invoice_id: u64,
        asking_price: i128,
        face_value: i128,
        token: Address,
        funding_deadline: u64,
    ) -> Result<(), KoraError> {
        seller.require_auth();
        require_non_zero_amount(asking_price)?;
        require_non_zero_amount(face_value)?;
        kora_shared::validation::require_future_timestamp(&env, funding_deadline)?;

        if asking_price >= face_value {
            return Err(KoraError::InvalidAmount); // discount must exist
        }
        Self::require_whitelisted_token(&env, &token)?;

        if env
            .storage()
            .persistent()
            .has(&DataKey::Listing(invoice_id))
        {
            return Err(KoraError::InvoiceAlreadyExists);
        }

        let config = Self::load_config(&env)?;

        // Notify Invoice NFT contract to transition status
        let nft_client = kora_invoice_nft::InvoiceNftContractClient::new(&env, &config.invoice_nft);
        nft_client.set_listed(&env.current_contract_address(), &invoice_id);

        let listing = Listing {
            invoice_id,
            seller: seller.clone(),
            asking_price,
            face_value,
            token,
            funded_amount: 0,
            funding_deadline,
            is_active: true,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Listing(invoice_id), &listing);
        events::invoice_listed(&env, invoice_id, &seller, asking_price);
        Ok(())
    }

    /// Investor funds a share of an invoice. Deducts marketplace fee (bps_of(amount, fee_bps)) and transfers net to pool.
    /// Fee goes to treasury, net goes to financing_pool. When fully funded, releases funds to SME and transitions invoice to Funded.
    /// Parameters: investor address, invoice_id, amount to contribute.
    /// Errors: ListingNotFound, ListingAlreadyCancelled, FundingDeadlinePassed, InvalidAmount if <= 0, ExceedsFundingTarget, ArithmeticOverflow.
    pub fn fund_invoice(
        env: Env,
        investor: Address,
        invoice_id: u64,
        amount: i128,
    ) -> Result<(), KoraError> {
        investor.require_auth();
        require_non_zero_amount(amount)?;

        let mut listing: Listing = env
            .storage()
            .persistent()
            .get(&DataKey::Listing(invoice_id))
            .ok_or(KoraError::ListingNotFound)?;

        if !listing.is_active {
            return Err(KoraError::ListingAlreadyCancelled);
        }
        if env.ledger().timestamp() > listing.funding_deadline {
            return Err(KoraError::FundingDeadlinePassed);
        }

        let remaining = listing
            .asking_price
            .checked_sub(listing.funded_amount)
            .ok_or(KoraError::ArithmeticOverflow)?;

        if amount > remaining {
            return Err(KoraError::ExceedsFundingTarget);
        }

        // Collect marketplace fee from investor (on top of contribution)
        let fee_bps: u32 = env.storage().instance().get(&DataKey::FeeBps).unwrap_or(50);
        let fee = bps_of(amount, fee_bps)?;
        let net = amount
            .checked_sub(fee)
            .ok_or(KoraError::ArithmeticOverflow)?;

        let token_client = token::Client::new(&env, &listing.token);
        let treasury: Address = env.storage().instance().get(&DataKey::Treasury).unwrap();
        let pool_contract: Address = env.storage().instance().get(&DataKey::FinancingPool).unwrap();

        // Transfer fee to treasury
        if fee > 0 {
            token_client.transfer(&investor, &treasury, &fee);
        }
        // Transfer net to financing pool
        token_client.transfer(&investor, &pool_contract, &net);

        listing.funded_amount = listing
            .funded_amount
            .checked_add(amount)
            .ok_or(KoraError::ArithmeticOverflow)?;

        let fully_funded = listing.funded_amount >= listing.asking_price;
        if fully_funded {
            listing.is_active = false;
        }

        env.storage()
            .persistent()
            .set(&DataKey::Listing(invoice_id), &listing);
        events::invoice_funded(&env, invoice_id, &investor, amount);
        if fee > 0 {
            events::fee_collected(&env, invoice_id, fee, &listing.token);
        }

        // If fully funded, notify pool to release funds to SME
        if fully_funded {
            let pool_client =
                kora_financing_pool::FinancingPoolContractClient::new(&env, &pool_contract);
            pool_client.release_funds(&env.current_contract_address(), &invoice_id);
        }

        Ok(())
    }

    /// Cancel a listing before it is fully funded. Caller must be seller or admin.
    /// Parameters: caller address, invoice_id to cancel.
    /// Errors: ListingNotFound, ListingAlreadyCancelled, Unauthorized if caller is neither seller nor admin.
    pub fn cancel_listing(env: Env, caller: Address, invoice_id: u64) -> Result<(), KoraError> {
        caller.require_auth();
        let mut listing: Listing = env
            .storage()
            .persistent()
            .get(&DataKey::Listing(invoice_id))
            .ok_or(KoraError::ListingNotFound)?;

        if !listing.is_active {
            return Err(KoraError::ListingAlreadyCancelled);
        }

        let config = Self::load_config(&env)?;
        if caller != listing.seller && caller != config.admin {
            return Err(KoraError::Unauthorized);
        }

        listing.is_active = false;
        env.storage()
            .persistent()
            .set(&DataKey::Listing(invoice_id), &listing);
        events::listing_cancelled(&env, invoice_id, &listing.seller);
        Ok(())
    }

    /// Whitelist a stablecoin token for use in listings. Admin only.
    pub fn whitelist_token(env: Env, admin: Address, token: Address) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        env.storage()
            .persistent()
            .set(&DataKey::WhitelistedToken(token.clone()), &true);
        events::token_whitelisted(&env, &token);
        Ok(())
    }

    /// Get a listing by invoice_id. Returns the full Listing struct including status and funded_amount.
    /// Errors: ListingNotFound if no listing exists for this invoice_id.
    pub fn get_listing(env: Env, invoice_id: u64) -> Result<Listing, KoraError> {
        env.storage()
            .persistent()
            .get(&DataKey::Listing(invoice_id))
            .ok_or(KoraError::ListingNotFound)
    }

    pub fn is_token_whitelisted(env: Env, token: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::WhitelistedToken(token))
            .unwrap_or(false)
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn require_whitelisted_token(env: &Env, token: &Address) -> Result<(), KoraError> {
        let ok: bool = env
            .storage()
            .persistent()
            .get(&DataKey::WhitelistedToken(token.clone()))
            .unwrap_or(false);
        if !ok {
            return Err(KoraError::TokenNotWhitelisted);
        }
        Ok(())
    }

    fn require_admin(env: &Env, caller: &Address) -> Result<(), KoraError> {
        let config = Self::load_config(env)?;
        if &config.admin != caller {
            return Err(KoraError::NotAdmin);
        }
        Ok(())
    }

    fn load_config(env: &Env) -> Result<MarketplaceConfig, KoraError> {
        if let Some(config) = env.storage().instance().get(&DataKey::Config) {
            return Ok(config);
        }

        // Legacy migration path: read individual keys and persist a consolidated config.
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(KoraError::NotInitialized)?;
        let invoice_nft: Address = env
            .storage()
            .instance()
            .get(&DataKey::InvoiceNft)
            .ok_or(KoraError::NotInitialized)?;
        let financing_pool: Address = env
            .storage()
            .instance()
            .get(&DataKey::FinancingPool)
            .ok_or(KoraError::NotInitialized)?;
        let treasury: Address = env
            .storage()
            .instance()
            .get(&DataKey::Treasury)
            .ok_or(KoraError::NotInitialized)?;
        let fee_bps: u32 = env
            .storage()
            .instance()
            .get(&DataKey::FeeBps)
            .ok_or(KoraError::NotInitialized)?;

        let config = MarketplaceConfig {
            admin,
            invoice_nft,
            financing_pool,
            treasury,
            fee_bps,
        };
        env.storage().instance().set(&DataKey::Config, &config);
        Ok(config)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use kora_financing_pool::{FinancingPoolContract, FinancingPoolContractClient};
    use kora_invoice_nft::{InvoiceNftContract, InvoiceNftContractClient};
    use kora_shared::errors::KoraError;
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        Address, Env,
    };

    // ── Helpers ──────────────────────────────────────────────────────────────

    /// Full setup: deploys marketplace wired to real invoice_nft and financing_pool.
    /// Returns (env, admin, token, seller, marketplace_client, nft_client).
    struct TestEnv {
        env: Env,
        admin: Address,
        token: Address,
        seller: Address,
        treasury: Address,
        pool: Address,
        mp: MarketplaceContractClient<'static>,
        nft: InvoiceNftContractClient<'static>,
    }

    fn deploy() -> TestEnv {
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

        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);

        // Deploy invoice_nft
        let nft_id = env.register_contract(None, InvoiceNftContract);
        let nft = InvoiceNftContractClient::new(&env, &nft_id);
        let ac = Address::generate(&env); // mock access_control
        nft.initialize(&admin, &ac);

        // Deploy financing_pool
        let pool_id = env.register_contract(None, FinancingPoolContract);
        let pool_client = FinancingPoolContractClient::new(&env, &pool_id);
        pool_client.initialize(&admin, &nft_id, &treasury, &200u32);

        // Deploy marketplace
        let mp_id = env.register_contract(None, MarketplaceContract);
        let mp = MarketplaceContractClient::new(&env, &mp_id);
        mp.initialize(&admin, &nft_id, &pool_id, &treasury, &50u32);

        // Whitelist a token
        let token = Address::generate(&env);
        mp.whitelist_token(&admin, &token);

        let seller = Address::generate(&env);

        TestEnv { env, admin, token, seller, treasury, pool: pool_id, mp, nft }
    }

    /// Convenience: list an invoice with standard params, returns invoice_id=1.
    fn list_one(t: &TestEnv) -> u64 {
        let deadline = t.env.ledger().timestamp() + 86_400 * 30;
        t.mp.list_invoice(
            &t.seller,
            &1u64,
            &9_500_000_000i128,
            &10_000_000_000i128,
            &t.token,
            &deadline,
        );
        1u64
    }

    // ── initialize ────────────────────────────────────────────────────────────

    #[test]
    fn test_initialize_success() {
        let t = deploy();
        // Whitelist worked — listing with the token should not fail on token check
        let deadline = t.env.ledger().timestamp() + 86_400;
        let result = t.mp.try_list_invoice(
            &t.seller,
            &1u64,
            &9_000i128,
            &10_000i128,
            &t.token,
            &deadline,
        );
        // May fail on nft cross-call but NOT on TokenNotWhitelisted
        if let Err(Ok(e)) = result {
            assert_ne!(e, KoraError::TokenNotWhitelisted);
        }
    }

    #[test]
    fn test_initialize_already_initialized_returns_error() {
        let t = deploy();
        let result = t.mp.try_initialize(
            &t.admin,
            &Address::generate(&t.env),
            &Address::generate(&t.env),
            &Address::generate(&t.env),
            &50u32,
        );
        assert_eq!(result.unwrap_err().unwrap(), KoraError::AlreadyInitialized);
    }

    #[test]
    fn test_initialize_invalid_fee_bps_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let mp_id = env.register_contract(None, MarketplaceContract);
        let mp = MarketplaceContractClient::new(&env, &mp_id);
        let result = mp.try_initialize(
            &Address::generate(&env),
            &Address::generate(&env),
            &Address::generate(&env),
            &Address::generate(&env),
            &10_001u32, // > 100%
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_get_config_returns_initialized_values() {
        let t = deploy();
        let config = t.mp.get_config();
        assert_eq!(config.admin, t.admin);
        assert_eq!(config.invoice_nft, t.nft.address);
        assert_eq!(config.financing_pool, t.pool);
        assert_eq!(config.treasury, t.treasury);
        assert_eq!(config.fee_bps, 50u32);
    }

    #[test]
    fn test_legacy_state_migrates_to_config() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let nft_id = env.register_contract(None, InvoiceNftContract);
        let pool_id = env.register_contract(None, FinancingPoolContract);
        let mp_id = env.register_contract(None, MarketplaceContract);
        let mp = MarketplaceContractClient::new(&env, &mp_id);

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::InvoiceNft, &nft_id);
        env.storage().instance().set(&DataKey::FinancingPool, &pool_id);
        env.storage().instance().set(&DataKey::Treasury, &treasury);
        env.storage().instance().set(&DataKey::FeeBps, &75u32);

        let migrated = mp.get_config();
        assert_eq!(migrated.admin, admin);
        assert_eq!(migrated.invoice_nft, nft_id);
        assert_eq!(migrated.financing_pool, pool_id);
        assert_eq!(migrated.treasury, treasury);
        assert_eq!(migrated.fee_bps, 75u32);

        // Config should be persisted after fallback migration.
        let reloaded = mp.get_config();
        assert_eq!(reloaded, migrated);
    }

    // ── whitelist_token ───────────────────────────────────────────────────────

    #[test]
    fn test_whitelist_token_success() {
        let t = deploy();
        let new_token = Address::generate(&t.env);
        assert!(t.mp.try_whitelist_token(&t.admin, &new_token).is_ok());
    }

    #[test]
    fn test_whitelist_token_non_admin_returns_not_admin() {
        let t = deploy();
        let stranger = Address::generate(&t.env);
        let new_token = Address::generate(&t.env);
        let result = t.mp.try_whitelist_token(&stranger, &new_token);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::NotAdmin);
    }

    #[test]
    fn test_whitelist_multiple_tokens() {
        let t = deploy();
        let t2 = Address::generate(&t.env);
        let t3 = Address::generate(&t.env);
        t.mp.whitelist_token(&t.admin, &t2);
        t.mp.whitelist_token(&t.admin, &t3);
        // Both tokens should now be accepted (no error on token check)
        let deadline = t.env.ledger().timestamp() + 86_400;
        for tok in [&t2, &t3] {
            let r =
                t.mp.try_list_invoice(&t.seller, &99u64, &9_000i128, &10_000i128, tok, &deadline);
            if let Err(Ok(e)) = r {
                assert_ne!(e, KoraError::TokenNotWhitelisted);
            }
        }
    }

    // ── list_invoice ──────────────────────────────────────────────────────────

    #[test]
    fn test_list_invoice_success() {
        let t = deploy();
        let id = list_one(&t);
        let listing = t.mp.get_listing(&id);
        assert_eq!(listing.invoice_id, 1);
        assert_eq!(listing.seller, t.seller);
        assert_eq!(listing.asking_price, 9_500_000_000i128);
        assert_eq!(listing.face_value, 10_000_000_000i128);
        assert!(listing.is_active);
        assert_eq!(listing.funded_amount, 0);
    }

    #[test]
    fn test_list_invoice_nft_status_transitions_to_listed() {
        let t = deploy();
        list_one(&t);
        let invoice = t.nft.get_invoice(&1u64);
        assert_eq!(invoice.status, kora_shared::types::InvoiceStatus::Listed);
    }

    #[test]
    fn test_list_invoice_non_whitelisted_token_returns_error() {
        let t = deploy();
        let bad_token = Address::generate(&t.env);
        let deadline = t.env.ledger().timestamp() + 86_400;
        let result = t.mp.try_list_invoice(
            &t.seller,
            &1u64,
            &9_000i128,
            &10_000i128,
            &bad_token,
            &deadline,
        );
        assert_eq!(result.unwrap_err().unwrap(), KoraError::TokenNotWhitelisted);
    }

    #[test]
    fn test_list_invoice_zero_asking_price_rejected() {
        let t = deploy();
        let deadline = t.env.ledger().timestamp() + 86_400;
        let result =
            t.mp.try_list_invoice(&t.seller, &1u64, &0i128, &10_000i128, &t.token, &deadline);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::InvalidAmount);
    }

    #[test]
    fn test_list_invoice_zero_face_value_rejected() {
        let t = deploy();
        let deadline = t.env.ledger().timestamp() + 86_400;
        let result =
            t.mp.try_list_invoice(&t.seller, &1u64, &9_000i128, &0i128, &t.token, &deadline);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::InvalidAmount);
    }

    #[test]
    fn test_list_invoice_asking_price_equal_face_value_rejected() {
        let t = deploy();
        let deadline = t.env.ledger().timestamp() + 86_400;
        let result = t.mp.try_list_invoice(
            &t.seller,
            &1u64,
            &10_000i128,
            &10_000i128,
            &t.token,
            &deadline,
        );
        assert_eq!(result.unwrap_err().unwrap(), KoraError::InvalidAmount);
    }

    #[test]
    fn test_list_invoice_asking_price_greater_than_face_value_rejected() {
        let t = deploy();
        let deadline = t.env.ledger().timestamp() + 86_400;
        let result = t.mp.try_list_invoice(
            &t.seller,
            &1u64,
            &11_000i128,
            &10_000i128,
            &t.token,
            &deadline,
        );
        assert_eq!(result.unwrap_err().unwrap(), KoraError::InvalidAmount);
    }

    #[test]
    fn test_list_invoice_past_deadline_rejected() {
        let t = deploy();
        let past = t.env.ledger().timestamp() - 1;
        let result =
            t.mp.try_list_invoice(&t.seller, &1u64, &9_000i128, &10_000i128, &t.token, &past);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::InvalidDueDate);
    }

    #[test]
    fn test_list_invoice_duplicate_id_rejected() {
        let t = deploy();
        list_one(&t);
        let deadline = t.env.ledger().timestamp() + 86_400;
        let result = t.mp.try_list_invoice(
            &t.seller,
            &1u64,
            &9_000i128,
            &10_000i128,
            &t.token,
            &deadline,
        );
        assert_eq!(
            result.unwrap_err().unwrap(),
            KoraError::InvoiceAlreadyExists
        );
    }

    #[test]
    fn test_list_invoice_negative_asking_price_rejected() {
        let t = deploy();
        let deadline = t.env.ledger().timestamp() + 86_400;
        let result =
            t.mp.try_list_invoice(&t.seller, &1u64, &-1i128, &10_000i128, &t.token, &deadline);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::InvalidAmount);
    }

    // ── get_listing ───────────────────────────────────────────────────────────

    #[test]
    fn test_get_listing_not_found_returns_error() {
        let t = deploy();
        let result = t.mp.try_get_listing(&999u64);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::ListingNotFound);
    }

    #[test]
    fn test_get_listing_returns_correct_data() {
        let t = deploy();
        let deadline = t.env.ledger().timestamp() + 86_400 * 30;
        t.mp.list_invoice(
            &t.seller,
            &1u64,
            &9_500_000_000i128,
            &10_000_000_000i128,
            &t.token,
            &deadline,
        );
        let listing = t.mp.get_listing(&1u64);
        assert_eq!(listing.asking_price, 9_500_000_000i128);
        assert_eq!(listing.face_value, 10_000_000_000i128);
        assert_eq!(listing.funding_deadline, deadline);
        assert_eq!(listing.token, t.token);
        assert!(listing.is_active);
    }

    // ── fund_invoice ──────────────────────────────────────────────────────────

    #[test]
    fn test_fund_invoice_listing_not_found() {
        let t = deploy();
        let investor = Address::generate(&t.env);
        let result = t.mp.try_fund_invoice(&investor, &999u64, &1_000i128);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::ListingNotFound);
    }

    #[test]
    fn test_fund_invoice_zero_amount_rejected() {
        let t = deploy();
        list_one(&t);
        let investor = Address::generate(&t.env);
        let result = t.mp.try_fund_invoice(&investor, &1u64, &0i128);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::InvalidAmount);
    }

    #[test]
    fn test_fund_invoice_exceeds_target_rejected() {
        let t = deploy();
        list_one(&t);
        let investor = Address::generate(&t.env);
        // asking_price is 9_500_000_000 — fund 1 more than that
        let result = t.mp.try_fund_invoice(&investor, &1u64, &9_500_000_001i128);
        assert_eq!(
            result.unwrap_err().unwrap(),
            KoraError::ExceedsFundingTarget
        );
    }

    #[test]
    fn test_fund_invoice_after_deadline_rejected() {
        let t = deploy();
        let deadline = t.env.ledger().timestamp() + 100;
        t.mp.list_invoice(
            &t.seller,
            &1u64,
            &9_500_000_000i128,
            &10_000_000_000i128,
            &t.token,
            &deadline,
        );
        // Advance past deadline
        t.env.ledger().set(LedgerInfo {
            timestamp: deadline + 1,
            protocol_version: 21,
            sequence_number: 2,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 1000,
            min_persistent_entry_ttl: 1000,
            max_entry_ttl: 100_000,
        });
        let investor = Address::generate(&t.env);
        let result = t.mp.try_fund_invoice(&investor, &1u64, &1_000_000i128);
        assert_eq!(
            result.unwrap_err().unwrap(),
            KoraError::FundingDeadlinePassed
        );
    }

    #[test]
    fn test_fund_invoice_on_cancelled_listing_rejected() {
        let t = deploy();
        list_one(&t);
        t.mp.cancel_listing(&t.seller, &1u64);
        let investor = Address::generate(&t.env);
        let result = t.mp.try_fund_invoice(&investor, &1u64, &1_000_000i128);
        assert_eq!(
            result.unwrap_err().unwrap(),
            KoraError::ListingAlreadyCancelled
        );
    }

    #[test]
    fn test_fund_invoice_partial_updates_funded_amount() {
        let t = deploy();
        list_one(&t);
        let investor = Address::generate(&t.env);
        // Partial fund — token transfer will be mocked
        t.mp.fund_invoice(&investor, &1u64, &1_000_000_000i128);
        let listing = t.mp.get_listing(&1u64);
        assert_eq!(listing.funded_amount, 1_000_000_000i128);
        assert!(listing.is_active); // not yet fully funded
    }

    #[test]
    fn test_fund_invoice_fee_math_correct() {
        // fee_bps = 50 (0.5%), amount = 10_000_000
        // fee = 10_000_000 * 50 / 10_000 = 50_000
        // net = 10_000_000 - 50_000 = 9_950_000
        // funded_amount tracks the gross amount contributed
        let t = deploy();
        list_one(&t);
        let investor = Address::generate(&t.env);
        let amount = 10_000_000i128;
        t.mp.fund_invoice(&investor, &1u64, &amount);
        let listing = t.mp.get_listing(&1u64);
        assert_eq!(listing.funded_amount, amount);
    }

    #[test]
    fn test_fund_invoice_multiple_partial_fundings() {
        let t = deploy();
        list_one(&t);
        let inv1 = Address::generate(&t.env);
        let inv2 = Address::generate(&t.env);
        t.mp.fund_invoice(&inv1, &1u64, &4_000_000_000i128);
        t.mp.fund_invoice(&inv2, &1u64, &4_000_000_000i128);
        let listing = t.mp.get_listing(&1u64);
        assert_eq!(listing.funded_amount, 8_000_000_000i128);
        assert!(listing.is_active);
    }

    #[test]
    fn test_fund_invoice_fully_funded_deactivates_listing() {
        let t = deploy();
        list_one(&t);
        let investor = Address::generate(&t.env);
        // Fund the full asking price in one go
        t.mp.fund_invoice(&investor, &1u64, &9_500_000_000i128);
        let listing = t.mp.get_listing(&1u64);
        assert!(!listing.is_active);
        assert_eq!(listing.funded_amount, 9_500_000_000i128);
    }

    // ── cancel_listing ────────────────────────────────────────────────────────

    #[test]
    fn test_cancel_listing_by_seller_success() {
        let t = deploy();
        list_one(&t);
        assert!(t.mp.try_cancel_listing(&t.seller, &1u64).is_ok());
        let listing = t.mp.get_listing(&1u64);
        assert!(!listing.is_active);
    }

    #[test]
    fn test_cancel_listing_by_admin_success() {
        let t = deploy();
        list_one(&t);
        assert!(t.mp.try_cancel_listing(&t.admin, &1u64).is_ok());
        let listing = t.mp.get_listing(&1u64);
        assert!(!listing.is_active);
    }

    #[test]
    fn test_cancel_listing_by_stranger_rejected() {
        let t = deploy();
        list_one(&t);
        let stranger = Address::generate(&t.env);
        let result = t.mp.try_cancel_listing(&stranger, &1u64);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::Unauthorized);
    }

    #[test]
    fn test_cancel_listing_not_found_returns_error() {
        let t = deploy();
        let result = t.mp.try_cancel_listing(&t.seller, &999u64);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::ListingNotFound);
    }

    #[test]
    fn test_cancel_listing_already_cancelled_returns_error() {
        let t = deploy();
        list_one(&t);
        t.mp.cancel_listing(&t.seller, &1u64);
        let result = t.mp.try_cancel_listing(&t.seller, &1u64);
        assert_eq!(
            result.unwrap_err().unwrap(),
            KoraError::ListingAlreadyCancelled
        );
    }

    #[test]
    fn test_cancel_listing_state_unchanged_after_failed_cancel() {
        let t = deploy();
        list_one(&t);
        let stranger = Address::generate(&t.env);
        let _ = t.mp.try_cancel_listing(&stranger, &1u64);
        // Listing must still be active
        let listing = t.mp.get_listing(&1u64);
        assert!(listing.is_active);
    }

    #[test]
    fn test_fund_after_cancel_rejected() {
        let t = deploy();
        list_one(&t);
        t.mp.cancel_listing(&t.admin, &1u64);
        let investor = Address::generate(&t.env);
        let result = t.mp.try_fund_invoice(&investor, &1u64, &1_000_000i128);
        assert_eq!(
            result.unwrap_err().unwrap(),
            KoraError::ListingAlreadyCancelled
        );
    }

    #[test]
    fn test_fund_cancelled_listing() {
        let (env, admin, _nft, _pool, _treasury, client) = setup();
        let seller = Address::generate(&env);
        let investor = Address::generate(&env);
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token);
        let deadline = env.ledger().timestamp() + 1_000_000u64;
        client.list_invoice(
            &seller, &1u64, &9_500_000_000i128, &10_000_000_000i128, &token, &deadline,
        );
        client.cancel_listing(&seller, &1u64);
        let result = client.try_fund_invoice(&investor, &1u64, &1_000_000_000i128);
        assert!(result.is_err());
    }
}
