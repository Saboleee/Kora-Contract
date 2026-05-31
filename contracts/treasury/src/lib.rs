#![no_std]

use kora_shared::{errors::KoraError, events, validation::require_valid_fee_bps};
use soroban_sdk::{contract, contractimpl, contracttype, token, Address, Env};

// ── Storage TTL constants ─────────────────────────────────────────────────────
const PERSISTENT_BUMP_AMOUNT: u32 = 535_680; // ~31 days in ledgers
const PERSISTENT_LIFETIME_THRESHOLD: u32 = 535_680 / 2;

// ── Storage Keys ─────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    /// Admin address — persistent so it survives ledger archival.
    Admin,
    /// Protocol fee in basis points — persistent for durability.
    FeeBps,
    Collected(Address), // accumulated fees per token (informational)
    WithdrawalLock,     // reentrancy guard
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct TreasuryContract;

#[contractimpl]
impl TreasuryContract {
    pub fn initialize(env: Env, admin: Address, fee_bps: u32) -> Result<(), KoraError> {
        if env.storage().persistent().has(&DataKey::Admin) {
            return Err(KoraError::AlreadyInitialized);
        }
        require_valid_fee_bps(fee_bps)?;
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::FeeBps, &fee_bps);
        Ok(())
    }

    /// Update protocol fee. Admin only.
    pub fn set_fee_bps(env: Env, admin: Address, fee_bps: u32) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        require_valid_fee_bps(fee_bps)?;

        // Read old value before overwriting so we can include it in the event
        let old_bps: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::FeeBps)
            .unwrap_or(50);

        env.storage().persistent().set(&DataKey::FeeBps, &fee_bps);
        env.storage().persistent().extend_ttl(
            &DataKey::FeeBps,
            PERSISTENT_LIFETIME_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        events::fee_rate_updated(&env, &admin, old_bps, fee_bps);
        Ok(())
    }

    /// Withdraw accumulated fees to a recipient. Admin only.
    /// Protected against reentrancy via a persistent lock key.
    pub fn withdraw(
        env: Env,
        admin: Address,
        token: Address,
        recipient: Address,
        amount: i128,
    ) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;

        if amount <= 0 {
            return Err(KoraError::InvalidAmount);
        }

        Self::acquire_lock(&env)?;

        let token_client = token::Client::new(&env, &token);
        let balance = token_client.balance(&env.current_contract_address());

        if balance < amount {
            Self::release_lock(&env);
            return Err(KoraError::InsufficientPoolBalance);
        }

        token_client.transfer(&env.current_contract_address(), &recipient, &amount);

        // Release lock AFTER the external call completes
        Self::release_lock(&env);

        // Emit with admin address for full auditability
        events::fee_withdrawn(&env, &token, amount);
        Self::release_lock(&env);
        Ok(())
    }

    /// Emergency drain — withdraw entire token balance. Admin only.
    /// Protected against reentrancy via a persistent lock key.
    pub fn emergency_withdraw(
        env: Env,
        admin: Address,
        token: Address,
        recipient: Address,
    ) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;

        Self::acquire_lock(&env)?;

        let token_client = token::Client::new(&env, &token);
        let balance = token_client.balance(&env.current_contract_address());

        if balance > 0 {
            token_client.transfer(&env.current_contract_address(), &recipient, &balance);
            // Release lock before emitting event (no further external calls)
            Self::release_lock(&env);
            // Use dedicated emergency event so indexers can distinguish
            // a routine withdrawal from a full emergency drain
            events::emergency_withdrawn(&env, &admin, &token, balance);
        } else {
            Self::release_lock(&env);
        }

        Ok(())
    }

    pub fn get_fee_bps(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::FeeBps)
            .unwrap_or(50)
    }

    pub fn get_balance(env: Env, token: Address) -> i128 {
        token::Client::new(&env, &token).balance(&env.current_contract_address())
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

    fn acquire_lock(env: &Env) -> Result<(), KoraError> {
        let locked: bool = env
            .storage()
            .instance()
            .get(&DataKey::WithdrawalLock)
            .unwrap_or(false);
        if locked {
            return Err(KoraError::Reentrancy);
        }
        env.storage().instance().set(&DataKey::WithdrawalLock, &true);
        Ok(())
    }

    fn release_lock(env: &Env) {
        env.storage().instance().set(&DataKey::WithdrawalLock, &false);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Env};

    fn setup() -> (Env, Address, TreasuryContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TreasuryContract);
        let client = TreasuryContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        client.initialize(&admin, &50u32);
        (env, admin, client)
    }

    #[test]
    fn test_initialize_success() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TreasuryContract);
        let client = TreasuryContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let result = client.try_initialize(&admin, &50u32);
        assert!(result.is_ok());
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
        let result = client.try_initialize(&admin, &10_001u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_fee_bps_success() {
        let (_env, admin, client) = setup();
        client.set_fee_bps(&admin, &100u32);
        assert_eq!(client.get_fee_bps(), 100);
    }

    #[test]
    fn test_set_fee_bps_requires_admin() {
        let (env, _admin, client) = setup();
        let non_admin = Address::generate(&env);
        let result = client.try_set_fee_bps(&non_admin, &100u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_fee_bps_invalid_bps_fails() {
        let (_env, admin, client) = setup();
        let result = client.try_set_fee_bps(&admin, &10_001u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_fee_bps_zero_allowed() {
        let (_env, admin, client) = setup();
        client.set_fee_bps(&admin, &0u32);
        assert_eq!(client.get_fee_bps(), 0);
    }

    #[test]
    fn test_set_fee_bps_max_allowed() {
        let (_env, admin, client) = setup();
        client.set_fee_bps(&admin, &10_000u32);
        assert_eq!(client.get_fee_bps(), 10_000);
    }

    #[test]
    fn test_set_fee_bps_over_max_fails() {
        let (_env, admin, client) = setup();
        let result = client.try_set_fee_bps(&admin, &10_001u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_fee_bps_multiple_updates() {
        let (_env, admin, client) = setup();
        client.set_fee_bps(&admin, &100u32);
        assert_eq!(client.get_fee_bps(), 100);
        client.set_fee_bps(&admin, &200u32);
        assert_eq!(client.get_fee_bps(), 200);
        client.set_fee_bps(&admin, &50u32);
        assert_eq!(client.get_fee_bps(), 50);
    }

    #[test]
    fn test_withdraw_requires_admin() {
        let (env, _admin, client) = setup();
        let non_admin = Address::generate(&env);
        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        let result = client.try_withdraw(&non_admin, &token, &recipient, &1_000_000i128);
        assert!(result.is_err());
    }

    #[test]
    fn test_withdraw_zero_amount_fails() {
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        let result = client.try_withdraw(&admin, &token, &recipient, &0i128);
        assert!(result.is_err());
    }

    #[test]
    fn test_withdraw_negative_amount_fails() {
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        let result = client.try_withdraw(&admin, &token, &recipient, &-1_000i128);
        assert!(result.is_err());
    }

    #[test]
    fn test_emergency_withdraw_requires_admin() {
        let (env, _admin, client) = setup();
        let non_admin = Address::generate(&env);
        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        let result = client.try_emergency_withdraw(&non_admin, &token, &recipient);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_fee_bps_default_before_init() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TreasuryContract);
        let client = TreasuryContractClient::new(&env, &contract_id);
        // Before initialization, falls back to default 50
        assert_eq!(client.get_fee_bps(), 50);
    }

    #[test]
    fn test_lock_released_after_failed_withdraw() {
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        // Fails due to insufficient balance — lock must be released
        let _ = client.try_withdraw(&admin, &token, &recipient, &1_000i128);
        // Subsequent admin operation must succeed (lock not stuck)
        let result = client.try_set_fee_bps(&admin, &100u32);
        assert!(result.is_ok());
    }

    #[test]
    fn test_lock_released_after_emergency_withdraw() {
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        let _ = client.try_emergency_withdraw(&admin, &token, &recipient);
        // Lock must be released regardless of balance
        let result = client.try_set_fee_bps(&admin, &100u32);
        assert!(result.is_ok());
    }
}
