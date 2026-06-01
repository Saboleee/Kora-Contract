#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env};
use kora_shared::{errors::KoraError, events};

// ── TTL constants (~30 days in ledgers at ~5s/ledger) ────────────────────────
/// Threshold below which a persistent entry's TTL is extended.
const PERSISTENT_TTL_THRESHOLD: u32 = 518_400;
/// Target TTL after extension (~30 days).
const PERSISTENT_TTL_BUMP: u32 = 518_400;
/// Instance storage TTL threshold (~7 days).
const INSTANCE_TTL_THRESHOLD: u32 = 120_960;
/// Instance storage TTL bump (~30 days).
const INSTANCE_TTL_BUMP: u32 = 518_400;

// ── Storage Keys ─────────────────────────────────────────────────────────────

/// Instance-storage keys for contract-level configuration.
/// These are tied to the contract instance lifetime and hold small,
/// frequently-read values (admin address, pause flag).
#[contracttype]
pub enum InstanceKey {
    /// The current admin address.
    Admin,
    /// Protocol-wide pause flag.
    Paused,
}

/// Persistent-storage keys for per-address role data.
/// Persistent entries have independently managed TTLs and must be
/// explicitly extended to avoid expiry.
#[contracttype]
pub enum PersistentKey {
    /// Role assigned to a specific address.
    Role(Address),
}

/// Role variants assignable to protocol participants.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Role {
    /// Full administrative privileges (pause, grant/revoke roles, transfer admin).
    Admin,
    /// Reserved for future keeper/operator automation.
    Operator,
    /// Trusted off-chain entity that can register SMEs and set debtor scores.
    Verifier,
    /// Sentinel value — address has no assigned role.
    None,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct AccessControlContract;

#[contractimpl]
impl AccessControlContract {
    /// One-time initialization. Sets the admin and initializes the paused flag to false.
    ///
    /// # Errors
    /// - `KoraError::AlreadyInitialized` if called more than once.
    pub fn initialize(env: Env, admin: Address) -> Result<(), KoraError> {
        if env.storage().instance().has(&InstanceKey::Admin) {
            return Err(KoraError::AlreadyInitialized);
        }
        env.storage().instance().set(&InstanceKey::Admin, &admin);
        env.storage().instance().set(&InstanceKey::Paused, &false);
        // Assign Admin role in persistent storage so get_role() reflects it
        env.storage()
            .persistent()
            .set(&PersistentKey::Role(admin.clone()), &Role::Admin);
        Self::bump_persistent(&env, &PersistentKey::Role(admin));
        Self::bump_instance(&env);
        Ok(())
    }

    // ── Pause / Unpause ───────────────────────────────────────────────────────

