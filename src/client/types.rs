use std::str::FromStr;

use alloy::primitives::{B256, U256};
use chrono::{DateTime, Local};

use crate::TokenIndex;

/// The metadata about some market.
#[derive(Clone, Debug)]
pub struct Market {
    pub market_id: u64,
    pub question: String,
    pub condition_id: B256,
    pub clob_token_ids: [U256; 2],
    pub start_date: DateTime<Local>,
    pub end_date: Option<DateTime<Local>>,
    /// The HALF spread in dollars.
    pub rewards_max_spread: f64,
    pub rewards_min_size: f64,
    pub reward_usdc: Option<u64>,
    pub tick_size: f64,
    pub order_min_size: u64,
    pub neg_risk: bool,
}

impl Market {
    pub fn token_id(&self, token_id: TokenIndex) -> U256 {
        match token_id {
            TokenIndex::Zero => self.clob_token_ids[0],
            TokenIndex::One => self.clob_token_ids[1],
        }
    }

    pub fn token_index(&self, token_id: U256) -> Option<TokenIndex> {
        if self.clob_token_ids[0] == token_id {
            Some(TokenIndex::Zero)
        } else if self.clob_token_ids[1] == token_id {
            Some(TokenIndex::One)
        } else {    
            None
        }
    }
}

pub use api::BookResponse as Book;

/// Note: This impl assumes that the API doesnt fuck up lol, it could panic.
impl From<api::MarketResponse> for Market {
    fn from(response: api::MarketResponse) -> Self {
        Self {
            market_id: response.id.parse().expect("Failed to parse market id"),
            question: response.question,
            condition_id: response
                .condition_id
                .parse()
                .expect("Failed to parse condition id"),
            clob_token_ids: response
                .clob_token_ids
                .into_iter()
                .map(|id| U256::from_str(&id).expect("Failed to parse clob token id"))
                .collect::<Vec<U256>>()
                .try_into()
                .expect("Failed to convert clob token ids to array"),
            start_date: response.start_date.parse().expect("Invalid start date"),
            end_date: response
                .end_date
                .map(|date| date.parse().expect("Invalid end date")),
            rewards_max_spread: response.rewards_max_spread,
            rewards_min_size: response.rewards_min_size,
            reward_usdc: response
                .clob_rewards
                .and_then(|rewards| rewards.first().map(|reward| reward.rewards_daily_rate)),
            tick_size: response.order_price_min_tick_size,
            order_min_size: response.order_min_size,
            neg_risk: response.neg_risk,
        }
    }
}

pub use api::{OrderType, Side};

pub mod api {
    use std::collections::HashMap;
    use uuid::Uuid;

    use alloy::primitives::{Address, B256, U256};
    use serde::{Deserialize, Deserializer, Serialize};
    use serde_json::Value;

    /// The metadata about some market.
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct MarketResponse {
        pub id: String,
        pub closed: bool,
        pub condition_id: String,
        #[serde(default)]
        pub neg_risk: bool,
        #[serde(deserialize_with = "deser_hacks::deserialize_inner_vec")]
        pub clob_token_ids: Vec<String>,
        pub question: String,
        pub end_date: Option<String>,
        pub start_date: String,
        #[serde(default)]
        pub liquidity_clob: f64,
        #[serde(default)]
        pub volume_clob: f64,
        pub rewards_max_spread: f64,
        pub rewards_min_size: f64,
        pub clob_rewards: Option<Vec<ClobRewards>>,
        pub order_price_min_tick_size: f64,
        pub order_min_size: u64,
    }

    /// A users opern orders.
    #[derive(Debug, Deserialize)]
    pub struct OpenOrders {
        pub associate_trades: Vec<Uuid>,
        pub id: String,
        pub price: String,
        pub original_size: String,
        pub size_matched: String,
        pub side: String,
        pub asset_id: String,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ClobRewards {
        pub rewards_daily_rate: u64,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct BookResponse {
        /// The timestamp of the book in milliseconds since the UNIX epoch.
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_u64")]
        pub timestamp: u64,
        #[serde(default)]
        pub bids: Vec<BookEntry>,
        #[serde(default)]
        pub asks: Vec<BookEntry>,
    }

    #[derive(Debug, Deserialize, Clone)]
    #[serde(rename_all = "camelCase")]
    pub struct BookEntry {
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub price: f64,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub size: f64,
    }

    /// A signed order to be submitted to the API.
    #[derive(Debug, Serialize, Clone)]
    #[serde(rename_all = "camelCase")]
    pub struct SignedOrder {
        /// The salt is a random number used to prevent replay attacks
        pub salt: u32,
        /// The maker is the address of the maker of the order
        pub maker: String,
        /// The signer is the address of the signer of the order
        pub signer: String,
        /// The taker is the address of the taker of the order
        pub taker: String,
        /// The token id is the clob token id
        pub token_id: String,
        /// The maker amount scaled by 1e6
        pub maker_amount: String,
        /// The taker amount scaled by 1e6
        pub taker_amount: String,
        /// Unix Timestamp of the expiration time
        pub expiration: String,
        /// The nonce is a random number used to prevent replay attacks
        pub nonce: String,
        /// The fee rate in basis points
        pub fee_rate_bps: String,
        /// The side of the order
        pub side: String,
        /// The signature type
        pub signature_type: u8,
        /// The signature is the signature of the order
        pub signature: String,
    }

    #[derive(Debug, Serialize, Deserialize, Clone, Copy, Hash, Eq, PartialEq)]
    pub enum OrderType {
        GTC,
        FOK,
        GTD,
    }

    impl OrderType {
        pub fn as_str(&self) -> &'static str {
            match self {
                OrderType::GTC => "GTC",
                OrderType::FOK => "FOK",
                OrderType::GTD => "GTD",
            }
        }
    }

