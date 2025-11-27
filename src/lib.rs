use chrono::{Datelike, TimeDelta, TimeZone, Utc};

pub mod client;
pub use client::{PolymarketClient, PolymarketClientError, api::APIError, types::*};

use alloy::primitives::U256;
use num_traits::ToPrimitive;

const TOKEN_SCALE: f64 = 1e6;

#[derive(Debug, Clone, Copy)]
pub enum PolymarketOrder {
    Bid {
        token_id: U256,
        price: f64,
        size: f64,
    },
    Ask {
        token_id: U256,
        price: f64,
        size: f64,
    },
}

impl PolymarketOrder {
    pub fn into_parts(self) -> (U256, Side, f64, f64) {
        match self {
            PolymarketOrder::Bid {
                token_id,
                price,
                size,
            } => (token_id, Side::BUY, price, size),
            PolymarketOrder::Ask {
                token_id,
                price,
                size,
            } => (token_id, Side::SELL, price, size),
        }
    }
}

pub enum Rounding {
    Up,
    Down,
}

/// Converts a U256 scaled by 1e6 to an f64
pub fn u256_to_f64(value: U256) -> f64 {
    let float = value.to_f64().expect("Failed to convert U256 to f64");

    float / TOKEN_SCALE
}

/// Converts an f64 to a U256 scaled by 1e6
pub fn f64_to_u256(value: f64) -> U256 {
    let scaled = value * TOKEN_SCALE;

    U256::from(scaled as u64)
}

pub fn round_to_tick(price: f64, tick_size: f64, rounding: Rounding) -> f64 {
    assert!(tick_size > 0.0, "Tick size must be greater than 0.0");

    let ticks = price / tick_size;
    let rounded = match rounding {
        Rounding::Down => ticks.floor(),
        Rounding::Up => ticks.ceil(),
    };

    let computed = rounded * tick_size;
    computed.clamp(tick_size, 1.0 - tick_size)
}

pub fn timestamp_of_tomorrow_at_12am_utc() -> u64 {
    let now = chrono::Utc::now();
    let today_midnight = Utc
        .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
        .unwrap();

    let next_midnight = if now < today_midnight {
        today_midnight.timestamp()
    } else {
        (today_midnight + TimeDelta::days(1)).timestamp()
    };

    next_midnight as u64
}

fn get_current_unix_time_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
