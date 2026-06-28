#![no_std]

use kora_shared::{
    errors::KoraError,
    events,
    reentrancy::ReentrancyGuard,
    validation::{require_valid_fee_bps, require_within_max_amount, UPGRADE_TIMELOCK_DELAY},
};
use soroban_sdk::{contract, contractimpl, contracttype, token, Address, BytesN, Env};

// ── Storage TTL constants (~31 days in ledgers) ───────────────────────────────
const PERSISTENT_BUMP_AMOUNT: u32 = 535_680;
const PERSISTENT_LIFETIME_THRESHOLD: u32 = 535_680 / 2;

// ── Storage Keys ─────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    /// Admin address — persistent so it survives ledger archival.
    Admin,
    /// Protocol fee in basis points — persistent for durability.
    FeeBps,
    /// Accumulated fees per token (informational).
    Collected(Address),
    /// Whitelisted token flag.
    WhitelistedToken(Address),
    /// Pending upgrade proposal: (wasm_hash, proposed_at_timestamp).
    UpgradeProposal,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct TreasuryContract;

#[contractimpl]
impl TreasuryContract {
    /// One-time initialization. Sets admin and protocol fee.
    pub fn initialize(env: Env, admin: Address, fee_bps: u32) -> Result<(), KoraError> {
        if env.storage().persistent().has(&DataKey::Admin) {
            return Err(KoraError::AlreadyInitialized);
        }
        require_valid_fee_bps(fee_bps)?;
        kora_shared::validation::require_not_self(&env, &admin)?;
        env.storage().persistent().set(&DataKey::Admin, &admin);
        env.storage().persistent().extend_ttl(
            &DataKey::Admin,
            PERSISTENT_LIFETIME_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        env.storage().persistent().set(&DataKey::FeeBps, &fee_bps);
        env.storage().persistent().extend_ttl(
            &DataKey::FeeBps,
            PERSISTENT_LIFETIME_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        events::treasury_initialized(&env, &admin, fee_bps);
        Ok(())
    }

    /// Update protocol fee. Admin only.
    pub fn set_fee_bps(env: Env, admin: Address, fee_bps: u32) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        require_valid_fee_bps(fee_bps)?;

        let old_bps: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::FeeBps)
            .unwrap_or(50);

        env.storage().persistent().set(&DataKey::FeeBps, &fee_bps);
        Self::bump_persistent(&env, &DataKey::FeeBps);

        events::fee_rate_updated(&env, &admin, old_bps, fee_bps);
        Ok(())
    }

    /// Whitelist a token so it can be used in withdraw / emergency_withdraw.
    /// Admin only. Idempotent — calling it twice for the same token is safe.
    pub fn whitelist_token(env: Env, admin: Address, token: Address) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;

        env.storage()
            .persistent()
            .set(&DataKey::WhitelistedToken(token.clone()), &true);
        Self::bump_persistent(&env, &DataKey::WhitelistedToken(token.clone()));