    #[derive(Debug, Serialize, Deserialize, Clone, Copy, Hash, Eq, PartialEq)]
    pub enum Side {
        BUY = 0,
        SELL = 1,
    }

    impl Side {
        pub fn opposite(&self) -> Self {
            match self {
                Side::BUY => Side::SELL,
                Side::SELL => Side::BUY,
            }
        }
    }

    impl Side {
        pub fn as_str(&self) -> &'static str {
            match self {
                Side::BUY => "BUY",
                Side::SELL => "SELL",
            }
        }
    }

    #[derive(Debug, Deserialize, Clone)]
    pub struct TotalUserEarning {
        pub date: String,
        pub asset_address: String,
        pub maker_address: String,
        pub earnings: f64,
        pub asset_rate: f64,
    }

    // The book at some timestamp.
    #[derive(Debug, Deserialize, Clone)]
    pub struct MarketUpdate {
        pub timestamp: String,
        pub asset_id: U256,
        pub bids: Vec<BookEntry>,
        pub asks: Vec<BookEntry>,
    }

    /// This is the last trade price for a given asset id.
    #[derive(Debug, Deserialize, Clone)]
    pub struct LastTradePrice {
        pub asset_id: U256,
        pub event_type: String,
        pub fee_rate_bps: String,
        pub market: B256,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub price: f64,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub size: f64,
        pub side: Side,
    }

    /// The new cumulative size at price for a given asset id.
    #[derive(Debug, Deserialize, Clone)]
    pub struct PriceChange {
        pub asset_id: U256,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub price: f64,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub size: f64,
        pub side: Side,
        pub hash: String,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub best_bid: f64,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub best_ask: f64,
    }

    /// Emitted when a order in the book is changed or a new trade is made.
    #[derive(Debug, Deserialize, Clone)]
    pub struct PriceChangeResponse {
        pub market: B256,
        pub price_changes: Vec<PriceChange>,
        /// The timestamp of the price change in milliseconds since the UNIX epoch.
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_u64")]
        pub timestamp: u64,
        pub event_type: String,
    }

    impl PriceChangeResponse {
        pub fn for_asset(&self, asset_id: U256) -> Option<&PriceChange> {
            self.price_changes.iter().find(|pc| pc.asset_id == asset_id)
        }
    }

    #[derive(Clone, Copy, Debug, Deserialize)]
    pub enum OrderStatus {
        MATCHED,
        MINED,
        CONFIRMED,
        RETRYING,
        FAILED,
    }

    /// A users trade has been updated.
    #[derive(Debug, Deserialize, Clone)]
    pub struct TradeStreamItem {
        pub asset_id: U256,
        pub id: Uuid,
        pub last_update: String,
        pub maker_orders: Vec<TradeMakerOrder>,
        pub market: B256,
        pub matchtime: Option<String>,
        pub outcome: String,
        pub owner: String,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub price: f64,
        pub side: Side,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub size: f64,
        pub status: OrderStatus,
        pub taker_order_id: B256,
        pub timestamp: String,
        pub trade_owner: String,
    }

    /// A maker order that has affected somehow.
    #[derive(Debug, Deserialize, Clone)]
    pub struct TradeMakerOrder {
        pub asset_id: U256,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub matched_amount: f64,
        pub order_id: B256,
        pub outcome: String,
        pub owner: String,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub price: f64,
    }

    #[derive(Debug, Deserialize, Clone)]
    pub struct HistoricalTrade {
        pub id: String,
        pub taker_order_id: String,
        pub market: B256,
        pub asset_id: U256,
        pub side: Side,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub size: f64,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub fee_rate_bps: f64,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub price: f64,
        pub status: OrderStatus,
        pub match_time: String,
        pub last_update: String,
        pub outcome: String,
        pub maker_address: String,
        pub owner: String,
        pub transaction_hash: String,
        pub bucket_index: u64,
        pub maker_orders: Vec<TradeMakerOrder>,
        pub trader_side: String,
    }

    /// Emitted when the tick size of an asset is changed.
    #[derive(Debug, Deserialize, Clone)]
    pub struct TickSizeChange {
        pub event_type: String,
        pub asset_id: U256,
        pub market: B256,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub old_tick_size: f64,
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_f64")]
        pub new_tick_size: f64,
    }

