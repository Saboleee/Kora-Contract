/// Integration test harness for the Kora Protocol.
///
/// Each test spins up a full mock environment with all contracts deployed
/// and wired together, mirroring a real Stellar Soroban deployment.
#[cfg(test)]
mod integration {
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        Address, Bytes, Env, String, Symbol,
    };

    use kora_access_control::{AccessControlContract, AccessControlContractClient};
    use kora_financing_pool::{FinancingPoolContract, FinancingPoolContractClient};
    use kora_invoice_nft::{InvoiceNftContract, InvoiceNftContractClient};
    use kora_marketplace::{MarketplaceContract, MarketplaceContractClient};
    use kora_risk_registry::{RiskRegistryContract, RiskRegistryContractClient};
    use kora_shared::types::InvoiceStatus;
    use kora_treasury::{TreasuryContract, TreasuryContractClient};

    // ── Test Environment ──────────────────────────────────────────────────────

    struct KoraEnv<'a> {
        env: Env,
        admin: Address,
        access_control: AccessControlContractClient<'a>,
        invoice_nft: InvoiceNftContractClient<'a>,
        marketplace: MarketplaceContractClient<'a>,
        pool: FinancingPoolContractClient<'a>,
        treasury: TreasuryContractClient<'a>,
        risk_registry: RiskRegistryContractClient<'a>,
    }

    fn deploy_protocol() -> KoraEnv<'static> {
        let env = Env::default();
        env.mock_all_auths();

        // Set a realistic starting timestamp
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

        // Register all contracts
        let ac_id = env.register_contract(None, AccessControlContract);
        let nft_id = env.register_contract(None, InvoiceNftContract);
        let mp_id = env.register_contract(None, MarketplaceContract);
        let pool_id = env.register_contract(None, FinancingPoolContract);
        let treasury_id = env.register_contract(None, TreasuryContract);
        let rr_id = env.register_contract(None, RiskRegistryContract);

        let ac = AccessControlContractClient::new(&env, &ac_id);
        let nft = InvoiceNftContractClient::new(&env, &nft_id);
        let mp = MarketplaceContractClient::new(&env, &mp_id);
        let pool = FinancingPoolContractClient::new(&env, &pool_id);
        let treasury = TreasuryContractClient::new(&env, &treasury_id);
        let rr = RiskRegistryContractClient::new(&env, &rr_id);

        // Initialize all contracts
        ac.initialize(&admin);
        nft.initialize(&admin, &ac_id);
        mp.initialize(&admin, &nft_id, &pool_id, &treasury_id, &50u32);
        pool.initialize(&admin, &nft_id, &treasury_id, &200u32);
        treasury.initialize(&admin, &50u32);
        rr.initialize(&admin, &nft_id);

        KoraEnv {
            env,
            admin,
            access_control: ac,
            invoice_nft: nft,
            marketplace: mp,
            pool,
            treasury,
            risk_registry: rr,
        }
    }

    fn sample_invoice_params(env: &Env) -> (Bytes, i128, Symbol, u64, String, u32) {
        let debtor_hash = Bytes::from_slice(env, &[0xABu8; 32]);
        let amount = 10_000_000_000i128; // 10,000 USDC (7 decimals)
        let currency = Symbol::new(env, "USDC");
        let due_date = env.ledger().timestamp() + 86_400 * 60; // 60 days
        let ipfs_cid = String::from_str(
            env,
            "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi",
        );
        let risk_score = 30u32;
        (
            debtor_hash,
            amount,
            currency,
            due_date,
            ipfs_cid,
            risk_score,
        )
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// Full happy path: mint → list → fund → repay
    #[test]
    fn test_full_invoice_lifecycle() {
        let k = deploy_protocol();
        let sme = Address::generate(&k.env);
        let (debtor_hash, amount, currency, due_date, ipfs_cid, risk_score) =
            sample_invoice_params(&k.env);

        // 1. Mint invoice NFT
        let invoice_id = k.invoice_nft.mint_invoice(
            &sme,
            &debtor_hash,
            &amount,
            &currency,
            &due_date,
            &ipfs_cid,
            &risk_score,
        );
        assert_eq!(invoice_id, 1);

        let invoice = k.invoice_nft.get_invoice(&invoice_id);
        assert_eq!(invoice.status, InvoiceStatus::Created);

        // 2. Transition to Listed (simulating marketplace call)
        k.invoice_nft
            .set_listed(&k.marketplace.address, &invoice_id);
        assert_eq!(
            k.invoice_nft.get_invoice(&invoice_id).status,
            InvoiceStatus::Listed
        );

        // 3. Transition to Funded (simulating pool call)
        k.invoice_nft.set_funded(&k.pool.address, &invoice_id);
        assert_eq!(
            k.invoice_nft.get_invoice(&invoice_id).status,
            InvoiceStatus::Funded
        );

        // 4. Repay (simulating pool repay call)
        k.invoice_nft.set_repaid(&k.pool.address, &invoice_id);
        assert_eq!(
            k.invoice_nft.get_invoice(&invoice_id).status,
            InvoiceStatus::Repaid
        );
    }

    /// Minting with zero amount must fail.
    #[test]
    fn test_mint_zero_amount_rejected() {
        let k = deploy_protocol();
        let sme = Address::generate(&k.env);
        let (debtor_hash, _, currency, due_date, ipfs_cid, risk_score) =
            sample_invoice_params(&k.env);

        let result = k.invoice_nft.try_mint_invoice(
            &sme,
            &debtor_hash,
            &0i128,
            &currency,
            &due_date,
            &ipfs_cid,
            &risk_score,
        );
        assert!(result.is_err());
    }

    /// Due date in the past must be rejected.
    #[test]
    fn test_mint_past_due_date_rejected() {
        let k = deploy_protocol();
        let sme = Address::generate(&k.env);
        let (debtor_hash, amount, currency, _, ipfs_cid, risk_score) =
            sample_invoice_params(&k.env);

        let past = k.env.ledger().timestamp() - 1;
        let result = k.invoice_nft.try_mint_invoice(
            &sme,
            &debtor_hash,
            &amount,
            &currency,
            &past,
            &ipfs_cid,
            &risk_score,
        );
        assert!(result.is_err());
    }

    /// Risk score above 100 must be rejected.
    #[test]
    fn test_mint_invalid_risk_score_rejected() {
        let k = deploy_protocol();
        let sme = Address::generate(&k.env);
        let (debtor_hash, amount, currency, due_date, ipfs_cid, _) = sample_invoice_params(&k.env);

        let result = k.invoice_nft.try_mint_invoice(
            &sme,
            &debtor_hash,
            &amount,
            &currency,
            &due_date,
            &ipfs_cid,
            &101u32,
        );
        assert!(result.is_err());
    }

    /// Invalid status transition must be rejected.
    #[test]
    fn test_invalid_status_transition_rejected() {
        let k = deploy_protocol();
        let sme = Address::generate(&k.env);
        let (debtor_hash, amount, currency, due_date, ipfs_cid, risk_score) =
            sample_invoice_params(&k.env);

        let id = k.invoice_nft.mint_invoice(
            &sme,
            &debtor_hash,
            &amount,
            &currency,
            &due_date,
            &ipfs_cid,
            &risk_score,
        );

        // Cannot go Created → Funded (must go through Listed first)
        let result = k.invoice_nft.try_set_funded(&k.pool.address, &id);
        assert!(result.is_err());
    }

    /// Protocol pause/unpause flow.
    #[test]
    fn test_pause_unpause_protocol() {
        let k = deploy_protocol();
        assert!(!k.access_control.is_paused());

        k.access_control.pause(&k.admin);
        assert!(k.access_control.is_paused());

        k.access_control.unpause(&k.admin);
        assert!(!k.access_control.is_paused());
    }

    /// Non-admin cannot pause.
    #[test]
    fn test_non_admin_cannot_pause() {
        let k = deploy_protocol();
        let stranger = Address::generate(&k.env);
        let result = k.access_control.try_pause(&stranger);
        assert!(result.is_err());
    }

    /// SME registration and risk scoring flow.
    #[test]
    fn test_sme_registration_flow() {
        let k = deploy_protocol();
        let verifier = Address::generate(&k.env);
        let sme = Address::generate(&k.env);

        k.risk_registry.add_verifier(&k.admin, &verifier);
        assert!(k.risk_registry.is_verifier(&verifier));

        k.risk_registry.register_sme(&verifier, &sme, &40u32);
        assert!(k.risk_registry.is_verified_sme(&sme));

        let profile = k.risk_registry.get_sme_profile(&sme);
        assert_eq!(profile.risk_score, 40);
        assert_eq!(profile.total_invoices, 0);
        assert_eq!(profile.defaults, 0);
    }

    /// Unregistered verifier cannot register SME.
    #[test]
    fn test_unregistered_verifier_rejected() {
        let k = deploy_protocol();
        let fake_verifier = Address::generate(&k.env);
        let sme = Address::generate(&k.env);

        let result = k
            .risk_registry
            .try_register_sme(&fake_verifier, &sme, &10u32);
        assert!(result.is_err());
    }

    /// Treasury fee configuration.
    #[test]
    fn test_treasury_fee_management() {
        let k = deploy_protocol();
        assert_eq!(k.treasury.get_fee_bps(), 50);

        k.treasury.set_fee_bps(&k.admin, &100u32);
        assert_eq!(k.treasury.get_fee_bps(), 100);
    }

    /// Fee above 100% must be rejected.
    #[test]
    fn test_treasury_fee_above_max_rejected() {
        let k = deploy_protocol();
        let result = k.treasury.try_set_fee_bps(&k.admin, &10_001u32);
        assert!(result.is_err());
    }

    /// Admin transfer flow.
    #[test]
    fn test_admin_transfer() {
        let k = deploy_protocol();
        let new_admin = Address::generate(&k.env);

        k.access_control.transfer_admin(&k.admin, &new_admin);
        assert_eq!(k.access_control.get_admin(), new_admin);
    }

    /// Defaulting an invoice before due date must fail.
    #[test]
    fn test_cannot_default_before_due_date() {
        let k = deploy_protocol();
        let sme = Address::generate(&k.env);
        let (debtor_hash, amount, currency, due_date, ipfs_cid, risk_score) =
            sample_invoice_params(&k.env);

        let id = k.invoice_nft.mint_invoice(
            &sme,
            &debtor_hash,
            &amount,
            &currency,
            &due_date,
            &ipfs_cid,
            &risk_score,
        );

        // Transition to Funded state
        k.invoice_nft.set_listed(&k.marketplace.address, &id);
        k.invoice_nft.set_funded(&k.pool.address, &id);

        // Due date has not passed — default should fail
        let result = k.invoice_nft.try_set_defaulted(&k.admin, &id);
        assert!(result.is_err());
    }

    /// Defaulting after due date succeeds.
    #[test]
    fn test_default_after_due_date_succeeds() {
        let k = deploy_protocol();
        let sme = Address::generate(&k.env);
        let (debtor_hash, amount, currency, due_date, ipfs_cid, risk_score) =
            sample_invoice_params(&k.env);

        let id = k.invoice_nft.mint_invoice(
            &sme,
            &debtor_hash,
            &amount,
            &currency,
            &due_date,
            &ipfs_cid,
            &risk_score,
        );

        k.invoice_nft.set_listed(&k.marketplace.address, &id);
        k.invoice_nft.set_funded(&k.pool.address, &id);

        // Advance ledger past due date
        k.env.ledger().set(LedgerInfo {
            timestamp: due_date + 1,
            protocol_version: 21,
            sequence_number: 100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 1000,
            min_persistent_entry_ttl: 1000,
            max_entry_ttl: 100_000,
        });

        k.invoice_nft.set_defaulted(&k.admin, &id);
        assert_eq!(
            k.invoice_nft.get_invoice(&id).status,
            InvoiceStatus::Defaulted
        );
    }

    /// Sequential invoice IDs are assigned correctly.
    #[test]
    fn test_sequential_invoice_ids() {
        let k = deploy_protocol();
        let sme = Address::generate(&k.env);
        let (debtor_hash, amount, currency, due_date, ipfs_cid, risk_score) =
            sample_invoice_params(&k.env);

        let id1 = k.invoice_nft.mint_invoice(
            &sme,
            &debtor_hash,
            &amount,
            &currency,
            &due_date,
            &ipfs_cid,
            &risk_score,
        );
        let id2 = k.invoice_nft.mint_invoice(
            &sme,
            &debtor_hash,
            &amount,
            &currency,
            &due_date,
            &ipfs_cid,
            &risk_score,
        );
        let id3 = k.invoice_nft.mint_invoice(
            &sme,
            &debtor_hash,
            &amount,
            &currency,
            &due_date,
            &ipfs_cid,
            &risk_score,
        );

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
        assert_eq!(k.invoice_nft.next_id(), 4);
    }
}
