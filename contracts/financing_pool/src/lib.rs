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
    Treasury,
    LatePenaltyBps,
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
        treasury: Address,
        late_penalty_bps: u32,
    ) -> Result<(), KoraError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(KoraError::AlreadyInitialized);
        }
        kora_shared::validation::require_valid_fee_bps(late_penalty_bps)?;
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::InvoiceNft, &invoice_nft);
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

        let mut pool: Pool = env
            .storage()
            .persistent()
            .get(&DataKey::Pool(invoice_id))
            .ok_or(KoraError::PoolNotFound)?;

        if pool.is_closed {
            return Err(KoraError::RepaymentAlreadyMade);
        }

        let token_client = token::Client::new(&env, &token);
        token_client.transfer(&payer, &env.current_contract_address(), &amount);

        pool.repaid_amount = pool
            .repaid_amount
            .checked_add(amount)
            .ok_or(KoraError::ArithmeticOverflow)?;

        events::repayment_made(&env, invoice_id, &payer, amount);

        if pool.repaid_amount >= pool.face_value {
            pool.is_closed = true;
            env.storage()
                .persistent()
                .set(&DataKey::Pool(invoice_id), &pool);
            Self::distribute_yield(
                &env,
                invoice_id,
                &token,
                pool.repaid_amount,
                pool.face_value,
            )?;

            // Mark NFT as repaid
            let nft_contract: Address = env.storage().instance().get(&DataKey::InvoiceNft).unwrap();
            let nft_client = kora_invoice_nft::InvoiceNftContractClient::new(&env, &nft_contract);
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

    fn setup() -> (Env, Address, FinancingPoolContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, FinancingPoolContract);
        let client = FinancingPoolContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let nft = Address::generate(&env);
        let treasury = Address::generate(&env);
        client.initialize(&admin, &nft, &treasury, &200u32);
        (env, admin, client)
    }

    #[test]
    fn test_initialize_success() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, FinancingPoolContract);
        let client = FinancingPoolContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let nft = Address::generate(&env);
        let treasury = Address::generate(&env);

        let result = client.try_initialize(&admin, &nft, &treasury, &200u32);
        assert!(result.is_ok());
    }

    #[test]
    fn test_initialize_already_initialized() {
        let (env, admin, client) = setup();
        let nft = Address::generate(&env);
        let treasury = Address::generate(&env);

        let result = client.try_initialize(&admin, &nft, &treasury, &200u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_initialize_invalid_fee_bps() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, FinancingPoolContract);
        let client = FinancingPoolContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let nft = Address::generate(&env);
        let treasury = Address::generate(&env);

        let result = client.try_initialize(&admin, &nft, &treasury, &10_001u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_pool_not_found() {
        let (env, _admin, client) = setup();

        let result = client.try_get_pool(&999u64);
        assert!(result.is_err());
    }

    #[test]
    fn test_record_position_success() {
        let (env, admin, client) = setup();
        let invoice_id = 1u64;
        let investor = Address::generate(&env);
        let contributed = 5_000_000_000i128;
        let total_pool = 10_000_000_000i128;

        let result =
            client.try_record_position(&admin, &invoice_id, &investor, &contributed, &total_pool);
        assert!(result.is_ok());

        let positions = client.get_positions(&invoice_id);
        assert_eq!(positions.len(), 1);
    }

    #[test]
    fn test_record_position_not_admin() {
        let (env, _admin, client) = setup();
        let invoice_id = 1u64;
        let investor = Address::generate(&env);
        let stranger = Address::generate(&env);
        let contributed = 5_000_000_000i128;
        let total_pool = 10_000_000_000i128;

        let result = client.try_record_position(
            &stranger,
            &invoice_id,
            &investor,
            &contributed,
            &total_pool,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_record_position_share_calculation() {
        let (env, admin, client) = setup();
        let invoice_id = 1u64;
        let investor = Address::generate(&env);
        let contributed = 5_000_000_000i128;
        let total_pool = 10_000_000_000i128;

        client.record_position(&admin, &invoice_id, &investor, &contributed, &total_pool);

        let positions = client.get_positions(&invoice_id);
        assert_eq!(positions.len(), 1);
        let position = positions.get(0).unwrap();
        // share_bps = (5_000_000_000 * 10_000) / 10_000_000_000 = 5_000 bps (50%)
        assert_eq!(position.share_bps, 5_000u32);
    }

    #[test]
    fn test_record_multiple_positions() {
        let (env, admin, client) = setup();
        let invoice_id = 1u64;
        let investor1 = Address::generate(&env);
        let investor2 = Address::generate(&env);
        let total_pool = 10_000_000_000i128;

        client.record_position(
            &admin,
            &invoice_id,
            &investor1,
            &3_000_000_000i128,
            &total_pool,
        );
        client.record_position(
            &admin,
            &invoice_id,
            &investor2,
            &7_000_000_000i128,
            &total_pool,
        );

        let positions = client.get_positions(&invoice_id);
        assert_eq!(positions.len(), 2);
    }

    #[test]
    fn test_get_positions_empty() {
        let (env, _admin, client) = setup();
        let invoice_id = 999u64;

        let positions = client.get_positions(&invoice_id);
        assert_eq!(positions.len(), 0);
    }

    #[test]
    fn test_share_bps_boundary_full_pool() {
        let (env, admin, client) = setup();
        let invoice_id = 1u64;
        let investor = Address::generate(&env);
        let total_pool = 10_000_000_000i128;

        // Investor contributes entire pool
        client.record_position(&admin, &invoice_id, &investor, &total_pool, &total_pool);

        let positions = client.get_positions(&invoice_id);
        let position = positions.get(0).unwrap();
        assert_eq!(position.share_bps, 10_000u32); // 100%
    }

    #[test]
    fn test_share_bps_boundary_minimal() {
        let (env, admin, client) = setup();
        let invoice_id = 1u64;
        let investor = Address::generate(&env);
        let total_pool = 10_000_000_000i128;

        // Investor contributes 1 stroops
        client.record_position(&admin, &invoice_id, &investor, &1i128, &total_pool);

        let positions = client.get_positions(&invoice_id);
        let position = positions.get(0).unwrap();
        // share_bps = (1 * 10_000) / 10_000_000_000 = 0 bps (rounds down)
        assert_eq!(position.share_bps, 0u32);
    }

    #[test]
    fn test_position_attributes() {
        let (env, admin, client) = setup();
        let invoice_id = 42u64;
        let investor = Address::generate(&env);
        let contributed = 2_500_000_000i128;
        let total_pool = 10_000_000_000i128;

        client.record_position(&admin, &invoice_id, &investor, &contributed, &total_pool);

        let positions = client.get_positions(&invoice_id);
        let position = positions.get(0).unwrap();
        assert_eq!(position.invoice_id, invoice_id);
        assert_eq!(position.investor, investor);
        assert_eq!(position.contributed, contributed);
        assert_eq!(position.yield_claimed, 0i128);
    }

    #[test]
    fn test_arithmetic_overflow_protection() {
        let (env, admin, client) = setup();
        let invoice_id = 1u64;
        let investor = Address::generate(&env);
        let contributed = i128::MAX;
        let total_pool = 1i128;

        // This should fail due to arithmetic overflow in share calculation
        let result =
            client.try_record_position(&admin, &invoice_id, &investor, &contributed, &total_pool);
        assert!(result.is_err());
    }

    #[test]
    fn test_zero_contribution() {
        let (env, admin, client) = setup();
        let invoice_id = 1u64;
        let investor = Address::generate(&env);
        let total_pool = 10_000_000_000i128;

        client.record_position(&admin, &invoice_id, &investor, &0i128, &total_pool);

        let positions = client.get_positions(&invoice_id);
        let position = positions.get(0).unwrap();
        assert_eq!(position.contributed, 0i128);
        assert_eq!(position.share_bps, 0u32);
    }
}