    /// The response type when posting an order.
    #[derive(Debug, Deserialize, Clone)]
    #[serde(rename_all = "camelCase")]
    pub struct PostOrderResponse {
        pub success: bool,
        /// The error message if the order failed to post.
        #[serde(deserialize_with = "deser_hacks::deser_empty_string_to_none")]
        pub error_msg: Option<String>,
        /// The id of the order that was posted.
        #[serde(rename = "orderID")]
        #[serde(deserialize_with = "deser_hacks::deser_empty_to_none")]
        pub order_id: Option<B256>,
        /// Any marketable orders that have already been filled.
        #[serde(default)]
        pub order_hash: Vec<String>,
    }

    #[derive(Debug, Deserialize, Clone)]
    pub struct CancelOrdersResponse {
        pub canceled: Vec<B256>,
        pub not_canceled: HashMap<B256, String>,
    }

    #[derive(Debug, Deserialize, Clone)]
    pub struct MarketReward {
        /// The ID of the market in polymarket.
        #[serde(deserialize_with = "deser_hacks::deserialize_string_to_u64")]
        pub market_id: u64,
        /// The condition id of the market.
        pub condition_id: B256,
        /// The question of the market.
        pub question: String,
        /// The volume of the market in the last 24 hours, assuming reset on UTC midnight.
        pub volume_24hr: f64,
        /// The rewards configuration for the market.
        pub rewards_config: Vec<RewardsConfig>,
        /// The maximum spread of the market.
        pub rewards_max_spread: f64,
        /// The minimum size of the market.
        pub rewards_min_size: f64,
        /// The competitiveness of the market, according to polymarket.
        pub market_competitiveness: f64,
    }

    #[derive(Debug, Deserialize, Clone)]
    pub struct RewardsConfig {
        // Havent seen any float values but easier to just parse them assuming they could be.
        pub rate_per_day: f64,
        pub asset_address: Address,
    }

    #[derive(Debug, Deserialize)]
    pub struct PolymarketHistoryEntry {
        pub t: u64,
        pub p: f64,
    }

    #[derive(Debug, Deserialize)]
    pub struct PolymarketHistoryResponse {
        pub history: Vec<PolymarketHistoryEntry>,
    }

    // 1m, 1w, 1d, 6h, 1h, max 
    pub enum HistoricalInterval {
        Minute,
        Hour,
        SixHour,
        Day,
        Week,
        Max,
        /// The start and end times, in UNIX timestamps.
        Custom { start_ts: u64, end_ts: u64 },
    }

    impl HistoricalInterval {
        pub fn as_query_param(&self) -> HashMap<&'static str, String> {
            match self {
                HistoricalInterval::Minute => HashMap::from([("interval", "1m".to_owned())]),
                HistoricalInterval::Hour => HashMap::from([("interval", "1h".to_owned())]),
                HistoricalInterval::SixHour => HashMap::from([("interval", "6h".to_owned())]),
                HistoricalInterval::Day => HashMap::from([("interval", "1d".to_owned())]),
                HistoricalInterval::Week => HashMap::from([("interval", "1w".to_owned())]),
                HistoricalInterval::Max => HashMap::from([("interval", "max".to_owned())]),
                HistoricalInterval::Custom { start_ts, end_ts } => HashMap::from([("startTs", start_ts.to_string()), ("endTs", end_ts.to_string())]),
            }
        }
    }

    mod deser_hacks {
        use std::{fmt::Display, str::FromStr};

        use super::*;

        pub(super) fn deser_empty_string_to_none<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
        where
            D: Deserializer<'de>,
        {
            let v = String::deserialize(deserializer)?;

            if v.is_empty() { Ok(None) } else { Ok(Some(v)) }
        }

        pub(super) fn deser_empty_to_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
        where
            D: Deserializer<'de>,
            T: FromStr<Err: Display>,
        {
            let v = String::deserialize(deserializer)?;

            if v.is_empty() {
                Ok(None)
            } else {
                Ok(Some(
                    T::from_str(&v).map_err(|e| serde::de::Error::custom(e.to_string()))?,
                ))
            }
        }

        pub(super) fn deserialize_inner_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
        where
            D: Deserializer<'de>,
        {
            let v = Value::deserialize(deserializer)?;

            match v {
                // case 1: already a JSON array
                Value::Array(arr) => Ok(arr
                    .into_iter()
                    .map(|x| x.as_str().unwrap_or_default().to_string())
                    .collect()),

                // case 2: string containing a JSON array
                Value::String(s) => {
                    serde_json::from_str::<Vec<String>>(&s).map_err(serde::de::Error::custom)
                }

                _ => Err(serde::de::Error::custom(
                    "expected array or stringified array",
                )),
            }
        }

        pub(super) fn deserialize_string_to_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
        where
            D: Deserializer<'de>,
        {
            let v = String::deserialize(deserializer)?;

            v.parse::<f64>().map_err(serde::de::Error::custom)
        }

        pub(super) fn deserialize_string_to_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
        where
            D: Deserializer<'de>,
        {
            let v = String::deserialize(deserializer)?;

            v.parse::<u64>().map_err(serde::de::Error::custom)
        }
    }
}
