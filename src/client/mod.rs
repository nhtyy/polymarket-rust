pub mod api;
pub use api::APIError;
pub use api::PaginatedStream;

mod ws;
pub use ws::{PolymarketWSClient, WSClientError, market_client, user_client};

pub mod contract;
pub mod order;
pub mod types;

use alloy::{
    primitives::{B256, U256},
    providers::{Provider, WalletProvider},
    signers::Signer,
};
use api::{ClobClient, GammaClient};
use chrono::{DateTime, Utc};
use contract::{
    ConditionalTokens::ConditionalTokensInstance as ConditionalToken, ContractConfig,
    ERC20::ERC20Instance as ERC20, POLYMARKET_PARTITION,
};
use thiserror::Error;

use crate::api::HistoricalInterval;
use crate::api::PolymarketHistoryResponse;
use crate::api::TickSizeChange;
use crate::{
    PolymarketOrder,
    api::{
        CancelOrdersResponse, HistoricalTrade, LastTradePrice, MarketReward, MarketUpdate,
        OpenOrders, PostOrderResponse, PriceChangeResponse, TotalUserEarning, TradeStreamItem,
    },
    client::types::{Market, api::BookResponse},
};

#[derive(Debug, Error)]
pub enum PolymarketClientError {
    #[error("Invalid chain id: {0}")]
    InvalidChain(u64),
    #[error("Failed to get chain id: {0}")]
    RpcError(#[from] alloy::rpc::json_rpc::RpcError<alloy::transports::TransportErrorKind>),
    #[error("Failed to send transaction: {0}")]
    SendTransactionError(#[from] alloy::contract::Error),
    #[error("Failed to confirm transaction: {0}")]
    WatchTransactionError(#[from] alloy::providers::PendingTransactionError),
    #[error("API Error: {0}")]
    ClobClientError(#[from] APIError),
    #[error("WS Error: {0}")]
    WSError(#[from] WSClientError),
    #[error("Provider should be able to sign on behalf of the signer")]
    SignerNotCredentialed,
}

#[derive(Clone)]
pub struct PolymarketClient<P, S> {
    provider: P,
    contract_config: ContractConfig,
    gamma_client: GammaClient,
    clob_client: ClobClient<S>,
}

impl<P: Provider + WalletProvider, S: Signer + Send + Sync> PolymarketClient<P, S> {
    /// Connect to the Polymarket client with a provider and signer.
    /// 
    /// # Errors
    /// 
    /// Returns `PolymarketClientError::SignerNotCredentialed` if the provider does not have a credential for the signer.
    /// Returns `PolymarketClientError::InvalidChain` if the chain id is not supported.
    pub async fn connect_with_provider(provider: P, signer: S) -> Result<Self, PolymarketClientError> {
        if !provider.has_signer_for(&signer.address()) {
            return Err(PolymarketClientError::SignerNotCredentialed);
        }

        let chain_id = provider.get_chain_id().await?;

        let contract_config = ContractConfig::from_chain_id(chain_id)
            .ok_or(PolymarketClientError::InvalidChain(chain_id))?;

        Ok(Self {
            contract_config,
            provider,
            gamma_client: GammaClient::new(),
            clob_client: ClobClient::new(signer).await?,
        })
    }
}

impl<P, S> PolymarketClient<P, S> {
    /// Get a stream of last trade price messages for a given asset id.
    pub async fn last_trade_price_stream(
        &self,
        asset_id: U256,
    ) -> Result<PolymarketWSClient<LastTradePrice>, WSClientError> {
        market_client(asset_id).await
    }

    /// Get a stream of market update messages for a given asset id.
    pub async fn market_update_stream(
        &self,
        asset_id: U256,
    ) -> Result<PolymarketWSClient<MarketUpdate>, WSClientError> {
        market_client(asset_id).await
    }

    /// Get a stream of tick size change messages for a given asset id.
    pub async fn tick_size_stream(
        &self,
        asset_id: U256,
    ) -> Result<PolymarketWSClient<TickSizeChange>, WSClientError> {
        market_client(asset_id).await
    }

    /// Get a stream of price change messages for a given asset id.
    pub async fn price_change_stream(
        &self,
        asset_id: U256,
    ) -> Result<PolymarketWSClient<PriceChangeResponse>, WSClientError> {
        market_client(asset_id).await
    }

    /// Get a stream of trade messages for a given market.
    pub async fn trade_stream(
        &self,
        market: B256,
    ) -> Result<PolymarketWSClient<TradeStreamItem>, WSClientError> {
        user_client(market, self.clob_client.api_creds.clone()).await
    }
}

// Gamma API
impl<P, S> PolymarketClient<P, S> {
    /// Get `limit` latest (open) markets starting from the `offset` index.
    pub async fn get_markets(
        &self,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<Market>, PolymarketClientError> {
        Ok(self
            .gamma_client
            .get_markets(offset, limit)
            .await?
            .into_iter()
            .map(|response| response.into())
            .collect())
    }

    /// Get a single market by its id.
    pub async fn get_market(&self, market_id: u64) -> Result<Market, PolymarketClientError> {
        Ok(self.gamma_client.get_market(market_id).await?.into())
    }
}

impl<P, S> PolymarketClient<P, S> {
    /// Get the dollar denomionated mid price for a given token id.
    pub async fn get_mid_price(&self, token_id: U256) -> Result<f64, PolymarketClientError> {
        Ok(self
            .clob_client
            .get_mid_price(token_id.to_string().as_str())
            .await?)
    }

    /// Get the price history for a given token id.
    pub async fn price_history(
        &self,
        token_id: U256,
        interval: HistoricalInterval,
        fidelity_minutes: u32,
    ) -> Result<PolymarketHistoryResponse, PolymarketClientError> {
        Ok(self
            .clob_client
            .price_history(token_id, interval, fidelity_minutes)
            .await?
        )
    }

    /// Get the order book for a given token id.
    pub async fn get_book(&self, token_id: U256) -> Result<BookResponse, PolymarketClientError> {
        self.clob_client
            .get_book(token_id.to_string().as_str())
            .await
            .map_err(Into::into)
    }

    /// Get a stream of the current market rewards.
    pub fn get_market_rewards(&self) -> PaginatedStream<MarketReward> {
        self.clob_client.get_market_rewards()
    }
}

// CLOB API
impl<P: Provider + WalletProvider, S: Signer + Send + Sync> PolymarketClient<P, S> {
    /// Get a stream of the trades for a given condition id, by the signer.
    pub async fn trades(
        &self,
        condition_id: B256,
    ) -> Result<PaginatedStream<HistoricalTrade>, PolymarketClientError> {
        Ok(self.clob_client.trades(condition_id).await?)
    }

    /// Post a batch of GTC orders to the CLOB.
    pub async fn post_gtc_orders(
        &self,
        market: &Market,
        orders: &[PolymarketOrder],
    ) -> Result<Vec<PostOrderResponse>, PolymarketClientError> {
        // Create a sign the orders.
        let orders = orders.iter().map(|order| {
            let (token_id, side, price, size) = order.into_parts();

            self.clob_client
                .create_gtc_order(token_id, market.neg_risk, price, side, size, market.tick_size)
        });
        let orders = futures::future::try_join_all(orders).await?;

        tracing::debug!("Orders: {:#?}", orders);

        // Batch submit all the orders.
        Ok(self.clob_client.post_orders(&orders).await?)
    }

    /// Post a batch of FOK orders to the CLOB.
    pub async fn post_fok_orders(
        &self,
        market: &Market,
        orders: &[PolymarketOrder],
    ) -> Result<Vec<PostOrderResponse>, PolymarketClientError> {
        // Create a sign the orders.
        let orders = orders.iter().map(|order| {
            let (token_id, side, price, size) = order.into_parts();

            self.clob_client
                .create_fok_order(token_id, market.neg_risk, price, side, size, market.tick_size)
        });
        let orders = futures::future::try_join_all(orders).await?;

        tracing::debug!("Orders: {:#?}", orders);

        // Batch submit all the orders.
        Ok(self.clob_client.post_orders(&orders).await?)
    }

    /// Cancel all open orders for a given token id.
    pub async fn cancel_orders(
        &self,
        token_id: U256,
    ) -> Result<CancelOrdersResponse, PolymarketClientError> {
        Ok(self.clob_client.cancel_orders(token_id).await?)
    }

    /// Get all open orders for a given token id.
    pub async fn get_open_orders(
        &self,
        token_id: U256,
    ) -> Result<PaginatedStream<OpenOrders>, PolymarketClientError> {
        Ok(self.clob_client.get_open_orders(token_id).await?)
    }

    /// Get an open order by its id.
    pub async fn get_order(&self, order_id: B256) -> Result<OpenOrders, PolymarketClientError> {
        Ok(self.clob_client.get_order(order_id).await?)
    }

    /// Check if an order is scoring.
    pub async fn is_order_scoring(&self, order_id: &str) -> Result<bool, PolymarketClientError> {
        Ok(self.clob_client.is_order_scoring(order_id).await?)
    }

    /// Get the total rewards earned for a given date.
    pub async fn get_total_rewards_earned(
        &self,
        date: DateTime<Utc>,
    ) -> Result<Vec<TotalUserEarning>, PolymarketClientError> {
        Ok(self.clob_client.get_total_rewards_earned(date).await?)
    }
}

/// Chain Interactions
impl<P: Provider + WalletProvider, S: Signer + Send + Sync> PolymarketClient<P, S> {
    /// Split `amount` of collateral into `amount` yes and no positions.
    pub async fn split(&self, market: &Market, amount: U256) -> Result<(), PolymarketClientError> {
        let (ctf, collateral) = self.contract_config.get_ctf_and_collateral(market.neg_risk);

        let conditional_token = ConditionalToken::new(ctf, &self.provider);
        let tx = conditional_token.splitPosition(
            collateral,
            B256::ZERO,
            market.condition_id,
            POLYMARKET_PARTITION.to_vec(),
            amount,
        );

        tx.send().await?.watch().await?;

        Ok(())
    }

    /// Redeem the positions for a given market.
    pub async fn redeem(&self, market: &Market) -> Result<(), PolymarketClientError> {
        let (ctf, collateral) = self.contract_config.get_ctf_and_collateral(market.neg_risk);

        let conditional_token = ConditionalToken::new(ctf, &self.provider);
        let tx = conditional_token.redeemPositions(
            collateral,
            B256::ZERO,
            market.condition_id,
            POLYMARKET_PARTITION.to_vec(),
        );

        tx.send().await?.watch().await?;

        Ok(())
    }

    /// Merge the positions for a given market.
    pub async fn merge(&self, market: &Market, amount: U256) -> Result<(), PolymarketClientError> {
        let (ctf, collateral) = self.contract_config.get_ctf_and_collateral(market.neg_risk);

        let conditional_token = ConditionalToken::new(ctf, &self.provider);
        let tx = conditional_token.mergePositions(
            collateral,
            B256::ZERO,
            market.condition_id,
            POLYMARKET_PARTITION.to_vec(),
            amount,
        );

        tx.send().await?.watch().await?;

        Ok(())
    }

    /// Approves [`U256::MAX`] of both collateral and conditional tokens for use by polymarket exchange.
    pub async fn approve_tokens(&self, neg_risk: bool) -> Result<(), PolymarketClientError> {
        let (ctf, collateral) = self.contract_config.get_ctf_and_collateral(neg_risk);
        let exchange = self.contract_config.get_exchange(neg_risk);

        let collateral = ERC20::new(collateral, &self.provider);
        collateral
            .approve(exchange, U256::MAX)
            .send()
            .await?
            .watch()
            .await?;

        collateral
            .approve(ctf, U256::MAX)
            .send()
            .await?
            .watch()
            .await?;

        if neg_risk {
            // Typically we want to interface with the adapter in lieu of the conditional token as it acts kinda like a proxy,
            // but for neg risk not only do we need to approve the exchange but we need to approve the adapter as well.
            let conditional_token = ConditionalToken::new(
                self.contract_config.neg_risk_conditional_tokens,
                &self.provider,
            );

            // Approve the exchange to spend the conditional tokens.
            conditional_token
                .setApprovalForAll(exchange, true)
                .send()
                .await?
                .watch()
                .await?;

            // Approve the adapter to spend the conditional tokens.
            conditional_token
                .setApprovalForAll(ctf, true)
                .send()
                .await?
                .watch()
                .await?;
        } else {
            let conditional_token = ConditionalToken::new(ctf, &self.provider);

            // Approve the exchange to spend the conditional tokens.
            conditional_token
                .setApprovalForAll(exchange, true)
                .send()
                .await?
                .watch()
                .await?;
        }

        Ok(())
    }

    /// Get the total collateral balance for the signer of this client.
    pub async fn collateral_balance(&self) -> Result<U256, PolymarketClientError> {
        // Note that neg_risk and !neg_risk have the same collateral address.
        let neg_risk = false;
        let (_, collateral) = self.contract_config.get_ctf_and_collateral(neg_risk);

        let collateral_contract = ERC20::new(collateral, &self.provider);

        Ok(collateral_contract
            .balanceOf(self.provider.default_signer_address())
            .call()
            .await?)
    }

    /// Get the balance of a given position id.
    pub async fn ctf_balance(
        &self,
        position_id: U256,
        neg_risk: bool,
    ) -> Result<U256, PolymarketClientError> {
        let (ctf, _) = self.contract_config.get_ctf_and_collateral(neg_risk);
        let conditional_token = ConditionalToken::new(ctf, &self.provider);

        Ok(conditional_token
            .balanceOf(self.provider.default_signer_address(), position_id)
            .call()
            .await?)
    }
}