    /// Pause the entire protocol. Admin only.
    ///
    /// # Errors
    /// - `KoraError::NotAdmin` if caller is not the current admin.
    /// - `KoraError::AlreadyPaused` if the protocol is already paused.
    pub fn pause(env: Env, admin: Address) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        if Self::read_paused(&env) {
            return Err(KoraError::AlreadyPaused);
        }
        env.storage().instance().set(&InstanceKey::Paused, &true);
        Self::bump_instance(&env);
        events::protocol_paused(&env, &admin);
        Ok(())
    }

    /// Unpause the protocol. Admin only.
    ///
    /// # Errors
    /// - `KoraError::NotAdmin` if caller is not the current admin.
    /// - `KoraError::NotPaused` if the protocol is not currently paused.
    pub fn unpause(env: Env, admin: Address) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        if !Self::read_paused(&env) {
            return Err(KoraError::NotPaused);
        }
        env.storage().instance().set(&InstanceKey::Paused, &false);
        Self::bump_instance(&env);
        events::protocol_unpaused(&env, &admin);
        Ok(())
    }

    // ── Role management ───────────────────────────────────────────────────────

    /// Assign a role to an address. Admin only.
    ///
    /// Constraints:
    /// - Cannot grant `Role::Admin` — use `transfer_admin` instead.
    /// - Cannot grant `Role::None` — use `revoke_role` instead.
    /// - Cannot grant a role to the current admin address.
    ///
    /// # Errors
    /// - `KoraError::NotAdmin` if caller is not the current admin.
    /// - `KoraError::Unauthorized` if role is Admin, None, or target is the admin.
    pub fn grant_role(
        env: Env,
        admin: Address,
        target: Address,
        role: Role,
    ) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        if role == Role::Admin {
            return Err(KoraError::Unauthorized);
        }
        if role == Role::None {
            return Err(KoraError::Unauthorized);
        }
        // Prevent silently overwriting the admin's own role entry
        if target == admin {
            return Err(KoraError::Unauthorized);
        }
        env.storage()
            .persistent()
            .set(&PersistentKey::Role(target.clone()), &role);
        Self::bump_persistent(&env, &PersistentKey::Role(target.clone()));
        events::role_granted(&env, &admin, &target);
        Ok(())
    }

    /// Revoke a role from an address. Admin only.
    ///
    /// Uses `remove()` to reclaim storage rather than writing `Role::None`.
    ///
    /// # Errors
    /// - `KoraError::NotAdmin` if caller is not the current admin.
    /// - `KoraError::Unauthorized` if target is the current admin.
    /// - `KoraError::RoleNotAssigned` if target has no role.
    pub fn revoke_role(env: Env, admin: Address, target: Address) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        let current_role = env
            .storage()
            .persistent()
            .get::<_, Role>(&PersistentKey::Role(target.clone()))
            .unwrap_or(Role::None);
        if current_role == Role::Admin {
            return Err(KoraError::Unauthorized);
        }
        if current_role == Role::None {
            return Err(KoraError::RoleNotAssigned);
        }
        // remove() reclaims storage rent rather than leaving a Role::None tombstone
        env.storage()
            .persistent()
            .remove(&PersistentKey::Role(target.clone()));
        events::role_revoked(&env, &admin, &target);
        Ok(())
    }

    /// Transfer admin to a new address. Current admin must sign.
    ///
    /// - Cannot transfer to self.
    /// - Cannot transfer to an address that already holds a non-None role
    ///   (caller must revoke first to avoid silent overwrite).
    ///
    /// # Errors
    /// - `KoraError::NotAdmin` if caller is not the current admin.
    /// - `KoraError::InvalidAddress` if `new_admin == current_admin`.
    /// - `KoraError::Unauthorized` if `new_admin` already holds a role.
    pub fn transfer_admin(
        env: Env,
        current_admin: Address,
        new_admin: Address,
    ) -> Result<(), KoraError> {
        current_admin.require_auth();
        Self::require_admin(&env, &current_admin)?;
        if current_admin == new_admin {
            return Err(KoraError::InvalidAddress);
        }
        // Guard: new_admin must not already hold a role (Operator/Verifier)
        let existing = env
            .storage()
            .persistent()
            .get::<_, Role>(&PersistentKey::Role(new_admin.clone()))
            .unwrap_or(Role::None);
        if existing != Role::None && existing != Role::Admin {
            return Err(KoraError::Unauthorized);
        }
        // Update instance storage: point Admin key to new address
        env.storage()
            .instance()
            .set(&InstanceKey::Admin, &new_admin);
        // Assign Admin role to new admin in persistent storage
        env.storage()
            .persistent()
            .set(&PersistentKey::Role(new_admin.clone()), &Role::Admin);
        Self::bump_persistent(&env, &PersistentKey::Role(new_admin.clone()));
        // Remove old admin's role entry to reclaim storage
        env.storage()
            .persistent()
            .remove(&PersistentKey::Role(current_admin));
        Self::bump_instance(&env);
        events::admin_transferred(&env, &new_admin);
        Ok(())
    }

    // ── Views ─────────────────────────────────────────────────────────────────

    /// Returns true if the protocol is currently paused.
    pub fn is_paused(env: Env) -> bool {
        Self::read_paused(&env)
    }

    /// Returns the role assigned to `address`, or `Role::None` if unassigned.
    pub fn get_role(env: Env, address: Address) -> Role {
        env.storage()
            .persistent()
            .get(&PersistentKey::Role(address))
            .unwrap_or(Role::None)
    }

    /// Returns the current admin address.
    ///
    /// # Errors
    /// - `KoraError::NotInitialized` if the contract has not been initialized.
    pub fn get_admin(env: Env) -> Result<Address, KoraError> {
        env.storage()
            .instance()
            .get(&InstanceKey::Admin)
            .ok_or(KoraError::NotInitialized)
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Verify that `caller` is the current admin. Returns `KoraError::NotAdmin` otherwise.
    fn require_admin(env: &Env, caller: &Address) -> Result<(), KoraError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&InstanceKey::Admin)
            .ok_or(KoraError::NotInitialized)?;
        if &admin != caller {
            return Err(KoraError::NotAdmin);
        }
        Ok(())
    }

    /// Read the paused flag from instance storage, defaulting to false.
    fn read_paused(env: &Env) -> bool {
        env.storage()
            .instance()
            .get::<_, bool>(&InstanceKey::Paused)
            .unwrap_or(false)
    }

    /// Extend the TTL of a persistent storage entry if it is below the threshold.
    fn bump_persistent(env: &Env, key: &PersistentKey) {
        env.storage()
            .persistent()
            .extend_ttl(key, PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_BUMP);
    }

    /// Extend the TTL of the contract instance storage if it is below the threshold.
    /// This keeps Admin and Paused accessible for the full bump window.
    fn bump_instance(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use kora_shared::errors::KoraError;
    use soroban_sdk::{
        testutils::{Address as _, MockAuth, MockAuthInvoke},
        Address, Env, IntoVal,
    };

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Deploy and initialize with mock_all_auths for convenience.
    fn setup() -> (Env, Address, AccessControlContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, AccessControlContract);
        let client = AccessControlContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        client.initialize(&admin);
        (env, admin, client)
    }

    /// Deploy without initializing (for pre-init tests).
    fn deploy_uninit() -> (Env, AccessControlContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, AccessControlContract);
        let client = AccessControlContractClient::new(&env, &contract_id);
        (env, client)
    }

    // ── initialize ────────────────────────────────────────────────────────────

    #[test]
    fn test_initialize_success() {
        let (env, client) = deploy_uninit();
        let admin = Address::generate(&env);
        assert!(client.try_initialize(&admin).is_ok());
        assert_eq!(client.get_admin(), admin);
        assert_eq!(client.get_role(&admin), Role::Admin);
        assert!(!client.is_paused());
    }

    #[test]
    fn test_initialize_already_initialized_returns_correct_error() {
        let (_, admin, client) = setup();
        let result = client.try_initialize(&admin);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::AlreadyInitialized);
    }

    #[test]
    fn test_initialize_second_admin_ignored() {
        // A second initialize with a different admin must fail — original admin unchanged
        let (env, admin, client) = setup();
        let attacker = Address::generate(&env);
        let _ = client.try_initialize(&attacker);
        assert_eq!(client.get_admin(), admin);
    }

    // ── pause ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_pause_sets_paused_flag() {
        let (_, admin, client) = setup();
        assert!(!client.is_paused());
        client.pause(&admin);
        assert!(client.is_paused());
    }

    #[test]
    fn test_pause_requires_admin_auth() {
        let (env, admin, client) = setup();
        env.mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "pause",
                args: (&admin,).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        assert!(client.try_pause(&admin).is_ok());
    }

    #[test]
    fn test_pause_non_admin_returns_not_admin() {
        let (env, _, client) = setup();
        let stranger = Address::generate(&env);
        let result = client.try_pause(&stranger);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::NotAdmin);
    }

    #[test]
    fn test_pause_already_paused_returns_correct_error() {
        let (_, admin, client) = setup();
        client.pause(&admin);
        let result = client.try_pause(&admin);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::AlreadyPaused);
    }

    #[test]
    fn test_pause_state_unchanged_after_double_pause() {
        let (_, admin, client) = setup();
        client.pause(&admin);
        let _ = client.try_pause(&admin);
        assert!(client.is_paused());
    }

    // ── unpause ───────────────────────────────────────────────────────────────

    #[test]
    fn test_unpause_clears_paused_flag() {
        let (_, admin, client) = setup();
        client.pause(&admin);
        client.unpause(&admin);
        assert!(!client.is_paused());
    }

    #[test]
    fn test_unpause_requires_admin_auth() {
        let (env, admin, client) = setup();
        client.pause(&admin);
        env.mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "unpause",
                args: (&admin,).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        assert!(client.try_unpause(&admin).is_ok());
    }

    #[test]
    fn test_unpause_non_admin_returns_not_admin() {
        let (env, admin, client) = setup();
        client.pause(&admin);
        let stranger = Address::generate(&env);
        let result = client.try_unpause(&stranger);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::NotAdmin);
    }

    #[test]
    fn test_unpause_when_not_paused_returns_correct_error() {
        let (_, admin, client) = setup();
        let result = client.try_unpause(&admin);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::NotPaused);
    }

    #[test]
    fn test_unpause_state_unchanged_after_failed_unpause() {
        let (_, admin, client) = setup();
        let _ = client.try_unpause(&admin);
        assert!(!client.is_paused());
    }

    #[test]
    fn test_pause_unpause_cycle_multiple_times() {
        let (_, admin, client) = setup();
        for _ in 0..5 {
            client.pause(&admin);
            assert!(client.is_paused());
            client.unpause(&admin);
            assert!(!client.is_paused());
        }
    }

    // ── grant_role ────────────────────────────────────────────────────────────

    #[test]
    fn test_grant_role_operator_success() {
        let (env, admin, client) = setup();
        let operator = Address::generate(&env);
        client.grant_role(&admin, &operator, &Role::Operator);
        assert_eq!(client.get_role(&operator), Role::Operator);
    }

    #[test]
    fn test_grant_role_verifier_success() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        client.grant_role(&admin, &verifier, &Role::Verifier);
        assert_eq!(client.get_role(&verifier), Role::Verifier);
    }

    #[test]
    fn test_grant_role_requires_admin_auth() {
        let (env, admin, client) = setup();
        let target = Address::generate(&env);
        env.mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "grant_role",
                args: (&admin, &target, &Role::Verifier).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        assert!(client.try_grant_role(&admin, &target, &Role::Verifier).is_ok());
    }

    #[test]
    fn test_grant_role_non_admin_returns_not_admin() {
        let (env, _, client) = setup();
        let stranger = Address::generate(&env);
        let target = Address::generate(&env);
        let result = client.try_grant_role(&stranger, &target, &Role::Verifier);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::NotAdmin);
    }

    #[test]
    fn test_grant_role_admin_variant_returns_unauthorized() {
        let (env, admin, client) = setup();
        let target = Address::generate(&env);
        let result = client.try_grant_role(&admin, &target, &Role::Admin);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::Unauthorized);
    }

    #[test]
    fn test_grant_role_none_variant_returns_unauthorized() {
        let (env, admin, client) = setup();
        let target = Address::generate(&env);
        let result = client.try_grant_role(&admin, &target, &Role::None);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::Unauthorized);
    }

    #[test]
    fn test_grant_role_to_self_returns_unauthorized() {
        let (_, admin, client) = setup();
        let result = client.try_grant_role(&admin, &admin, &Role::Operator);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::Unauthorized);
    }

    #[test]
    fn test_grant_role_state_unchanged_after_failed_grant() {
        let (env, admin, client) = setup();
        let target = Address::generate(&env);
        let _ = client.try_grant_role(&admin, &target, &Role::Admin);
        assert_eq!(client.get_role(&target), Role::None);
    }

    #[test]
    fn test_grant_role_override_operator_to_verifier() {
        let (env, admin, client) = setup();
        let user = Address::generate(&env);
        client.grant_role(&admin, &user, &Role::Operator);
        client.grant_role(&admin, &user, &Role::Verifier);
        assert_eq!(client.get_role(&user), Role::Verifier);
    }

    #[test]
    fn test_grant_role_override_verifier_to_operator() {
        let (env, admin, client) = setup();
        let user = Address::generate(&env);
        client.grant_role(&admin, &user, &Role::Verifier);
        client.grant_role(&admin, &user, &Role::Operator);
        assert_eq!(client.get_role(&user), Role::Operator);
    }

    #[test]
    fn test_grant_role_same_role_twice_idempotent() {
        let (env, admin, client) = setup();
        let user = Address::generate(&env);
        client.grant_role(&admin, &user, &Role::Verifier);
        client.grant_role(&admin, &user, &Role::Verifier);
        assert_eq!(client.get_role(&user), Role::Verifier);
    }

    #[test]
    fn test_grant_role_multiple_users_independent() {
        let (env, admin, client) = setup();
        let v1 = Address::generate(&env);
        let v2 = Address::generate(&env);
        let op = Address::generate(&env);
        client.grant_role(&admin, &v1, &Role::Verifier);
        client.grant_role(&admin, &v2, &Role::Verifier);
        client.grant_role(&admin, &op, &Role::Operator);
        assert_eq!(client.get_role(&v1), Role::Verifier);
        assert_eq!(client.get_role(&v2), Role::Verifier);
        assert_eq!(client.get_role(&op), Role::Operator);
        // Revoking one does not affect others
        client.revoke_role(&admin, &v1);
        assert_eq!(client.get_role(&v1), Role::None);
        assert_eq!(client.get_role(&v2), Role::Verifier);
    }

    // ── revoke_role ───────────────────────────────────────────────────────────

    #[test]
    fn test_revoke_role_success() {
        let (env, admin, client) = setup();
        let operator = Address::generate(&env);
        client.grant_role(&admin, &operator, &Role::Operator);
        client.revoke_role(&admin, &operator);
        assert_eq!(client.get_role(&operator), Role::None);
    }

    #[test]
    fn test_revoke_role_requires_admin_auth() {
        let (env, admin, client) = setup();
        let target = Address::generate(&env);
        client.grant_role(&admin, &target, &Role::Operator);
        env.mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "revoke_role",
                args: (&admin, &target).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        assert!(client.try_revoke_role(&admin, &target).is_ok());
    }

    #[test]
    fn test_revoke_role_non_admin_returns_not_admin() {
        let (env, admin, client) = setup();
        let operator = Address::generate(&env);
        let stranger = Address::generate(&env);
        client.grant_role(&admin, &operator, &Role::Operator);
        let result = client.try_revoke_role(&stranger, &operator);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::NotAdmin);
    }

    #[test]
    fn test_revoke_role_admin_returns_unauthorized() {
        let (_, admin, client) = setup();
        let result = client.try_revoke_role(&admin, &admin);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::Unauthorized);
    }

    #[test]
    fn test_revoke_role_not_assigned_returns_correct_error() {
        let (env, admin, client) = setup();
        let stranger = Address::generate(&env);
        let result = client.try_revoke_role(&admin, &stranger);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::RoleNotAssigned);
    }

    #[test]
    fn test_revoke_role_state_unchanged_after_failed_revoke() {
        let (env, admin, client) = setup();
        let stranger = Address::generate(&env);
        let _ = client.try_revoke_role(&admin, &stranger);
        assert_eq!(client.get_role(&stranger), Role::None);
    }

    #[test]
    fn test_revoke_role_twice_fails_second_time() {
        let (env, admin, client) = setup();
        let user = Address::generate(&env);
        client.grant_role(&admin, &user, &Role::Verifier);
        client.revoke_role(&admin, &user);
        let result = client.try_revoke_role(&admin, &user);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::RoleNotAssigned);
    }

    #[test]
    fn test_revoke_then_re_grant() {
        let (env, admin, client) = setup();
        let user = Address::generate(&env);
        client.grant_role(&admin, &user, &Role::Verifier);
        client.revoke_role(&admin, &user);
        client.grant_role(&admin, &user, &Role::Operator);
        assert_eq!(client.get_role(&user), Role::Operator);
    }

    // ── transfer_admin ────────────────────────────────────────────────────────

    #[test]
    fn test_transfer_admin_success() {
        let (env, admin, client) = setup();
        let new_admin = Address::generate(&env);
        client.transfer_admin(&admin, &new_admin);
        assert_eq!(client.get_admin(), new_admin);
        assert_eq!(client.get_role(&new_admin), Role::Admin);
        assert_eq!(client.get_role(&admin), Role::None);
    }

    #[test]
    fn test_transfer_admin_requires_current_admin_auth() {
        let (env, admin, client) = setup();
        let new_admin = Address::generate(&env);
        env.mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "transfer_admin",
                args: (&admin, &new_admin).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        assert!(client.try_transfer_admin(&admin, &new_admin).is_ok());
    }

    #[test]
    fn test_transfer_admin_non_admin_returns_not_admin() {
        let (env, _, client) = setup();
        let stranger = Address::generate(&env);
        let new_admin = Address::generate(&env);
        let result = client.try_transfer_admin(&stranger, &new_admin);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::NotAdmin);
    }

    #[test]
    fn test_transfer_admin_to_self_returns_invalid_address() {
        let (_, admin, client) = setup();
        let result = client.try_transfer_admin(&admin, &admin);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::InvalidAddress);
    }

    #[test]
    fn test_transfer_admin_to_operator_returns_unauthorized() {
        let (env, admin, client) = setup();
        let operator = Address::generate(&env);
        client.grant_role(&admin, &operator, &Role::Operator);
        let result = client.try_transfer_admin(&admin, &operator);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::Unauthorized);
    }

    #[test]
    fn test_transfer_admin_to_verifier_returns_unauthorized() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        client.grant_role(&admin, &verifier, &Role::Verifier);
        let result = client.try_transfer_admin(&admin, &verifier);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::Unauthorized);
    }

    #[test]
    fn test_transfer_admin_state_unchanged_after_failed_transfer() {
        let (env, admin, client) = setup();
        let _ = client.try_transfer_admin(&admin, &admin);
        assert_eq!(client.get_admin(), admin);
        assert_eq!(client.get_role(&admin), Role::Admin);
    }

    #[test]
    fn test_transfer_admin_old_admin_loses_all_privileges() {
        let (env, admin, client) = setup();
        let new_admin = Address::generate(&env);
        client.transfer_admin(&admin, &new_admin);
        // Old admin cannot pause
        assert!(client.try_pause(&admin).is_err());
        // Old admin cannot grant roles
        let target = Address::generate(&env);
        assert!(client.try_grant_role(&admin, &target, &Role::Verifier).is_err());
        // Old admin cannot transfer admin again
        assert!(client.try_transfer_admin(&admin, &target).is_err());
    }

    #[test]
    fn test_transfer_admin_new_admin_has_full_privileges() {
        let (env, admin, client) = setup();
        let new_admin = Address::generate(&env);
        client.transfer_admin(&admin, &new_admin);
        client.pause(&new_admin);
        assert!(client.is_paused());
        client.unpause(&new_admin);
        let target = Address::generate(&env);
        client.grant_role(&new_admin, &target, &Role::Verifier);
        assert_eq!(client.get_role(&target), Role::Verifier);
    }

    #[test]
    fn test_transfer_admin_chain_a_to_b_to_c() {
        let (env, admin_a, client) = setup();
        let admin_b = Address::generate(&env);
        let admin_c = Address::generate(&env);
        client.transfer_admin(&admin_a, &admin_b);
        assert_eq!(client.get_admin(), admin_b);
        client.transfer_admin(&admin_b, &admin_c);
        assert_eq!(client.get_admin(), admin_c);
        assert_eq!(client.get_role(&admin_a), Role::None);
        assert_eq!(client.get_role(&admin_b), Role::None);
        assert_eq!(client.get_role(&admin_c), Role::Admin);
    }

    #[test]
    fn test_transfer_admin_to_clean_address_succeeds() {
        let (env, admin, client) = setup();
        let new_admin = Address::generate(&env);
        assert_eq!(client.get_role(&new_admin), Role::None);
        assert!(client.try_transfer_admin(&admin, &new_admin).is_ok());
    }

    // ── get_admin ─────────────────────────────────────────────────────────────

    #[test]
    fn test_get_admin_before_init_returns_not_initialized() {
        let (_, client) = deploy_uninit();
        let result = client.try_get_admin();
        assert_eq!(result.unwrap_err().unwrap(), KoraError::NotInitialized);
    }

    #[test]
    fn test_get_admin_returns_correct_address() {
        let (_, admin, client) = setup();
        assert_eq!(client.get_admin(), admin);
    }

    // ── get_role ──────────────────────────────────────────────────────────────

    #[test]
    fn test_get_role_unknown_address_returns_none() {
        let (env, _, client) = setup();
        let unknown = Address::generate(&env);
        assert_eq!(client.get_role(&unknown), Role::None);
    }

    #[test]
    fn test_get_role_admin_returns_admin() {
        let (_, admin, client) = setup();
        assert_eq!(client.get_role(&admin), Role::Admin);
    }

    // ── is_paused ─────────────────────────────────────────────────────────────

    #[test]
    fn test_is_paused_default_false() {
        let (_, _, client) = setup();
        assert!(!client.is_paused());
    }

    #[test]
    fn test_is_paused_reflects_state_correctly() {
        let (_, admin, client) = setup();
        assert!(!client.is_paused());
        client.pause(&admin);
        assert!(client.is_paused());
        client.unpause(&admin);
        assert!(!client.is_paused());
    }

    // ── cross-function interaction ────────────────────────────────────────────

    #[test]
    fn test_revoke_role_then_transfer_admin_to_that_address_succeeds() {
        let (env, admin, client) = setup();
        let user = Address::generate(&env);
        client.grant_role(&admin, &user, &Role::Operator);
        client.revoke_role(&admin, &user);
        assert_eq!(client.get_role(&user), Role::None);
        assert!(client.try_transfer_admin(&admin, &user).is_ok());
        assert_eq!(client.get_admin(), user);
    }

    #[test]
    fn test_pause_does_not_affect_role_state() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        client.grant_role(&admin, &verifier, &Role::Verifier);
        client.pause(&admin);
        assert_eq!(client.get_role(&verifier), Role::Verifier);
        assert_eq!(client.get_role(&admin), Role::Admin);
    }

    #[test]
    fn test_grant_and_revoke_do_not_affect_pause_state() {
        let (env, admin, client) = setup();
        let user = Address::generate(&env);
        client.pause(&admin);
        client.grant_role(&admin, &user, &Role::Verifier);
        assert!(client.is_paused());
        client.revoke_role(&admin, &user);
        assert!(client.is_paused());
    }

    // ── storage key migration: InstanceKey / PersistentKey ────────────────────

    #[test]
    fn test_instance_key_admin_and_paused_are_independent() {
        // Mutating Paused must not affect Admin and vice versa
        let (_, admin, client) = setup();
        client.pause(&admin);
        assert_eq!(client.get_admin(), admin); // Admin unchanged after pause
        client.unpause(&admin);
        assert_eq!(client.get_admin(), admin); // Admin unchanged after unpause
    }

    #[test]
    fn test_persistent_key_role_entries_are_independent() {
        // Each Role(Address) entry is keyed independently; removing one must not
        // affect any other address's role entry.
        let (env, admin, client) = setup();
        let u1 = Address::generate(&env);
        let u2 = Address::generate(&env);
        let u3 = Address::generate(&env);
        client.grant_role(&admin, &u1, &Role::Operator);
        client.grant_role(&admin, &u2, &Role::Verifier);
        client.grant_role(&admin, &u3, &Role::Operator);

        client.revoke_role(&admin, &u2);

        assert_eq!(client.get_role(&u1), Role::Operator);
        assert_eq!(client.get_role(&u2), Role::None); // removed
        assert_eq!(client.get_role(&u3), Role::Operator);
        assert_eq!(client.get_role(&admin), Role::Admin); // admin unaffected
    }

    #[test]
    fn test_no_role_none_tombstone_after_revoke() {
        // After revoke_role the entry is removed (not set to Role::None),
        // so get_role returns None via the unwrap_or default — not a stored value.
        // We verify the observable behaviour: get_role returns None.
        let (env, admin, client) = setup();
        let user = Address::generate(&env);
        client.grant_role(&admin, &user, &Role::Verifier);
        client.revoke_role(&admin, &user);
        assert_eq!(client.get_role(&user), Role::None);
        // Re-granting after removal must succeed (no stale tombstone blocking it)
        assert!(client.try_grant_role(&admin, &user, &Role::Operator).is_ok());
        assert_eq!(client.get_role(&user), Role::Operator);
    }

    #[test]
    fn test_transfer_admin_removes_old_role_entry() {
        // After transfer_admin the old admin's persistent Role entry is removed,
        // not left as Role::Admin.
        let (env, admin, client) = setup();
        let new_admin = Address::generate(&env);
        client.transfer_admin(&admin, &new_admin);
        // Old admin's role must be gone (returns None via default)
        assert_eq!(client.get_role(&admin), Role::None);
        // New admin's role must be Admin
        assert_eq!(client.get_role(&new_admin), Role::Admin);
    }
}
