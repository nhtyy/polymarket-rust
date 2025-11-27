use crate::client::contract::{ContractConfig, Order, sign_order_message};
use crate::f64_to_u256;

use super::api::APIError;
use super::types::api::{OrderType, Side, SignedOrder as APISignedOrder};
use alloy::primitives::{Address, U256};
use alloy::signers::Signer;
use rand::Rng;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RoundConfig {
    price: u8,
    size: u8,
    amount: u8,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedOrder {
    pub order: APISignedOrder,
    pub owner: String,
    pub order_type: OrderType,
    pub defer_exec: bool,
}

pub enum OrderParams {
    Amounts {
        maker_amount: U256,
        taker_amount: U256,
        side: Side,
    },
    Price {
        size: U256,
        price: f64,
        side: Side,
    },
}

/// Note: This method populates the following fields as defaults:
/// - nonce
/// - taker
/// - signature type as eoa
/// - fee rate as 0
#[allow(clippy::too_many_arguments)]
pub async fn create_signed_order<S: Signer + Sync>(
    signer: &S,
    chain_id: u64,
    neg_risk: bool,
    clob_token_id: U256,
    price: f64,
    side: Side,
    size: f64,
    order_type: OrderType,
    tick_size: f64,
    expiration_offset_seconds: u64,
) -> Result<APISignedOrder, APIError> {
    let (maker_amount, taker_amount) = if matches!(order_type, OrderType::FOK) {
        get_market_order_maker_taker_amounts(price, side, size, tick_size)
    } else {
        get_maker_taker_amounts(price, side, size, tick_size)
    };
    
    let maker = signer.address();
    let expiration = if expiration_offset_seconds > 0 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        now + expiration_offset_seconds
    } else {
        0
    };

    // ?.
    let nonce = 0;
    // no fee yet
    let fee_rate_bps = 0;
    // eoa = 0
    let signature_type = 0;

    let salt: u32 = rand::rng().random();
    let order = Order {
        salt: U256::from(salt),
        maker,
        signer: maker,
        taker: Address::ZERO,
        tokenId: clob_token_id,
        makerAmount: maker_amount,
        takerAmount: taker_amount,
        expiration: U256::from(expiration),
        nonce: U256::from(nonce),
        feeRateBps: U256::from(fee_rate_bps),
        side: side as u8,
        signatureType: signature_type as u8,
    };
    let chain_config =
        ContractConfig::from_chain_id(chain_id).ok_or(APIError::InvalidChain(chain_id))?;
    let signature =
        sign_order_message(signer, order, chain_id, chain_config.get_exchange(neg_risk)).await?;

    let api_signed_order = APISignedOrder {
        salt: salt,
        maker: maker.to_checksum(None),
        signer: signer.address().to_checksum(None),
        taker: Address::ZERO.to_checksum(None),
        token_id: clob_token_id.to_string(),
        maker_amount: maker_amount.to_string(),
        taker_amount: taker_amount.to_string(),
        expiration: expiration.to_string(),
        nonce: nonce.to_string(),
        fee_rate_bps: fee_rate_bps.to_string(),
        side: side.as_str().to_string(),
        signature_type: 0,
        signature: format!("0x{}", alloy::primitives::hex::encode(signature.as_bytes())),
    };

    Ok(api_signed_order)
}

fn get_maker_taker_amounts(price: f64, side: Side, size: f64, tick_size: f64) -> (U256, U256) {
    let config = round_config(tick_size).expect("Got invalid tick size, no round config known.");

    // Ensure the price is rounded to the nearest tick.
    let price = round(price, config.price);
    
    match side {
        Side::BUY => {
            let taker_amount = round(size, config.size);

            let maker_amount = taker_amount * price;
            let maker_amount = round_down(maker_amount, config.amount);

            (f64_to_u256(maker_amount), f64_to_u256(taker_amount))
        }
        Side::SELL => {
            let maker_amount = round(size, config.size);

            let taker_amount = maker_amount * price;
            let taker_amount = round_down(taker_amount, config.amount);

            (f64_to_u256(maker_amount), f64_to_u256(taker_amount))
        }
    }
}

fn get_market_order_maker_taker_amounts(price: f64, side: Side, size: f64, tick_size: f64) -> (U256, U256) {
    let config = round_config(tick_size).expect("Got invalid tick size, no round config known.");

    let price = round(price, config.price);

    match side {
        Side::BUY => {
            let maker_amount = size * price;
            let maker_amount = round_up(maker_amount, config.size);

            let taker_amount = size;
            let taker_amount = round(taker_amount, config.amount);

            (f64_to_u256(maker_amount), f64_to_u256(taker_amount))
        }
        Side::SELL => {
            let maker_amount = size;
            let maker_amount = round(maker_amount, config.size);

            let taker_amount = size * price;
            let taker_amount = round_down(taker_amount, config.amount);

            (f64_to_u256(maker_amount), f64_to_u256(taker_amount))
        }
    }
}

fn round_up(amount: f64, decimals: u8) -> f64 {
    let precision = 10_f64.powi(decimals as i32);
    (amount * precision).ceil() / precision
}

fn round_down(amount: f64, decimals: u8) -> f64 {
    let precision = 10_f64.powi(decimals as i32);
    (amount * precision).floor() / precision
}

fn round(amount: f64, decimals: u8) -> f64 {
    let precision = 10_f64.powi(decimals as i32);
    (amount * precision).round() / precision
}

const fn round_config(tick_size: f64) -> Option<RoundConfig> {
    let tick_config = match tick_size {
        0.1 => RoundConfig {
            price: 1,
            size: 2,
            amount: 3,
        },
        0.01 => RoundConfig {
            price: 2,
            size: 2,
            amount: 4,
        },
        0.001 => RoundConfig {
            price: 3,
            size: 2,
            amount: 5,
        },
        0.0001 => RoundConfig {
            price: 4,
            size: 2,
            amount: 6,
        },
        _ => return None,
    };

    Some(tick_config)
}