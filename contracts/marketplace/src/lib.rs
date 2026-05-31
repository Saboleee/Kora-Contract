#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, token, Address, Env,
};
use kora_shared::{
    errors::KoraError,
    events,
    reentrancy::ReentrancyGuard,
    types::Listing,
    validation::{bps_of, require_non_zero_amount, require_valid_fee_bps, safe_add, safe_sub},
};

s (~30 days in ledgers at ~5s/ledger) ─────────────────────────
const PERSISTENT_TTL_THRESHOLD: u32 = 518_400;
const PERSISTENT_TTL_BUMP: u32 = 518_400;

// ── Stora───────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Config,
    Listing(u64),
    WhitelistedToken(Address),
}

// ── Config st─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketplaceConfig {
    pub admin: Address,
    pub invoice_nft: Address,
    pub financing_pool: Address,
    pub treasury: Address,
    pub fee_bps: u32,
}

// ── Contr─────────────────────────────

ract]
pub struct MarketplaceContract;

#[contractimpl]
impl MarketplaceContract {
    /// One-time initializer. Stores a consolidated config in instance storage.
    pub fn initialize(
        env: Env,
        admin: Address,
        invoice_nft: Address,
        financing_pool: Address,
        treasury: Address,
        fee_bps: u32,
    ) -> 
        if env.storage().instance().has(::Config) {
            return Err(KoraError::AlreadyInitialized);
        }
        require_valid_fee_bps(fee_bps)?;

        let config = MarketplaceConfig {
            admin,
     ,
            financing_pool,
            treasury,
            fee_bps,
        };
        env.storage().instance().set(&DataKey::Config, &config);
        Ok(())
    }

    /pdate the marketplace fee. Admin only.
 fee_bps: u32) -> Result<(), KoraError> {
        admin.require_auth();
        let mut config = Self::load_config(&env)?;
        if config.admin != admin {
            return Err(KoraError::NotAdmin);
        }
        require_valid_fee_bps(fee_bps)?;

        let old_bps = config.fee_bps;
        config.fee_bps = fee_bps;
        env.storage().instance().set(&DataKey::Config, &config);
        events::fee_rate_updated(&env, &admin, old_bps, fee_bps);
        Ok(())
    }

    /// Returns the current fein basis points.
    pub fn get_fee_bps(env: Env) -> Result<u32, KoraError> {
        Ok(Self::load_config(&env)?.fee_bps)
    }

    /// Returns the full config struct.
    pub fn get_config(env: Env) -> Result<MarketplaceConfig, KoraError> {
        Self::load_config(&env)
    }

    /// Whitelist a stablecoin token for use in listings. Admin only.
    pub fn whitelist_token(env: Env, admin: Address, token: Address) -> Result<(), KoraError> {
        aauth();

        if config.admin != admin {
            return Err(KoraError::NotAdmin);
        }
        env.storage()
            .persistent()
            .set(&DataKey::WhitelistedToken(token.clone()), &true);
        Self::bump_persistent(&env, &DataKey::WhitelistedToken(token.clone()));
        events::token_whitelisted(&env, &token);
        Ok(())
    }

    /// Remove a token from the whitelist. Admin only.
    pub fn remove_token_whitelist(
        env: Env,
        admin: Address,
        token: Address,
    ) -> Result<(), KoraError> {
        admin.require_auth();
        let coad_config(&env)?;
        if config.admin != admin {
ror::NotAdmin);
        }
        if !env
            .storage()
            .persistent()
            .get::<_, bool>(&DataKey::WhitelistedToken(token.clone()))
            .unwrap_or(false)
        {
            return Err(KoraError::TokenNotWhitelisted);
        }
        env.storage()
            .persistent()
            .remove(&DataKey::WhitelistedToken(token));
        Ok(())
    }

    /// SME lists an invoice NFT for financing.
    ///
    /// Invariants enforced:
    /// -face_value` > 0
    /// - `asking_price` < `face_value` (discount must exist)
    /// - `funding_deadline` is strictly in the future
    /// - `token` is whitelisted
    /// - No existing active listing for this `invoice_id`
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

        // ── Input validation ──────────────────────────────────────────────────
        require_non_zero_amount(asking_price)?;
