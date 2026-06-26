#![no_std]

use kora_shared::errors::KoraError;
use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, Symbol};

const MAX_STALENESS_SECS: u64 = 3600;

#[contracttype]
#[derive(Clone, Debug)]
pub struct PriceData {
    pub price: i128,
    pub timestamp: u64,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Price(Symbol, Symbol),
}

#[contract]
pub struct PriceOracleContract;

#[contractimpl]
impl PriceOracleContract {
    pub fn initialize(env: Env, admin: Address) -> Result<(), KoraError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(KoraError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        Ok(())
    }

    /// Set a price for a currency pair. Admin only.
    /// Price is expressed as `base` units per 1 unit of `quote`, scaled by 1e7 (stroops).
    pub fn set_price(
        env: Env,
        admin: Address,
        base: Symbol,
        quote: Symbol,
        price: i128,
    ) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;

        if price <= 0 {
            return Err(KoraError::InvalidAmount);
        }

        let data = PriceData {
            price,
            timestamp: env.ledger().timestamp(),
        };
        env.storage()
            .persistent()
            .set(&DataKey::Price(base, quote), &data);
        Ok(())
    }

    /// Get the price for a pair. Returns the price and its timestamp.
    /// Fails if the price is stale (older than MAX_STALENESS_SECS) or missing.
    pub fn get_price(
        env: Env,
        base: Symbol,
        quote: Symbol,
    ) -> Result<PriceData, KoraError> {
        let data: PriceData = env
            .storage()
            .persistent()
            .get(&DataKey::Price(base.clone(), quote.clone()))
            .ok_or(KoraError::InvalidAmount)?;

        let age = env
            .ledger()
            .timestamp()
            .saturating_sub(data.timestamp);
        if age > MAX_STALENESS_SECS {
            return Err(KoraError::InvoiceExpired);
        }

        Ok(data)
    }

    /// Convert an amount from one currency to another using the stored price.
    /// Rejects stale or missing prices.
    pub fn convert(
        env: Env,
        amount: i128,
        from: Symbol,
        to: Symbol,
    ) -> Result<i128, KoraError> {
        if from == to {
            return Ok(amount);
        }

        let price_data = Self::get_price(env.clone(), from, to)?;
        let converted = amount
            .checked_mul(price_data.price)
            .and_then(|v| v.checked_div(10_000_000))
            .ok_or(KoraError::ArithmeticOverflow)?;

        if converted <= 0 {
            return Err(KoraError::InvalidAmount);
        }

        Ok(converted)
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

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Env, Symbol};

    fn setup() -> (Env, Address, PriceOracleContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, PriceOracleContract);
        let client = PriceOracleContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        client.initialize(&admin);
        (env, admin, client)
    }

    #[test]
    fn test_set_and_get_price() {
        let (env, admin, client) = setup();
        let base = Symbol::new(&env, "EURC");
        let quote = Symbol::new(&env, "USDC");
        client.set_price(&admin, &base, &quote, &11_000_000i128);
        let data = client.get_price(&base, &quote);
        assert_eq!(data.price, 11_000_000i128);
    }

    #[test]
    fn test_convert_same_currency() {
        let (env, _admin, client) = setup();
        let sym = Symbol::new(&env, "USDC");
        let result = client.convert(&1_000_000i128, &sym, &sym);
        assert_eq!(result, 1_000_000i128);
    }

    #[test]
    fn test_convert_different_currency() {
        let (env, admin, client) = setup();
        let eurc = Symbol::new(&env, "EURC");
        let usdc = Symbol::new(&env, "USDC");
        // 1 EURC = 1.1 USDC (11_000_000 stroops per 10_000_000)
        client.set_price(&admin, &eurc, &usdc, &11_000_000i128);
        let result = client.convert(&10_000_000i128, &eurc, &usdc);
        assert_eq!(result, 11_000_000i128);
    }

    #[test]
    fn test_get_price_missing_fails() {
        let (env, _admin, client) = setup();
        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");
        let result = client.try_get_price(&base, &quote);
        assert!(result.is_err());
    }

    #[test]
    fn test_stale_price_rejected() {
        use soroban_sdk::testutils::{Ledger, LedgerInfo};
        let (env, admin, client) = setup();
        let base = Symbol::new(&env, "EURC");
        let quote = Symbol::new(&env, "USDC");
        client.set_price(&admin, &base, &quote, &11_000_000i128);

        env.ledger().set(LedgerInfo {
            timestamp: env.ledger().timestamp() + MAX_STALENESS_SECS + 1,
            protocol_version: 21,
            sequence_number: 2,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 1000,
            min_persistent_entry_ttl: 1000,
            max_entry_ttl: 100_000,
        });

        let result = client.try_get_price(&base, &quote);
        assert!(result.is_err());
    }
}