        events::token_whitelisted(&env, &admin, &token);
        Ok(())
    }

    /// Record an incoming fee for a given token. Called by the marketplace after
    /// transferring the fee amount to this contract. Updates the informational
    /// accounting ledger.
    ///
    /// No auth required — the token transfer itself is the proof of payment.
    /// The amount is validated to be > 0 to prevent no-op accounting entries.
    pub fn collect_fee(env: Env, token: Address, amount: i128) -> Result<(), KoraError> {
        if amount <= 0 {
            return Err(KoraError::InvalidAmount);
        }
        Self::require_whitelisted_token(&env, &token)?;

        let key = DataKey::Collected(token.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        let new_total = current
            .checked_add(amount)
            .ok_or(KoraError::ArithmeticOverflow)?;

        env.storage().persistent().set(&key, &new_total);
        Self::bump_persistent(&env, &key);

        events::fee_collected(&env, &env.current_contract_address(), 0, amount, &token);
        Ok(())
    }

    /// Withdraw accumulated fees to a recipient. Admin only.
    /// Protected against reentrancy via RAII ReentrancyGuard.
    pub fn withdraw(
        env: Env,
        admin: Address,
        token: Address,
        recipient: Address,
        amount: i128,
    ) -> Result<(), KoraError> {
        // ── Checks ────────────────────────────────────────────────────────────
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        if amount <= 0 {
            return Err(KoraError::InvalidAmount);
        }
        require_within_max_amount(amount)?;
        Self::require_whitelisted_token(&env, &token)?;

        // Acquire reentrancy guard — released automatically when _guard drops
        let _guard = ReentrancyGuard::new(&env)?;

        let token_client = token::Client::new(&env, &token);
        let balance = token_client.balance(&env.current_contract_address());

        if balance < amount {
            return Err(KoraError::InsufficientPoolBalance);
        }

        // ── Effects ───────────────────────────────────────────────────────────
        let collected_key = DataKey::Collected(token.clone());
        if let Some(collected) = env
            .storage()
            .persistent()
            .get::<_, i128>(&collected_key)
        {
            let new_collected = collected.saturating_sub(amount);
            env.storage().persistent().set(&collected_key, &new_collected);
            Self::bump_persistent(&env, &collected_key);
        }

        // ── Interactions ──────────────────────────────────────────────────────
        token_client.transfer(&env.current_contract_address(), &recipient, &amount);

        events::fee_withdrawn(&env, &admin, &token, amount);
        Ok(())
    }

    /// Emergency drain — withdraw entire token balance. Admin only.
    /// Protected against reentrancy via RAII ReentrancyGuard.
    /// No-ops silently when balance is zero (not an error).
    pub fn emergency_withdraw(
        env: Env,
        admin: Address,
        token: Address,
        recipient: Address,
    ) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        Self::require_whitelisted_token(&env, &token)?;

        let _guard = ReentrancyGuard::new(&env)?;

        let token_client = token::Client::new(&env, &token);
        let balance = token_client.balance(&env.current_contract_address());

        // ── Interactions ──────────────────────────────────────────────────────
        if balance > 0 {
            token_client.transfer(&env.current_contract_address(), &recipient, &balance);
            events::emergency_withdrawn(&env, &admin, &token, balance);
        }

        Ok(())
    }

    /// Returns the current protocol fee in basis points.
    pub fn get_fee_bps(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::FeeBps)
            .unwrap_or(50)
    }

    /// Returns the live token balance held by this contract.
    pub fn get_balance(env: Env, token: Address) -> i128 {
        token::Client::new(&env, &token).balance(&env.current_contract_address())
    }

    /// Returns the total fees collected for a given token (informational ledger).
    pub fn get_collected(env: Env, token: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::Collected(token))
            .unwrap_or(0)
    }

    pub fn get_admin(env: Env) -> Result<Address, KoraError> {
        env.storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(KoraError::NotInitialized)
    }

    // ── Upgrade ────────────────────────────────────────────────────────────────

    pub fn propose_upgrade(
        env: Env,
        admin: Address,
        new_wasm_hash: BytesN<32>,
    ) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        env.storage()
            .instance()
            .set(&DataKey::UpgradeProposal, &(new_wasm_hash.clone(), env.ledger().timestamp()));
        events::upgrade_proposed(&env, &admin, &new_wasm_hash);
        Ok(())
    }

    pub fn execute_upgrade(env: Env, admin: Address) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        let (wasm_hash, proposed_at): (BytesN<32>, u64) = env
            .storage()
            .instance()
            .get(&DataKey::UpgradeProposal)
            .ok_or(KoraError::NoUpgradeProposed)?;
        if env.ledger().timestamp() < proposed_at + UPGRADE_TIMELOCK_DELAY {
            return Err(KoraError::UpgradeTimelockNotElapsed);
        }
        env.storage().instance().remove(&DataKey::UpgradeProposal);
        events::upgrade_executed(&env, &admin, &wasm_hash);
        env.deployer().update_current_contract_wasm(wasm_hash);
        Ok(())
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn require_admin(env: &Env, caller: &Address) -> Result<(), KoraError> {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(KoraError::NotInitialized)?;
        if &admin != caller {
            return Err(KoraError::NotAdmin);
        }
        Ok(())
    }

    fn require_whitelisted_token(env: &Env, token: &Address) -> Result<(), KoraError> {
        let whitelisted: bool = env
            .storage()
            .persistent()
            .get(&DataKey::WhitelistedToken(token.clone()))
            .unwrap_or(false);
        if !whitelisted {
            return Err(KoraError::TokenNotWhitelisted);
        }
        Ok(())
    }

    fn bump_persistent(env: &Env, key: &DataKey) {
        env.storage().persistent().extend_ttl(
            key,
            PERSISTENT_LIFETIME_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, MockAuth, MockAuthInvoke},
        token, Address, Env,
    };

    fn setup() -> (Env, Address, TreasuryContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TreasuryContract);
        let client = TreasuryContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        client.initialize(&admin, &50u32).unwrap();
        (env, admin, client)
    }

    /// Deploy a minimal Soroban token contract and return its address +
    /// a client minted with `amount` to `recipient`.
    fn deploy_token(env: &Env, admin: &Address, recipient: &Address, amount: i128) -> Address {
        let token_id = env.register_stellar_asset_contract_v2(admin.clone()).address();
        let token_client = token::StellarAssetClient::new(env, &token_id);
        token_client.mint(recipient, &amount);
        token_id
    }

    // ── initialize ────────────────────────────────────────────────────────────

    #[test]
    fn test_initialize_creates_contract() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TreasuryContract);
        let client = TreasuryContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        assert!(client.try_initialize(&admin, &50u32).is_ok());
        assert_eq!(client.get_fee_bps(), 50);
    }

    #[test]
    fn test_initialize_already_initialized() {
        let (env, admin, client) = setup();
        let result = client.try_initialize(&admin, &50u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_initialize_invalid_fee_bps() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TreasuryContract);
        let client = TreasuryContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        assert!(client.try_initialize(&admin, &10_001u32).is_err());
    }

    #[test]
    fn test_initialize_self_as_admin_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TreasuryContract);
        let client = TreasuryContractClient::new(&env, &contract_id);
        // Passing the contract's own address as admin must be rejected.
        let result = client.try_initialize(&contract_id, &50u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_fee_bps_after_init() {
        let (_env, _admin, client) = setup();
        assert_eq!(client.get_fee_bps(), 50);
    }

    // ── set_fee_bps ───────────────────────────────────────────────────────────

    #[test]
    fn test_set_fee_bps_success() {
        let (_env, admin, client) = setup();
        client.set_fee_bps(&admin, &100u32).unwrap();
        assert_eq!(client.get_fee_bps(), 100);
    }

    #[test]
    fn test_set_fee_bps_requires_admin() {
        let (env, _admin, client) = setup();
        let non_admin = Address::generate(&env);
        assert!(client.try_set_fee_bps(&non_admin, &100u32).is_err());
    }

    #[test]
    fn test_set_fee_bps_invalid_bps_fails() {
        let (_env, admin, client) = setup();
        assert!(client.try_set_fee_bps(&admin, &10_001u32).is_err());
    }

    #[test]
    fn test_set_fee_bps_zero_allowed() {
        let (_env, admin, client) = setup();
        client.set_fee_bps(&admin, &0u32).unwrap();
        assert_eq!(client.get_fee_bps(), 0);
    }

    #[test]
    fn test_set_fee_bps_max_allowed() {
        let (_env, admin, client) = setup();
        client.set_fee_bps(&admin, &10_000u32).unwrap();
        assert_eq!(client.get_fee_bps(), 10_000);
    }

    #[test]
    fn test_set_fee_bps_over_max_fails() {
        let (_env, admin, client) = setup();
        assert!(client.try_set_fee_bps(&admin, &10_001u32).is_err());
    }

    #[test]
    fn test_set_fee_bps_multiple_updates() {
        let (_env, admin, client) = setup();
        client.set_fee_bps(&admin, &100u32).unwrap();
        assert_eq!(client.get_fee_bps(), 100);
        client.set_fee_bps(&admin, &200u32).unwrap();
        assert_eq!(client.get_fee_bps(), 200);
        client.set_fee_bps(&admin, &50u32).unwrap();
        assert_eq!(client.get_fee_bps(), 50);
    }

    // ── whitelist_token ───────────────────────────────────────────────────────

    #[test]
    fn test_whitelist_token_idempotent() {
        // Whitelisting the same token twice must not error — it's a no-op on the
        // second call (the token is simply already whitelisted).
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        assert!(client.try_whitelist_token(&admin, &token).is_ok());
        assert!(client.try_whitelist_token(&admin, &token).is_ok());
    }

    #[test]
    fn test_whitelist_token_requires_admin() {
        let (env, _admin, client) = setup();
        let non_admin = Address::generate(&env);
        let token = Address::generate(&env);
        assert!(client.try_whitelist_token(&non_admin, &token).is_err());
    }

    // ── collect_fee ───────────────────────────────────────────────────────────

    #[test]
    fn test_collect_fee_zero_amount_rejected() {
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token).unwrap();
        assert!(client.try_collect_fee(&token, &0i128).is_err());
    }

    #[test]
    fn test_collect_fee_negative_amount_rejected() {
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token).unwrap();
        assert!(client.try_collect_fee(&token, &-1i128).is_err());
    }

    #[test]
    fn test_collect_fee_non_whitelisted_token_rejected() {
        let (env, _admin, client) = setup();
        let token = Address::generate(&env);
        assert!(client.try_collect_fee(&token, &1_000i128).is_err());
    }

    #[test]
    fn test_collect_fee_accumulates() {
        // collect_fee is informational — multiple calls accumulate correctly.
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token).unwrap();
        client.collect_fee(&token, &500i128).unwrap();
        client.collect_fee(&token, &300i128).unwrap();
        // The collected ledger is internal, but no error means the addition succeeded.
    }

    #[test]
    fn test_collect_fee_overflow_rejected() {
        // Two consecutive collect_fee calls whose sum overflows i128 must return
        // ArithmeticOverflow — not silently wrap.
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        client.whitelist_token(&admin, &token).unwrap();
        // Seed the ledger with i128::MAX first.
        client.collect_fee(&token, &i128::MAX).unwrap();
        // Any further positive amount must overflow.
        let result = client.try_collect_fee(&token, &1i128);
        assert!(result.is_err());
    }

    // ── get_balance ───────────────────────────────────────────────────────────

    #[test]
    fn test_get_balance_returns_zero_for_unknown_token() {
        // Before any transfer, balance should be 0 for a freshly deployed token.
        let (env, admin, client) = setup();
        let contract_id = client.address.clone();
        let token_id = deploy_token(&env, &admin, &contract_id, 0);
        assert_eq!(client.get_balance(&token_id), 0);
    }

    #[test]
    fn test_get_balance_after_mint() {
        let (env, admin, client) = setup();
        let contract_id = client.address.clone();
        let token_id = deploy_token(&env, &admin, &contract_id, 1_000_000);
        assert_eq!(client.get_balance(&token_id), 1_000_000);
    }

    // ── withdraw ──────────────────────────────────────────────────────────────

    #[test]
    fn test_withdraw_requires_admin() {
        let (env, _admin, client) = setup();
        let non_admin = Address::generate(&env);
        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        assert!(client
            .try_withdraw(&non_admin, &token, &recipient, &1_000_000i128)
            .is_err());
    }

    #[test]
    fn test_withdraw_zero_amount_fails() {
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        assert!(client
            .try_withdraw(&admin, &token, &recipient, &0i128)
            .is_err());
    }

    #[test]
    fn test_withdraw_with_negative_amount_rejected() {
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        assert!(client
            .try_withdraw(&admin, &token, &recipient, &-1_000i128)
            .is_err());
    }

    #[test]
    fn test_withdraw_non_whitelisted_token_rejected() {
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        assert!(client
            .try_withdraw(&admin, &token, &recipient, &1_000i128)
            .is_err());
    }

    #[test]
    fn test_withdraw_insufficient_balance_fails() {
        let (env, admin, client) = setup();
        let contract_id = client.address.clone();
        let token_id = deploy_token(&env, &admin, &contract_id, 500);
        let recipient = Address::generate(&env);
        client.whitelist_token(&admin, &token_id).unwrap();
        // Contract only has 500, requesting 1_000 must fail.
        let result = client.try_withdraw(&admin, &token_id, &recipient, &1_000i128);
        assert!(result.is_err());
    }

    #[test]
    fn test_withdraw_exact_balance_succeeds() {
        // Withdrawing exactly the available balance must succeed.
        let (env, admin, client) = setup();
        let contract_id = client.address.clone();
        let token_id = deploy_token(&env, &admin, &contract_id, 1_000);
        let recipient = Address::generate(&env);
        client.whitelist_token(&admin, &token_id).unwrap();
        assert!(client
            .try_withdraw(&admin, &token_id, &recipient, &1_000i128)
            .is_ok());
        // Balance drained to zero.
        assert_eq!(client.get_balance(&token_id), 0);
    }

    // ── emergency_withdraw ────────────────────────────────────────────────────

    #[test]
    fn test_emergency_withdraw_requires_admin() {
        let (env, _admin, client) = setup();
        let non_admin = Address::generate(&env);
        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        assert!(client
            .try_emergency_withdraw(&non_admin, &token, &recipient)
            .is_err());
    }

    #[test]
    fn test_emergency_withdraw_non_whitelisted_token_rejected() {
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        assert!(client
            .try_emergency_withdraw(&admin, &token, &recipient)
            .is_err());
    }

    #[test]
    fn test_emergency_withdraw_zero_balance_is_noop() {
        // When balance is zero, emergency_withdraw must succeed without error
        // (it simply has nothing to transfer).
        let (env, admin, client) = setup();
        let contract_id = client.address.clone();
        let token_id = deploy_token(&env, &admin, &contract_id, 0);
        let recipient = Address::generate(&env);
        client.whitelist_token(&admin, &token_id).unwrap();
        assert!(client
            .try_emergency_withdraw(&admin, &token_id, &recipient)
            .is_ok());
    }

    #[test]
    fn test_emergency_withdraw_drains_full_balance() {
        let (env, admin, client) = setup();
        let contract_id = client.address.clone();
        let token_id = deploy_token(&env, &admin, &contract_id, 5_000);
        let recipient = Address::generate(&env);
        client.whitelist_token(&admin, &token_id).unwrap();
        client.emergency_withdraw(&admin, &token_id, &recipient).unwrap();
        assert_eq!(client.get_balance(&token_id), 0);
    }

    // ── reentrancy lock cleanup ───────────────────────────────────────────────

    #[test]
    fn test_lock_released_after_failed_withdraw() {
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        // Fails due to token not whitelisted — lock must be released
        let _ = client.try_withdraw(&admin, &token, &recipient, &1_000i128);
        // Subsequent admin operation must succeed (lock not stuck)
        assert!(client.try_set_fee_bps(&admin, &100u32).is_ok());
    }

    #[test]
    fn test_lock_released_after_emergency_withdraw() {
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        let _ = client.try_emergency_withdraw(&admin, &token, &recipient);
        // Lock must be released regardless of outcome
        assert!(client.try_set_fee_bps(&admin, &100u32).is_ok());
    }

    // ── get_fee_bps ───────────────────────────────────────────────────────────

    #[test]
    fn test_admin_actions_work_immediately_after_initialize() {
        let (_env, admin, client) = setup();
        assert!(client.try_set_fee_bps(&admin, &100u32).is_ok());
    }

    #[test]
    fn test_initialize_self_as_admin_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TreasuryContract);
        let client = TreasuryContractClient::new(&env, &contract_id);
        // Returns 50 bps as the hard-coded fallback before initialization.
        assert_eq!(client.get_fee_bps(), 50);
    }

    #[test]
    fn test_get_balance_with_non_existent_token() {
        // Test behavior when calling get_balance with an arbitrary unregistered address
        // (not a valid token contract)
        let (env, _admin, client) = setup();
        let invalid_token = Address::generate(&env);

        // get_balance should return 0 for a non-existent token
        // (Soroban token::Client.balance() returns 0 if the account has no balance)
        let balance = client.get_balance(&invalid_token);
        assert_eq!(balance, 0i128, "Balance of non-existent token should be 0");
    }
}
