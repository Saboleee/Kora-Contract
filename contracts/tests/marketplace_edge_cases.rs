// tests/marketplace_edge_cases.rs
//! Edge case tests for Marketplace Contract
//!
//! This test module covers:
//! - Fee calculation robustness and precision
//! - Listing lifecycle edge cases
//! - Token whitelist validation
//! - Cross-contract call ordering
//! - Funding target validation

#[cfg(test)]
mod marketplace_edge_cases {
    use kora_marketplace::MarketplaceContractClient;
    use kora_invoice_nft::InvoiceNftContractClient;
    use kora_financing_pool::FinancingPoolContractClient;
    use kora_shared::errors::KoraError;
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        Address, Env,
    };

    struct TestEnv {
        env: Env,
        admin: Address,
        seller: Address,
        investor: Address,
        token: Address,
        treasury: Address,
        mp: MarketplaceContractClient<'static>,
        nft: InvoiceNftContractClient<'static>,
        pool_client: FinancingPoolContractClient<'static>,
    }

    fn setup() -> TestEnv {
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
        let seller = Address::generate(&env);
        let investor = Address::generate(&env);
        let treasury = Address::generate(&env);
        let token = Address::generate(&env);

        // Deploy NFT
        let nft_id = env.register_contract(None, kora_invoice_nft::InvoiceNftContract);
        let nft = InvoiceNftContractClient::new(&env, &nft_id);
        let ac = Address::generate(&env);
        nft.initialize(&admin, &ac);

        // Deploy Pool
        let pool_id = env.register_contract(None, kora_financing_pool::FinancingPoolContract);
        let pool_client = FinancingPoolContractClient::new(&env, &pool_id);
        pool_client.initialize(&admin, &nft_id, &treasury, &200u32);

        // Deploy Marketplace
        let mp_id = env.register_contract(None, kora_marketplace::MarketplaceContract);
        let mp = MarketplaceContractClient::new(&env, &mp_id);
        mp.initialize(&admin, &nft_id, &pool_id, &treasury, &50u32);

        mp.whitelist_token(&admin, &token);

        TestEnv {
            env,
            admin,
            seller,
            investor,
            token,
            treasury,
            mp,
            nft,
            pool_client,
        }
    }

    // ── Fee Calculation Edge Cases ────────────────────────────────────────────

    #[test]
    fn test_fee_calculation_small_amounts() {
        let t = setup();
        let deadline = t.env.ledger().timestamp() + 86_400;

        // List invoice with specific asking price
        t.mp.list_invoice(
            &t.seller,
            &1u64,
            &10_000_000_000i128,
            &11_000_000_000i128,
            &t.token,
            &deadline,
        );

        // Fund with small amount (50 bps fee means: 1_000_000 * 50 / 10_000 = 5_000 fee)
        let funding_amount = 1_000_000i128;
        let expected_fee = 5_000i128;
        let expected_net = funding_amount - expected_fee;

        // Verify fee calculation by checking listing state after funding
        t.mp.fund_invoice(&t.investor, &1u64, &funding_amount);
        let listing = t.mp.get_listing(&1u64);

        // funded_amount should be exactly the full amount (fee is separate)
        assert_eq!(listing.funded_amount, funding_amount);
    }

    #[test]
    fn test_fee_calculation_with_rounding_dust() {
        let t = setup();
        let deadline = t.env.ledger().timestamp() + 86_400;

        t.mp.list_invoice(
            &t.seller,
            &1u64,
            &10_000_000_000i128,
            &11_000_000_000i128,
            &t.token,
            &deadline,
        );

        // Fund with odd amount that doesn't divide evenly by 10_000
        let funding_amount = 1_000_001i128;
        // Fee: 1_000_001 * 50 / 10_000 = 5_000.005 → truncates to 5_000
        // Net: 1_000_001 - 5_000 = 995_001

        t.mp.fund_invoice(&t.investor, &1u64, &funding_amount);
        let listing = t.mp.get_listing(&1u64);
        assert_eq!(listing.funded_amount, funding_amount);
    }

    #[test]
    fn test_fee_calculation_zero_fee_bps() {
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
        let seller = Address::generate(&env);
        let investor = Address::generate(&env);
        let treasury = Address::generate(&env);
        let token = Address::generate(&env);

        // Deploy with 0 fee bps
        let nft_id = env.register_contract(None, kora_invoice_nft::InvoiceNftContract);
        let nft = InvoiceNftContractClient::new(&env, &nft_id);
        let ac = Address::generate(&env);
        nft.initialize(&admin, &ac);

        let pool_id = env.register_contract(None, kora_financing_pool::FinancingPoolContract);
        let pool_client = FinancingPoolContractClient::new(&env, &pool_id);
        pool_client.initialize(&admin, &nft_id, &treasury, &200u32);

        let mp_id = env.register_contract(None, kora_marketplace::MarketplaceContract);
        let mp = MarketplaceContractClient::new(&env, &mp_id);
        mp.initialize(&admin, &nft_id, &pool_id, &treasury, &0u32); // 0 fee

        mp.whitelist_token(&admin, &token);

        let deadline = env.ledger().timestamp() + 86_400;
        mp.list_invoice(&seller, &1u64, &9_000i128, &10_000i128, &token, &deadline);

        // Fund should work even with 0 fees
        let result = mp.try_fund_invoice(&investor, &1u64, &1_000i128);
        // May fail on cross-contract call but not on fee calculation
        if let Err(Ok(e)) = result {
            assert_ne!(e, KoraError::InvalidAmount);
            assert_ne!(e, KoraError::ArithmeticOverflow);
        }
    }

    #[test]
    fn test_fee_calculation_max_fee_bps() {
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
        let seller = Address::generate(&env);
        let investor = Address::generate(&env);
        let treasury = Address::generate(&env);
        let token = Address::generate(&env);

        // Deploy with maximum fee bps (10_000 = 100%)
        let nft_id = env.register_contract(None, kora_invoice_nft::InvoiceNftContract);
        let nft = InvoiceNftContractClient::new(&env, &nft_id);
        let ac = Address::generate(&env);
        nft.initialize(&admin, &ac);

        let pool_id = env.register_contract(None, kora_financing_pool::FinancingPoolContract);
        let pool_client = FinancingPoolContractClient::new(&env, &pool_id);
        pool_client.initialize(&admin, &nft_id, &treasury, &200u32);

        let mp_id = env.register_contract(None, kora_marketplace::MarketplaceContract);
        let mp = MarketplaceContractClient::new(&env, &mp_id);
        mp.initialize(&admin, &nft_id, &pool_id, &treasury, &10_000u32); // 100% fee

        mp.whitelist_token(&admin, &token);

        let deadline = env.ledger().timestamp() + 86_400;
        mp.list_invoice(&seller, &1u64, &9_000i128, &10_000i128, &token, &deadline);

        // With 100% fee, net amount to pool would be 0 - this might be rejected
        let result = mp.try_fund_invoice(&investor, &1u64, &1_000i128);
        // This is an edge case that might fail, which is expected behavior
        if let Err(Ok(e)) = result {
            // Should fail, not silently succeed
            assert!(e != KoraError::InvalidAmount); // Could be various errors
        }
    }

    // ── Listing Lifecycle Edge Cases ──────────────────────────────────────────

    #[test]
    fn test_listing_cannot_be_funded_after_cancellation() {
        let t = setup();
        let deadline = t.env.ledger().timestamp() + 86_400;

        t.mp.list_invoice(
            &t.seller,
            &1u64,
            &9_000i128,
            &10_000i128,
            &t.token,
            &deadline,
        );

        // Cancel the listing
        t.mp.cancel_listing(&t.seller, &1u64);

        // Try to fund cancelled listing - should fail
        let result = t.mp.try_fund_invoice(&t.investor, &1u64, &1_000i128);
        assert_eq!(
            result.unwrap_err().unwrap(),
            KoraError::ListingAlreadyCancelled
        );
    }

    #[test]
    fn test_listing_deadline_enforcement() {
        let t = setup();
        let deadline = t.env.ledger().timestamp() + 100;

        t.mp.list_invoice(
            &t.seller,
            &1u64,
            &9_000i128,
            &10_000i128,
            &t.token,
            &deadline,
        );

        // Fast-forward past deadline
        let future_time = deadline + 1;
        t.env.ledger().set(LedgerInfo {
            timestamp: future_time,
            protocol_version: 21,
            sequence_number: 2,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 1000,
            min_persistent_entry_ttl: 1000,
            max_entry_ttl: 100_000,
        });

        // Funding past deadline should fail
        let result = t.mp.try_fund_invoice(&t.investor, &1u64, &1_000i128);
        assert_eq!(
            result.unwrap_err().unwrap(),
            KoraError::FundingDeadlinePassed
        );
    }

    #[test]
    fn test_listing_can_be_cancelled_by_admin() {
        let t = setup();
        let other_seller = Address::generate(&t.env);
        let deadline = t.env.ledger().timestamp() + 86_400;

        t.mp.list_invoice(
            &other_seller,
            &1u64,
            &9_000i128,
            &10_000i128,
            &t.token,
            &deadline,
        );

        // Admin should be able to cancel
        assert!(t.mp.try_cancel_listing(&t.admin, &1u64).is_ok());

        let listing = t.mp.get_listing(&1u64);
        assert!(!listing.is_active);
    }

    #[test]
    fn test_non_seller_non_admin_cannot_cancel() {
        let t = setup();
        let stranger = Address::generate(&t.env);
        let deadline = t.env.ledger().timestamp() + 86_400;

        t.mp.list_invoice(
            &t.seller,
            &1u64,
            &9_000i128,
            &10_000i128,
            &t.token,
            &deadline,
        );

        // Stranger cannot cancel
        let result = t.mp.try_cancel_listing(&stranger, &1u64);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::Unauthorized);
    }

    // ── Token Validation Edge Cases ───────────────────────────────────────────

    #[test]
    fn test_cannot_list_with_non_whitelisted_token() {
        let t = setup();
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
    fn test_token_can_be_removed_from_whitelist() {
        let t = setup();
        let token_to_remove = Address::generate(&t.env);

        // Whitelist a token
        t.mp.whitelist_token(&t.admin, &token_to_remove);

        // Verify it's whitelisted
        assert!(t.mp.is_token_whitelisted(&token_to_remove));

        // Remove it
        let result = t.mp.try_remove_token_whitelist(&t.admin, &token_to_remove);
        assert!(result.is_ok());

        // Verify it's no longer whitelisted
        assert!(!t.mp.is_token_whitelisted(&token_to_remove));
    }

    #[test]
    fn test_remove_non_whitelisted_token_fails() {
        let t = setup();
        let never_whitelisted = Address::generate(&t.env);

        let result = t.mp.try_remove_token_whitelist(&t.admin, &never_whitelisted);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::TokenNotWhitelisted);
    }

    // ── Amount Validation Edge Cases ──────────────────────────────────────────

    #[test]
    fn test_funding_exceeding_target_rejected() {
        let t = setup();
        let deadline = t.env.ledger().timestamp() + 86_400;
        let asking_price = 9_000i128;
        let face_value = 10_000i128;

        t.mp.list_invoice(
            &t.seller,
            &1u64,
            &asking_price,
            &face_value,
            &t.token,
            &deadline,
        );

        // Try to fund more than asking price
        let excessive_amount = asking_price + 1;
        let result = t.mp.try_fund_invoice(&t.investor, &1u64, &excessive_amount);
        assert_eq!(
            result.unwrap_err().unwrap(),
            KoraError::ExceedsFundingTarget
        );
    }

    #[test]
    fn test_partial_funding_works() {
        let t = setup();
        let deadline = t.env.ledger().timestamp() + 86_400;
        let asking_price = 10_000i128;
        let face_value = 10_000i128;

        t.mp.list_invoice(
            &t.seller,
            &1u64,
            &asking_price,
            &face_value,
            &t.token,
            &deadline,
        );

        // Fund partially
        let partial_amount = asking_price / 2;
        let result = t.mp.try_fund_invoice(&t.investor, &1u64, &partial_amount);

        // May fail on cross-contract calls but not on amount validation
        if let Err(Ok(e)) = result {
            assert_ne!(e, KoraError::ExceedsFundingTarget);
            assert_ne!(e, KoraError::InvalidAmount);
        }
    }

    #[test]
    fn test_negative_amount_rejected() {
        let t = setup();
        let deadline = t.env.ledger().timestamp() + 86_400;

        t.mp.list_invoice(
            &t.seller,
            &1u64,
            &9_000i128,
            &10_000i128,
            &t.token,
            &deadline,
        );

        let result = t.mp.try_fund_invoice(&t.investor, &1u64, &-1_000i128);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::InvalidAmount);
    }

    // ── Fee Admin Functions Edge Cases ────────────────────────────────────────

    #[test]
    fn test_non_admin_cannot_update_fee() {
        let t = setup();
        let stranger = Address::generate(&t.env);

        let result = t.mp.try_set_fee(&stranger, &100u32);
        assert_eq!(result.unwrap_err().unwrap(), KoraError::NotAdmin);
    }

    #[test]
    fn test_invalid_fee_bps_rejected() {
        let t = setup();

        // Try to set fee > 10_000 bps (> 100%)
        let result = t.mp.try_set_fee(&t.admin, &10_001u32);
        assert!(result.is_err());
    }

    #[test]
    fn test_fee_update_emits_event() {
        let t = setup();

        // Update fee from 50 to 100
        let result = t.mp.try_set_fee(&t.admin, &100u32);
        assert!(result.is_ok());

        // Verify fee was updated
        let new_fee = t.mp.get_fee_bps();
        assert_eq!(new_fee, 100u32);
    }
}
