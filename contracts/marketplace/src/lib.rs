#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, token, Address, Env,
};
use kora_shared::{
    errors::KoraError,
    events,
    types::Listing,
    validation::{bps_of, require_non_zero_amount, require_valid_fee_bps},
};

// ── Storage Keys ─────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Listing(u64),
    Admin,
    InvoiceNft,
    FinancingPool,
    Treasury,
    FeeBps,
    WhitelistedToken(Address),
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct MarketplaceContract;

#[contractimpl]
impl MarketplaceContract {
    pub fn initialize(
        env: Env,
        admin: Address,
        invoice_nft: Address,
        financing_pool: Address,
        treasury: Address,
        fee_bps: u32,
    ) -> Result<(), KoraError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(KoraError::AlreadyInitialized);
        }
        require_valid_fee_bps(fee_bps)?;
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::InvoiceNft, &invoice_nft);
        env.storage().instance().set(&DataKey::FinancingPool, &financing_pool);
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

        if env.storage().persistent().has(&DataKey::Listing(invoice_id)) {
            return Err(KoraError::InvoiceAlreadyExists);
        }

        // Notify Invoice NFT contract to transition status
        let nft_contract: Address = env.storage().instance().get(&DataKey::InvoiceNft).unwrap();
        let nft_client = kora_invoice_nft::InvoiceNftContractClient::new(&env, &nft_contract);
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
        env.storage().persistent().set(&DataKey::Listing(invoice_id), &listing);
        events::invoice_listed(&env, invoice_id, &seller, asking_price);
        Ok(())
    }

    /// Investor funds a share of the invoice.
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
        let net = amount.checked_sub(fee).ok_or(KoraError::ArithmeticOverflow)?;

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

        env.storage().persistent().set(&DataKey::Listing(invoice_id), &listing);
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

    /// SME or admin cancels a listing before it is fully funded.
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

        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if caller != listing.seller && caller != admin {
            return Err(KoraError::Unauthorized);
        }

        listing.is_active = false;
        env.storage().persistent().set(&DataKey::Listing(invoice_id), &listing);
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
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Env};

    fn setup() -> (Env, Address, Address, Address, Address, MarketplaceContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, MarketplaceContract);
        let client = MarketplaceContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let nft = Address::generate(&env);
        let pool = Address::generate(&env);
        let treasury = Address::generate(&env);
        client.initialize(&admin, &nft, &pool, &treasury, &50u32);
        (env, admin, nft, pool, treasury, client)
    }

    #[test]
    fn test_initialize_success() {
        let (_env, _admin, _nft, _pool, _treasury, client) = setup();
        assert_eq!(client.get_fee_bps(), 50);
    }

    #[test]
    fn test_initialize_already_initialized() {
        let (env, admin, nft, pool, treasury, client) = setup();
        let result = client.try_initialize(&admin, &nft, &pool, &treasury, &50u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_initialize_invalid_fee_bps() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, MarketplaceContract);
        let client = MarketplaceContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let nft = Address::generate(&env);
        let pool = Address::generate(&env);
        let treasury = Address::generate(&env);
        let result = client.try_initialize(&admin, &nft, &pool, &treasury, &10_001u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_fee_bps_success() {
        let (_env, admin, _nft, _pool, _treasury, client) = setup();
        client.update_fee_bps(&admin, &100u32);
        assert_eq!(client.get_fee_bps(), 100);
    }

    #[test]
    fn test_update_fee_bps_requires_admin() {
        let (env, _admin, _nft, _pool, _treasury, client) = setup();
        let stranger = Address::generate(&env);
        let result = client.try_update_fee_bps(&stranger, &100u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_fee_bps_invalid_bps() {
        let (_env, admin, _nft, _pool, _treasury, client) = setup();
        let result = client.try_update_fee_bps(&admin, &10_001u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_fee_bps_zero_allowed() {
        let (_env, admin, _nft, _pool, _treasury, client) = setup();
        client.update_fee_bps(&admin, &0u32);
        assert_eq!(client.get_fee_bps(), 0);
    }

    #[test]
    fn test_whitelist_token_success() {
        let (env, admin, _nft, _pool, _treasury, client) = setup();
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token);
        assert!(client.is_token_whitelisted(&token));
    }

    #[test]
    fn test_whitelist_token_non_admin() {
        let (env, _admin, _nft, _pool, _treasury, client) = setup();
        let stranger = Address::generate(&env);
        let token = Address::generate(&env);
        let result = client.try_whitelist_token(&stranger, &token);
        assert!(result.is_err());
    }

    #[test]
    fn test_is_token_whitelisted_false_by_default() {
        let (env, _admin, _nft, _pool, _treasury, client) = setup();
        let token = Address::generate(&env);
        assert!(!client.is_token_whitelisted(&token));
    }

    #[test]
    fn test_list_invoice_non_whitelisted_token() {
        let (env, _admin, _nft, _pool, _treasury, client) = setup();
        let seller = Address::generate(&env);
        let token = Address::generate(&env);
        let deadline = 1_800_000_000u64;
        let result = client.try_list_invoice(
            &seller, &1u64, &9_500_000_000i128, &10_000_000_000i128, &token, &deadline,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_list_invoice_asking_price_must_be_less_than_face_value() {
        let (env, admin, _nft, _pool, _treasury, client) = setup();
        let seller = Address::generate(&env);
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token);
        let deadline = 1_800_000_000u64;
        // asking_price == face_value — no discount, must fail
        let result = client.try_list_invoice(
            &seller, &1u64, &10_000_000_000i128, &10_000_000_000i128, &token, &deadline,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_list_invoice_asking_price_greater_than_face_value() {
        let (env, admin, _nft, _pool, _treasury, client) = setup();
        let seller = Address::generate(&env);
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token);
        let deadline = 1_800_000_000u64;
        let result = client.try_list_invoice(
            &seller, &1u64, &11_000_000_000i128, &10_000_000_000i128, &token, &deadline,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_list_invoice_zero_asking_price() {
        let (env, admin, _nft, _pool, _treasury, client) = setup();
        let seller = Address::generate(&env);
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token);
        let deadline = 1_800_000_000u64;
        let result = client.try_list_invoice(
            &seller, &1u64, &0i128, &10_000_000_000i128, &token, &deadline,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_list_invoice_zero_face_value() {
        let (env, admin, _nft, _pool, _treasury, client) = setup();
        let seller = Address::generate(&env);
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token);
        let deadline = 1_800_000_000u64;
        let result = client.try_list_invoice(
            &seller, &1u64, &9_500_000_000i128, &0i128, &token, &deadline,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_list_invoice_past_deadline() {
        let (env, admin, _nft, _pool, _treasury, client) = setup();
        let seller = Address::generate(&env);
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token);
        let past = env.ledger().timestamp().saturating_sub(1);
        let result = client.try_list_invoice(
            &seller, &1u64, &9_500_000_000i128, &10_000_000_000i128, &token, &past,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_get_listing_not_found() {
        let (_env, _admin, _nft, _pool, _treasury, client) = setup();
        let result = client.try_get_listing(&999u64);
        assert!(result.is_err());
    }

    #[test]
    fn test_fund_invoice_listing_not_found() {
        let (env, _admin, _nft, _pool, _treasury, client) = setup();
        let investor = Address::generate(&env);
        let result = client.try_fund_invoice(&investor, &999u64, &1_000_000_000i128);
        assert!(result.is_err());
    }

    #[test]
    fn test_fund_invoice_zero_amount() {
        let (env, _admin, _nft, _pool, _treasury, client) = setup();
        let investor = Address::generate(&env);
        let result = client.try_fund_invoice(&investor, &1u64, &0i128);
        assert!(result.is_err());
    }

    #[test]
    fn test_fund_invoice_after_deadline() {
        let (env, admin, _nft, _pool, _treasury, client) = setup();
        let seller = Address::generate(&env);
        let investor = Address::generate(&env);
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token);
        let deadline = env.ledger().timestamp() + 100u64;
        client.list_invoice(
            &seller, &1u64, &9_500_000_000i128, &10_000_000_000i128, &token, &deadline,
        );
        env.ledger().set_timestamp(deadline + 1);
        let result = client.try_fund_invoice(&investor, &1u64, &5_000_000_000i128);
        assert!(result.is_err());
    }

    #[test]
    fn test_fund_invoice_exceeds_target() {
        let (env, admin, _nft, _pool, _treasury, client) = setup();
        let seller = Address::generate(&env);
        let investor = Address::generate(&env);
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token);
        let asking_price = 9_500_000_000i128;
        let face_value = 10_000_000_000i128;
        let deadline = env.ledger().timestamp() + 1_000_000u64;
        client.list_invoice(&seller, &1u64, &asking_price, &face_value, &token, &deadline);
        // Try to fund more than asking price
        let result = client.try_fund_invoice(&investor, &1u64, &(asking_price + 1));
        assert!(result.is_err());
    }

    #[test]
    fn test_cancel_listing_not_found() {
        let (env, _admin, _nft, _pool, _treasury, client) = setup();
        let stranger = Address::generate(&env);
        let result = client.try_cancel_listing(&stranger, &999u64);
        assert!(result.is_err());
    }

    #[test]
    fn test_cancel_listing_unauthorized() {
        let (env, admin, _nft, _pool, _treasury, client) = setup();
        let seller = Address::generate(&env);
        let stranger = Address::generate(&env);
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token);
        let deadline = env.ledger().timestamp() + 1_000_000u64;
        client.list_invoice(
            &seller, &1u64, &9_500_000_000i128, &10_000_000_000i128, &token, &deadline,
        );
        let result = client.try_cancel_listing(&stranger, &1u64);
        assert!(result.is_err());
    }

    #[test]
    fn test_cancel_listing_by_seller() {
        let (env, admin, _nft, _pool, _treasury, client) = setup();
        let seller = Address::generate(&env);
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token);
        let deadline = env.ledger().timestamp() + 1_000_000u64;
        client.list_invoice(
            &seller, &1u64, &9_500_000_000i128, &10_000_000_000i128, &token, &deadline,
        );
        client.cancel_listing(&seller, &1u64);
        let listing = client.get_listing(&1u64);
        assert!(!listing.is_active);
    }

    #[test]
    fn test_cancel_listing_by_admin() {
        let (env, admin, _nft, _pool, _treasury, client) = setup();
        let seller = Address::generate(&env);
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token);
        let deadline = env.ledger().timestamp() + 1_000_000u64;
        client.list_invoice(
            &seller, &1u64, &9_500_000_000i128, &10_000_000_000i128, &token, &deadline,
        );
        client.cancel_listing(&admin, &1u64);
        let listing = client.get_listing(&1u64);
        assert!(!listing.is_active);
    }

    #[test]
    fn test_cancel_listing_already_cancelled() {
        let (env, admin, _nft, _pool, _treasury, client) = setup();
        let seller = Address::generate(&env);
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token);
        let deadline = env.ledger().timestamp() + 1_000_000u64;
        client.list_invoice(
            &seller, &1u64, &9_500_000_000i128, &10_000_000_000i128, &token, &deadline,
        );
        client.cancel_listing(&seller, &1u64);
        let result = client.try_cancel_listing(&seller, &1u64);
        assert!(result.is_err());
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
