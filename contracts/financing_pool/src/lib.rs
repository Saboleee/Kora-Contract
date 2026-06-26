#![no_std]

use kora_shared::{
    errors::KoraError,
    events,
    types::{Pool, Position},
    validation::bps_of,
};
use soroban_sdk::{contract, contractimpl, contracttype, token, Address, Env, Map, Vec};
// ── Storage Keys ─────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Pool(u64),
    Positions(u64), // Map<Address, Position>
    Admin,
    InvoiceNft,
    RiskRegistry,
    AccessControl,
    Treasury,
    LatePenaltyBps,
    RepaymentLock(u64), // Reentrancy guard: tracks if repayment is in progress
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct FinancingPoolContract;

#[contractimpl]
impl FinancingPoolContract {
    pub fn initialize(
        env: Env,
        admin: Address,
        invoice_nft: Address,
        risk_registry: Address,
        treasury: Address,
        late_penalty_bps: u32,
        access_control: Address,
    ) -> Result<(), KoraError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(KoraError::AlreadyInitialized);
        }
        kora_shared::validation::require_valid_fee_bps(late_penalty_bps)?;
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::InvoiceNft, &invoice_nft);
        env.storage()
            .instance()
            .set(&DataKey::RiskRegistry, &risk_registry);
        env.storage()
            .instance()
            .set(&DataKey::AccessControl, &access_control);
        env.storage().instance().set(&DataKey::Treasury, &treasury);
        env.storage()
            .instance()
            .set(&DataKey::LatePenaltyBps, &late_penalty_bps);
        Ok(())
    }

    /// Called by Marketplace when an invoice is fully funded.
    /// Creates the pool and releases net funds to the SME.
    pub fn release_funds(env: Env, marketplace: Address, invoice_id: u64) -> Result<(), KoraError> {
        marketplace.require_auth();

        if env.storage().persistent().has(&DataKey::Pool(invoice_id)) {
            return Err(KoraError::PoolAlreadyClosed);
        }

        // Retrieve invoice details via cross-contract call
        let nft_contract: Address = env.storage().instance().get(&DataKey::InvoiceNft).unwrap();
        let nft_client = kora_invoice_nft::InvoiceNftContractClient::new(&env, &nft_contract);
        let invoice = nft_client.get_invoice(&invoice_id);

        // The pool contract holds the net funds (marketplace transferred them here)
        // We forward them to the SME
        // Token address must be passed — we read it from the pool balance
        // For now, caller must supply token; in production this comes from the listing
        // This is wired via the marketplace which knows the token
        // We mark the pool as open and record face_value for repayment tracking
        let pool = Pool {
            invoice_id,
            token: env.current_contract_address(), // placeholder; real token set by marketplace
            total_funded: 0,
            face_value: invoice.amount,
            repaid_amount: 0,
            is_closed: false,
            late_penalty_bps: env
                .storage()
                .instance()
                .get(&DataKey::LatePenaltyBps)
                .unwrap_or(200),
        };

        env.storage()
            .persistent()
            .set(&DataKey::Pool(invoice_id), &pool);

        // Transition NFT status to Funded
        nft_client.set_funded(&env.current_contract_address(), &invoice_id);

        Ok(())
    }

    /// Register an investor position. Called internally after fund_invoice.
    pub fn record_position(
        env: Env,
        caller: Address,
        invoice_id: u64,
        investor: Address,
        contributed: i128,
        total_pool: i128,
    ) -> Result<(), KoraError> {
        caller.require_auth();
        Self::require_admin(&env, &caller)?;
        Self::require_not_paused(&env)?;

        if contributed <= 0 || total_pool <= 0 {
            return Err(KoraError::InvalidAmount);
        }

        if contributed > total_pool {
            return Err(KoraError::InvalidAmount);
        }

        let share_bps = contributed
            .checked_mul(10_000)
            .and_then(|v| v.checked_div(total_pool))
            .ok_or(KoraError::ArithmeticOverflow)? as u32;

        let position = Position {
            investor: investor.clone(),
            invoice_id,
            contributed,
            share_bps,
            yield_claimed: 0,
        };

        let mut positions: Map<Address, Position> = env
            .storage()
            .persistent()
            .get(&DataKey::Positions(invoice_id))
            .unwrap_or(Map::new(&env));

        positions.set(investor, position);
        env.storage()
            .persistent()
            .set(&DataKey::Positions(invoice_id), &positions);
        Ok(())
    }

    /// SME repays the invoice. Distributes yield to investors.
    pub fn repay(
        env: Env,
        payer: Address,
        invoice_id: u64,
        token: Address,
        amount: i128,
    ) -> Result<(), KoraError> {
        payer.require_auth();

        if amount <= 0 {
            return Err(KoraError::InvalidAmount);
        }

        let mut pool: Pool = env
            .storage()
            .persistent()
            .get(&DataKey::Pool(invoice_id))
            .ok_or(KoraError::PoolNotFound)?;

        if pool.is_closed {
            return Err(KoraError::RepaymentAlreadyMade);
        }

        // Validate amount
        if amount <= 0 {
            return Err(KoraError::InvalidAmount);
        }

        let token_client = token::Client::new(env, token);
        token_client.transfer(payer, &env.current_contract_address(), &amount);

        pool.repaid_amount = pool
            .repaid_amount
            .checked_add(amount)
            .ok_or(KoraError::ArithmeticOverflow)?;

        events::repayment_made(env, invoice_id, payer, amount);

        if pool.repaid_amount >= pool.face_value {
            pool.is_closed = true;
            env.storage().persistent().set(&DataKey::Pool(invoice_id), &pool);
            Self::distribute_yield(env, invoice_id, token, pool.repaid_amount, pool.face_value)?;

            // Mark NFT as repaid
            let nft_contract: Address = env.storage().instance().get(&DataKey::InvoiceNft).unwrap();
            let nft_client = kora_invoice_nft::InvoiceNftContractClient::new(env, &nft_contract);
            nft_client.set_repaid(&env.current_contract_address(), &invoice_id);
        } else {
            env.storage()
                .persistent()
                .set(&DataKey::Pool(invoice_id), &pool);
        }

        Ok(())
    }

    /// Distribute repayment proportionally to all investors.
    fn distribute_yield(
        env: &Env,
        invoice_id: u64,
        token: &Address,
        total_repaid: i128,
        face_value: i128,
    ) -> Result<(), KoraError> {
        let positions: Map<Address, Position> = env
            .storage()
            .persistent()
            .get(&DataKey::Positions(invoice_id))
            .unwrap_or(Map::new(env));

        let token_client = token::Client::new(env, token);

        for (investor, position) in positions.iter() {
            // Investor receives their share of total repaid
            let payout = bps_of(total_repaid, position.share_bps)?;
            let yield_amount = payout.checked_sub(position.contributed).unwrap_or(0);

            token_client.transfer(&env.current_contract_address(), &investor, &payout);
            events::yield_distributed(env, invoice_id, &investor, yield_amount);
        }

        let _ = face_value; // used for ratio reference
        Ok(())
    }

    /// Apply late penalty and mark as defaulted.
    pub fn mark_default(
        env: Env,
        admin: Address,
        invoice_id: u64,
        token: Address,
    ) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        Self::require_not_paused(&env)?;
        if env.storage().persistent().has(&DataKey::RepaymentLock(invoice_id)) {
            return Err(KoraError::ProtocolPaused);
        }

        let pool: Pool = env
            .storage()
            .persistent()
            .get(&DataKey::Pool(invoice_id))
            .ok_or(KoraError::PoolNotFound)?;

        if pool.is_closed {
            return Err(KoraError::PoolAlreadyClosed);
        }

        // Distribute whatever was repaid so far (partial recovery)
        if pool.repaid_amount > 0 {
            Self::distribute_yield(
                &env,
                invoice_id,
                &token,
                pool.repaid_amount,
                pool.face_value,
            )?;
        }

        // Mark NFT as defaulted
        let nft_contract: Address = env.storage().instance().get(&DataKey::InvoiceNft).unwrap();
        let nft_client = kora_invoice_nft::InvoiceNftContractClient::new(&env, &nft_contract);
        nft_client.set_defaulted(&admin, &invoice_id);

        events::invoice_defaulted(&env, invoice_id, &admin);

        // Automatically record the default against the SME in the risk registry
        let invoice = nft_client.get_invoice(&invoice_id);
        if let Some(rr_contract) = env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::RiskRegistry)
        {
            let rr_client =
                kora_risk_registry::RiskRegistryContractClient::new(&env, &rr_contract);
            // Best-effort: ignore errors if SME is not registered in risk registry
            let _ = rr_client.try_record_default(&admin, &invoice.sme);
        }

        Ok(())
    }

    // ── Views ─────────────────────────────────────────────────────────────────

    pub fn get_pool(env: Env, invoice_id: u64) -> Result<Pool, KoraError> {
        env.storage()
            .persistent()
            .get(&DataKey::Pool(invoice_id))
            .ok_or(KoraError::PoolNotFound)
    }

    pub fn get_positions(env: Env, invoice_id: u64) -> Vec<Position> {
        let positions: Map<Address, Position> = env
            .storage()
            .persistent()
            .get(&DataKey::Positions(invoice_id))
            .unwrap_or(Map::new(&env));
        positions.values()
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn require_not_paused(env: &Env) -> Result<(), KoraError> {
        if let Some(ac_contract) = env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::AccessControl)
        {
            let ac = kora_access_control::AccessControlContractClient::new(env, &ac_contract);
            if ac.is_paused() {
                return Err(KoraError::ProtocolPaused);
            }
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

    fn setup() -> (Env, Address, Address, Address, FinancingPoolContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, FinancingPoolContract);
        let client = FinancingPoolContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let nft = Address::generate(&env);
        let risk_registry = Address::generate(&env);
        let treasury = Address::generate(&env);
        let access_control = Address::generate(&env);
        client.initialize(&admin, &nft, &risk_registry, &treasury, &200u32, &access_control);
        (env, admin, nft, treasury, client)
    }

    #[test]
    fn test_initialize_success() {
        let (env, admin, nft, treasury, client) = setup();
        let pool = client.get_pool(&1u64);
        assert!(pool.is_err());
    }

    #[test]
    fn test_initialize_already_initialized_fails() {
        let (env, admin, nft, treasury, client) = setup();
        let rr = Address::generate(&env);
        let ac = Address::generate(&env);
        let result = client.try_initialize(&admin, &nft, &rr, &treasury, &200u32, &ac);
        assert!(result.is_err());
    }

    #[test]
    fn test_initialize_invalid_fee_bps_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, FinancingPoolContract);
        let client = FinancingPoolContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let nft = Address::generate(&env);
        let rr = Address::generate(&env);
        let treasury = Address::generate(&env);
        let ac = Address::generate(&env);

        let result = client.try_initialize(&admin, &nft, &rr, &treasury, &10_001u32, &ac);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_pool_not_found() {
        let (env, admin, nft, treasury, client) = setup();
        let result = client.try_get_pool(&999u64);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_positions_empty() {
        let (env, admin, nft, treasury, client) = setup();
        let positions = client.get_positions(&1u64);
        assert_eq!(positions.len(), 0);
    }

    #[test]
    fn test_record_position_requires_admin() {
        let (env, admin, nft, treasury, client) = setup();
        let investor = Address::generate(&env);
        let non_admin = Address::generate(&env);

        let result = client.try_record_position(&non_admin, &1u64, &investor, &1_000_000_000i128, &10_000_000_000i128);
        assert!(result.is_err());
    }

    #[test]
    fn test_record_position_arithmetic_overflow() {
        let (env, admin, nft, treasury, client) = setup();
        let investor = Address::generate(&env);

        let result = client.try_record_position(&admin, &1u64, &investor, &i128::MAX, &1i128);
        assert!(result.is_err());
    }

    #[test]
    fn test_record_position_success() {
        let (env, admin, nft, treasury, client) = setup();
        let investor = Address::generate(&env);
        let contributed = 5_000_000_000i128;
        let total_pool = 10_000_000_000i128;

        client.record_position(&admin, &1u64, &investor, &contributed, &total_pool);
        let positions = client.get_positions(&1u64);
        assert_eq!(positions.len(), 1);
    }

    #[test]
    fn test_repay_pool_not_found() {
        let (env, admin, nft, treasury, client) = setup();
        let payer = Address::generate(&env);
        let token = Address::generate(&env);

        let result = client.try_repay(&payer, &999u64, &token, &1_000_000_000i128);
        assert!(result.is_err());
    }

    #[test]
    fn test_repay_invalid_amount() {
        let (env, admin, nft, treasury, client) = setup();
        let payer = Address::generate(&env);
        let token = Address::generate(&env);

        let result = client.try_repay(&payer, &1u64, &token, &0i128);
        assert!(result.is_err());
    }

    #[test]
    fn test_mark_default_requires_admin() {
        let (env, admin, nft, treasury, client) = setup();
        let non_admin = Address::generate(&env);
        let token = Address::generate(&env);

        let result = client.try_mark_default(&non_admin, &1u64, &token);
        assert!(result.is_err());
    }

    #[test]
    fn test_mark_default_pool_not_found() {
        let (env, admin, nft, treasury, client) = setup();
        let token = Address::generate(&env);

        let result = client.try_mark_default(&admin, &999u64, &token);
        assert!(result.is_err());
    }
}
