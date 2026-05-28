#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, token, Address, Env,
};
use kora_shared::{errors::KoraError, events, validation::require_valid_fee_bps};

// ── Storage Keys ─────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Admin,
    FeeBps,
    Collected(Address), // accumulated fees per token
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct TreasuryContract;

#[contractimpl]
impl TreasuryContract {
    pub fn initialize(env: Env, admin: Address, fee_bps: u32) -> Result<(), KoraError> {
        if env.storage().instance().has(&DataKey::Admin) {
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
        env.storage().instance().set(&DataKey::FeeBps, &fee_bps);
        Ok(())
    }

    /// Withdraw accumulated fees to a recipient. Admin only.
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

        let token_client = token::Client::new(&env, &token);
        let balance = token_client.balance(&env.current_contract_address());
        if balance < amount {
            return Err(KoraError::InsufficientPoolBalance);
        }

        token_client.transfer(&env.current_contract_address(), &recipient, &amount);
        events::fee_withdrawn(&env, &token, amount);
        Ok(())
    }

    /// Emergency drain — withdraw entire token balance. Admin only.
    pub fn emergency_withdraw(
        env: Env,
        admin: Address,
        token: Address,
        recipient: Address,
    ) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;

        let token_client = token::Client::new(&env, &token);
        let balance = token_client.balance(&env.current_contract_address());
        if balance > 0 {
            token_client.transfer(&env.current_contract_address(), &recipient, &balance);
        }
        Ok(())
    }

    pub fn get_fee_bps(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::FeeBps).unwrap_or(50)
    }

    pub fn get_balance(env: Env, token: Address) -> i128 {
        token::Client::new(&env, &token).balance(&env.current_contract_address())
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

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
        
        client.initialize(&admin, &50u32);
        assert_eq!(client.get_fee_bps(), 50);
    }

    #[test]
    fn test_initialize_already_initialized_fails() {
        let (env, admin, client) = setup();
        let result = client.try_initialize(&admin, &50u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_initialize_invalid_fee_bps_fails() {
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
        let (env, admin, client) = setup();
        assert_eq!(client.get_fee_bps(), 50);
        
        client.set_fee_bps(&admin, &100u32);
        assert_eq!(client.get_fee_bps(), 100);
    }

    #[test]
    fn test_set_fee_bps_requires_admin() {
        let (env, admin, client) = setup();
        let non_admin = Address::generate(&env);
        
        let result = client.try_set_fee_bps(&non_admin, &100u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_fee_bps_invalid_bps_fails() {
        let (env, admin, client) = setup();
        
        let result = client.try_set_fee_bps(&admin, &10_001u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_fee_bps_zero_succeeds() {
        let (env, admin, client) = setup();
        
        client.set_fee_bps(&admin, &0u32);
        assert_eq!(client.get_fee_bps(), 0);
    }

    #[test]
    fn test_set_fee_bps_max_succeeds() {
        let (env, admin, client) = setup();
        
        client.set_fee_bps(&admin, &10_000u32);
        assert_eq!(client.get_fee_bps(), 10_000);
    }

    #[test]
    fn test_withdraw_requires_admin() {
        let (env, admin, client) = setup();
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
        
        let result = client.try_withdraw(&admin, &token, &recipient, &-1_000_000i128);
        assert!(result.is_err());
    }

    #[test]
    fn test_emergency_withdraw_requires_admin() {
        let (env, admin, client) = setup();
        let non_admin = Address::generate(&env);
        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        
        let result = client.try_emergency_withdraw(&non_admin, &token, &recipient);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_fee_bps_default() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TreasuryContract);
        let client = TreasuryContractClient::new(&env, &contract_id);
        
        assert_eq!(client.get_fee_bps(), 50);
    }

    #[test]
    fn test_get_balance_zero_initially() {
        let (env, admin, client) = setup();
        let token = Address::generate(&env);
        
        let balance = client.get_balance(&token);
        assert_eq!(balance, 0);
    }

    #[test]
    fn test_invalid_fee_bps_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TreasuryContract);
        let client = TreasuryContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let result = client.try_initialize(&admin, &10_001u32);
        assert!(result.is_err());
    }
}
