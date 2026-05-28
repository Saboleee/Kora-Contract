#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, Address, Env,
};
use kora_shared::{errors::KoraError, events};

// ── Storage Keys ─────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Admin,
    Paused,
    Role(Address),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Role {
    Admin,
    Operator,
    Verifier,
    None,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct AccessControlContract;

#[contractimpl]
impl AccessControlContract {
    pub fn initialize(env: Env, admin: Address) -> Result<(), KoraError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(KoraError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage().persistent().set(&DataKey::Role(admin.clone()), &Role::Admin);
        Ok(())
    }

    /// Pause the entire protocol. Admin only.
    pub fn pause(env: Env, admin: Address) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        env.storage().instance().set(&DataKey::Paused, &true);
        events::protocol_paused(&env, &admin);
        Ok(())
    }

    /// Unpause the protocol. Admin only.
    pub fn unpause(env: Env, admin: Address) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        env.storage().instance().set(&DataKey::Paused, &false);
        events::protocol_unpaused(&env, &admin);
        Ok(())
    }

    /// Assign a role to an address. Admin only.
    pub fn grant_role(
        env: Env,
        admin: Address,
        target: Address,
        role: Role,
    ) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        env.storage().persistent().set(&DataKey::Role(target), &role);
        Ok(())
    }

    /// Revoke a role from an address. Admin only.
    pub fn revoke_role(env: Env, admin: Address, target: Address) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        env.storage().persistent().set(&DataKey::Role(target), &Role::None);
        Ok(())
    }

    /// Transfer admin to a new address. Current admin must sign.
    pub fn transfer_admin(env: Env, current_admin: Address, new_admin: Address) -> Result<(), KoraError> {
        current_admin.require_auth();
        Self::require_admin(&env, &current_admin)?;
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        env.storage().persistent().set(&DataKey::Role(new_admin.clone()), &Role::Admin);
        env.storage().persistent().set(&DataKey::Role(current_admin), &Role::None);
        Ok(())
    }

    // ── Views ─────────────────────────────────────────────────────────────────

    pub fn is_paused(env: Env) -> bool {
        env.storage().instance().get(&DataKey::Paused).unwrap_or(false)
    }

    pub fn get_role(env: Env, address: Address) -> Role {
        env.storage()
            .persistent()
            .get(&DataKey::Role(address))
            .unwrap_or(Role::None)
    }

    pub fn get_admin(env: Env) -> Result<Address, KoraError> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(KoraError::NotInitialized)
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

    fn setup() -> (Env, Address, AccessControlContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, AccessControlContract);
        let client = AccessControlContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        client.initialize(&admin);
        (env, admin, client)
    }

    #[test]
    fn test_pause_unpause() {
        let (_, admin, client) = setup();
        assert!(!client.is_paused());
        client.pause(&admin);
        assert!(client.is_paused());
        client.unpause(&admin);
        assert!(!client.is_paused());
    }

    #[test]
    fn test_grant_revoke_role() {
        let (env, admin, client) = setup();
        let operator = Address::generate(&env);

        client.grant_role(&admin, &operator, &Role::Operator);
        assert_eq!(client.get_role(&operator), Role::Operator);

        client.revoke_role(&admin, &operator);
        assert_eq!(client.get_role(&operator), Role::None);
    }

    #[test]
    fn test_transfer_admin() {
        let (env, admin, client) = setup();
        let new_admin = Address::generate(&env);

        client.transfer_admin(&admin, &new_admin);
        assert_eq!(client.get_admin(), new_admin);
        assert_eq!(client.get_role(&new_admin), Role::Admin);
        assert_eq!(client.get_role(&admin), Role::None);
    }

    #[test]
    fn test_non_admin_cannot_pause() {
        let (env, _admin, client) = setup();
        let stranger = Address::generate(&env);
        let result = client.try_pause(&stranger);
        assert!(result.is_err());
    }

    #[test]
    fn test_non_admin_cannot_grant_role() {
        let (env, _admin, client) = setup();
        let stranger = Address::generate(&env);
        let target = Address::generate(&env);
        let result = client.try_grant_role(&stranger, &target, &Role::Verifier);
        assert!(result.is_err());
    }

    #[test]
    fn test_non_admin_cannot_transfer_admin() {
        let (env, _admin, client) = setup();
        let stranger = Address::generate(&env);
        let new_admin = Address::generate(&env);
        let result = client.try_transfer_admin(&stranger, &new_admin);
        assert!(result.is_err());
    }

    #[test]
    fn test_multiple_role_assignments() {
        let (env, admin, client) = setup();
        let verifier1 = Address::generate(&env);
        let verifier2 = Address::generate(&env);
        let operator = Address::generate(&env);

        client.grant_role(&admin, &verifier1, &Role::Verifier);
        client.grant_role(&admin, &verifier2, &Role::Verifier);
        client.grant_role(&admin, &operator, &Role::Operator);

        assert_eq!(client.get_role(&verifier1), Role::Verifier);
        assert_eq!(client.get_role(&verifier2), Role::Verifier);
        assert_eq!(client.get_role(&operator), Role::Operator);
    }

    #[test]
    fn test_role_override() {
        let (env, admin, client) = setup();
        let user = Address::generate(&env);

        client.grant_role(&admin, &user, &Role::Operator);
        assert_eq!(client.get_role(&user), Role::Operator);

        // Override with different role
        client.grant_role(&admin, &user, &Role::Verifier);
        assert_eq!(client.get_role(&user), Role::Verifier);
    }
}