alue)?;
        kora_shared::validation::require_future_timestamp(&env, funding_deadline)?;

        if asking_price >= face_value {
            return Err(KoraError::InvalidAmount);
        }
        Self::require_whitelisted_token(&env, &token)?;

        if env.storage().persistent().has(&DataKey::Listing(invoice_id)) {
            return Err(KoraError::InvoiceAlreadyExists);
        }

        // ── Reentrancy guard ──────────────────────────────────────────────────
        let _guard = Reent:new(&env)?;

        let config = Self::load_config(&env)?;

        // ── Cross-contract: transition NFT to Listed ──────────────────────────
        let nft_client =
            kora_invoice_nft::InvoiceNftContractClient::new(&env, &config.invoice_nft);
        nft_client.set_listed(&env.current_contract_address(), &invoice_id);

        // ── Effects ───────────────────────────────────────────────────────────
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
        Self::bump_persistent(&env, &DataKey::Listing(invoice_id));

        events::invoice_listed(&env, invoice_id, &seller, asking_price);
        Ok(())
    }

    /// Investor funds a share of the invoice.
    ///
    /// Fee model: the fee is charged ON TOP of the investor's contribution.
    ///   - `amount` is the gross contribution that counts toward `asking_price`
unt × fee_bps / 10_000` is transferred to treasury
    ///   - `amount` is transferred to the financing pool
    ///   - `funded_amount` tracks the gross contribution (not net)
    ///
    /// This ensures the pool always receives the full `asking_price` when
    /// fully funded, and the fee is an additional cost to the investor.
    pub fn fund_invoice(
        env: Env,
        investor: Address,
        invoice_id: u64,
        amount: i128,
    ) -> Result<(), KoraError> {
        investor.require_auth();

        // ── Input validation ──────────────────────────────────────────────────
_amount(amount)?;

        // ── Load and validate listing ─────────────────────────────────────────
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

        let remaining = safe_sub(listing.askinnded_amount)?;
        if amount > remaining {
turn Err(KoraError::ExceedsFundingTarget);
        }

        // ── Reentrancy guard ──────────────────────────────────────────────────
        let _guard = ReentrancyGuard::new(&env)?;

        // ── Fee calculation (safe arithmetic) ─────────────────────────────────
        let config = Self::load_config(&env)?;
        let fee = bps_of(amount, config.fee_bps)?;

        let token_client = token::Client::new(&env, &listing.token);

        // ── Interact────────────────
        // Transfer fee to treasury (if non-zero)
        if fee > 0 {
            token_client.transfer(&investor, &config.treasury, &fee);
        }
        // Transfer full contribution amount to financing pool
        token_client.transfer(&investor, &config.financing_pool, &amount);

        // ── Effects: update state after transfers ─────────────────────────────
        listing.funded_amount = safe_add(listing.funded_amount, amount)?;

        let fully_funded = listing.funded_amoing.asking_price;
        if fully_funded {
            listing.is_active = false;
        }

     ge()
            .persistent()
            .set(&DataKey::Listing(invoice_id), &listing);
        Self::bump_persistent(&env, &DataKey::Listing(invoice_id));

        events::invoice_funded(&env, invoice_id, &investor, amount);
        if fee > 0 {
            events::fee_collected(&env, invoice_id, fee, &listing.token);
        }

        // ── Cross-contract: notify pool to release funds to SME ───────────────
        if fully_funded {
            let pool_client = kora_financing_potClient::new(
                &env,
                &confipool,
            );
            pool_client.release_funds(&env.current_contract_address(), &invoice_id);
        }

        Ok(())
    }

    /// SME or admin cancels a listing before it is fully funded.
    /// Notifies the invoice NFT to revert status back to Created.
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

        // ── Reentrancy guard ──────────────────────────────────────────────────
        let _guard = ReentrancyGuard::new(&env)?;

        // ── Effects ───────────────────────────────────────────────────────────
        listing.is_active = false;
        env.storage()
            .persistent()
            .set(&DataKey::Listing(invoice_id), &listing);
        Self::bump_persistent(&env, &DataKey::Listing(invoice_id));

        events::listing_cancelled(&env, invoice_id, &listing.seller);
        Ok(())
    }

    /// Returns a listing by invoice ID.
    pub fn get_listing(env: Env, invoice_id: u64) -> Result<Listing, KoraError> {
        env.storage()
            .persistent()
            .get(&DataKey::Listing(invoice_id))
            .ok_or(KoraError::ListingNotFound)
    }

    /// Returns whether a token is whitelisted.
    pub fn is_token_whitelisted(env: Env, token: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::WhitelistedToken(token))
            .unwrap_or(false)
    }

    /rivate helpers ───────────────────────────────────────────────────────

    fn load_config(env: &Env) -> Result<MarketplaceConfig, KoraError> {
        env.storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(KoraError::NotInitialized)
    }

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

    fn bump_persistent(env: &Env, key: &DataKey) {
        env.storage()
            .persistent()
            .extend_ttl(key, PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_BUMP);
    }
}
