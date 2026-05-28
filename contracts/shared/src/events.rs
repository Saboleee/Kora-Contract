use soroban_sdk::{Address, Env, Symbol, symbol_short};

fn emit(env: &Env, name: Symbol, data: impl soroban_sdk::IntoVal<Env, soroban_sdk::Val>) {
    env.events().publish((name,), data);
}

// ── Invoice Events ──────────────────────────────────────────────────────────

pub fn invoice_created(env: &Env, invoice_id: u64, sme: &Address, amount: i128) {
    emit(env, symbol_short!("INV_CRT"), (invoice_id, sme.clone(), amount));
}

pub fn invoice_listed(env: &Env, invoice_id: u64, seller: &Address, asking_price: i128) {
    emit(env, symbol_short!("INV_LST"), (invoice_id, seller.clone(), asking_price));
}

pub fn invoice_funded(env: &Env, invoice_id: u64, investor: &Address, amount: i128) {
    emit(env, symbol_short!("INV_FND"), (invoice_id, investor.clone(), amount));
}

pub fn invoice_repaid(env: &Env, invoice_id: u64, sme: &Address, amount: i128) {
    emit(env, symbol_short!("INV_RPD"), (invoice_id, sme.clone(), amount));
}

pub fn invoice_defaulted(env: &Env, invoice_id: u64, sme: &Address) {
    emit(env, symbol_short!("INV_DFT"), (invoice_id, sme.clone()));
}

// ── Repayment Events ────────────────────────────────────────────────────────

pub fn repayment_made(env: &Env, invoice_id: u64, payer: &Address, amount: i128) {
    emit(env, symbol_short!("REPAY"), (invoice_id, payer.clone(), amount));
}

pub fn yield_distributed(env: &Env, invoice_id: u64, investor: &Address, yield_amount: i128) {
    emit(env, symbol_short!("YIELD"), (invoice_id, investor.clone(), yield_amount));
}

// ── Marketplace Events ──────────────────────────────────────────────────────

pub fn listing_cancelled(env: &Env, invoice_id: u64, seller: &Address) {
    emit(env, symbol_short!("LST_CXL"), (invoice_id, seller.clone()));
}

pub fn fee_collected(env: &Env, invoice_id: u64, fee_amount: i128, token: &Address) {
    emit(env, symbol_short!("FEE_COL"), (invoice_id, fee_amount, token.clone()));
}

// ── Protocol Events ────────────────────────────────────────────────────────

pub fn protocol_paused(env: &Env, by: &Address) {
    emit(env, symbol_short!("PAUSED"), by.clone());
}

pub fn protocol_unpaused(env: &Env, by: &Address) {
    emit(env, symbol_short!("UNPAUSED"), by.clone());
}

pub fn fee_withdrawn(env: &Env, token: &Address, amount: i128) {
    emit(env, symbol_short!("FEE_WTH"), (token.clone(), amount));
}
