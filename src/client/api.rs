use std::collections::HashMap;
use std::time::Duration;

use crate::OrderType;
use crate::api::{
    CancelOrdersResponse, HistoricalInterval, OpenOrders, PolymarketHistoryResponse,
    PostOrderResponse,
};
use crate::client::contract::sign_clob_auth_message;
use crate::client::order::create_signed_order;
use crate::client::types::Side;

use super::order::SignedOrder;
use super::types::api::{
    BookResponse, HistoricalTrade, MarketResponse, MarketReward, TotalUserEarning,
};
use alloy::hex::encode_prefixed;
use alloy::primitives::{B256, U256};
use alloy::signers::Signer;
use base64::{Engine, engine::general_purpose::URL_SAFE};
use chrono::{DateTime, Utc};
use futures::Stream;
use hmac::{Hmac, Mac};
use rand::Rng;
use reqwest::{Client, IntoUrl, RequestBuilder, StatusCode, Url};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::Sha256;
use thiserror::Error;
use tokio::task::JoinHandle;

use std::pin::Pin;
use std::task::{Context, Poll, ready};

const GAMMA_URL: &str = "https://gamma-api.polymarket.com";
const CLOB_URL: &str = "https://clob.polymarket.com";
const POLYMARKET_API_URL: &str = "https://polymarket.com/api/";

const END_CURSOR: &str = "LTE=";
const START_CURSOR: &str = "MA==";

const START_RETRY_DELAY_SECS: f64 = 1.0;
const NUM_RETRIES: u32 = 3;

type Headers = HashMap<&'static str, String>;

#[derive(Debug, Error)]
pub enum APIError {
    #[error("HTTP error: {0}")]
    HTTPError(#[from] reqwest::Error),
    #[error("Error status code: {0}")]
    ErrorStatusCode(StatusCode),
    #[error("Invalid Payload: {0}")]
    SerdeError(#[from] serde_json::Error),
    #[error("Signing error: {0}")]
    SigningError(#[from] alloy::signers::Error),
    #[error("Invalid chain: {0}")]
    InvalidChain(u64),
    #[error("Can't decode secret to base64")]
    Base64DecodeError(#[from] base64::DecodeError),
    #[error("Failed to post orders: {0:?}")]
    PostOrdersError(Vec<PostOrderResponse>),
    #[error("Failed to cancel orders: {0:?}")]
    CancelOrdersError(HashMap<B256, String>),
    #[error("Failed to poll in flight task: {0}")]
    PollInFlightError(#[from] tokio::task::JoinError),
}

#[derive(Clone)]
pub struct ClobClient<S> {
    signer: S,
    http_client: ErrorDebugClient,
    url: Url,
    chain_id: u64,
    pub api_creds: ApiCreds,
}

impl<S: Signer + Send + Sync> ClobClient<S> {
    pub async fn new(signer: S) -> Result<Self, APIError> {
        let http_client = ErrorDebugClient::new(Client::new());
        let url = Url::parse(CLOB_URL).expect("A valid clob url");

        let api_creds = match Self::get_api_creds(&http_client, &signer, &url).await {
            Ok(api_creds) => api_creds,
            Err(APIError::ErrorStatusCode(StatusCode::BAD_REQUEST)) => {
                let api_creds = Self::derive_api_creds(&http_client, &signer, &url).await?;
                api_creds
            }
            Err(e) => return Err(e),
        };

        Ok(Self {
            signer: signer,
            http_client: ErrorDebugClient::new(Client::new()),
            url: Url::parse(CLOB_URL).expect("A valid clob url"),
            chain_id: 137,
            api_creds: api_creds,
        })
    }

    pub fn with_chain_id(self, chain_id: u64) -> Self {
        Self { chain_id, ..self }
    }

    /// Return the first 500 open orders for a given token id.
    pub async fn get_open_orders(
        &self,
        token_id: U256,
    ) -> Result<PaginatedStream<OpenOrders>, APIError> {
        let mut headers =
            create_l2_headers::<_, ()>(&self.signer, &self.api_creds, "GET", "/data/orders", None)
                .await?;

        headers.insert("asset_id", token_id.to_string());

        let url = self.url.join("data/orders").expect("A valid url");

        Ok(PaginatedStream::new(
            self.http_client.clone(),
            url,
            headers
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        ))
    }

    /// Return the first 500 open orders for a given token id.
    pub async fn get_order(&self, order_id: B256) -> Result<OpenOrders, APIError> {
        let req_path = format!("/data/order/{}", order_id);
        let l2_headers =
            create_l2_headers::<_, ()>(&self.signer, &self.api_creds, "GET", &req_path, None)
                .await?;

        let response = self
            .http_client
            .get(self.url.join(&req_path).expect("A valid url"))
            .headers(l2_headers)
            .send()
            .await?;

        Ok(response)
    }

    pub async fn is_order_scoring(&self, order_id: &str) -> Result<bool, APIError> {
        #[derive(Deserialize)]
        struct Response {
            scoring: bool,
        }

        let headers = create_l2_headers::<_, ()>(
            &self.signer,
            &self.api_creds,
            "GET",
            "/order-scoring",
            None,
        )
        .await?;

        let response: Response = self
            .http_client
            .get(self.url.join("order-scoring").expect("A valid url"))
            .headers(headers)
            .query("order_id", order_id)
            .send()
            .await?;

        Ok(response.scoring)
    }

    pub async fn get_total_rewards_earned(
        &self,
        date: DateTime<Utc>,
    ) -> Result<Vec<TotalUserEarning>, APIError> {
        let l2_headers = create_l2_headers::<_, ()>(
            &self.signer,
            &self.api_creds,
            "GET",
            "/rewards/user/total",
            None,
        )
        .await?;

        let date = date.format("%Y-%m-%d").to_string();
        let response = self
            .http_client
            .get(self.url.join("rewards/user/total").expect("A valid url"))
            .headers(l2_headers)
            .query("date", date)
            // todo: note this only works for eoa
            .query("signature_type", "0")
            .send()
            .await?;

        Ok(response)
    }

    pub async fn create_gtc_order(
        &self,
        clob_token_id: U256,
        neg_risk: bool,
        price: f64,
        side: Side,
        size: f64,
        tick_size: f64,
    ) -> Result<SignedOrder, APIError> {
        let order_type = OrderType::GTC;

        let order = create_signed_order(
            &self.signer,
            self.chain_id,
            neg_risk,
            clob_token_id,
            price,
            side,
            size,
            order_type,
            tick_size,
            0,
        )
        .await?;

        Ok(SignedOrder {
            order_type: order_type,
            owner: self.api_key(),
            order: order,
            defer_exec: false,
        })
    }

    pub async fn create_fok_order(
        &self,
        clob_token_id: U256,
        neg_risk: bool,
        price: f64,
        side: Side,
        size: f64,
        tick_size: f64,
    ) -> Result<SignedOrder, APIError> {
        let order_type = OrderType::FOK;

        let order = create_signed_order(
            &self.signer,
            self.chain_id,
            neg_risk,
            clob_token_id,
            price,
            side,
            size,
            order_type,
            tick_size,
            0,
        )
        .await?;

        Ok(SignedOrder {
            order_type: order_type,
            owner: self.api_key(),
            order: order,
            defer_exec: false,
        })
    }

    pub async fn post_orders(
        &self,
        order: &[SignedOrder],
    ) -> Result<Vec<PostOrderResponse>, APIError> {
        let l2_headers = create_l2_headers(
            &self.signer,
            &self.api_creds,
            "POST",
            "/orders",
            Some(order),
        )
        .await?;

        let response: Vec<PostOrderResponse> = self
            .http_client
            .post(self.url.join("orders").expect("A valid url"))
            .headers(l2_headers)
            .body(&order)
            .send()
            .await?;

        tracing::debug!("Posted orders response: {:#?}", response);

        if response
            .iter()
            .any(|order| !order.success || order.error_msg.is_some())
        {
            return Err(APIError::PostOrdersError(response));
        }

        Ok(response)
    }

    pub async fn cancel_orders(&self, token_id: U256) -> Result<CancelOrdersResponse, APIError> {
        #[derive(Serialize)]
        struct Body {
            asset_id: String,
        }

        let body = Body {
            asset_id: token_id.to_string(),
        };

        let l2_headers: HashMap<&'static str, String> = create_l2_headers(
            &self.signer,
            &self.api_creds,
            "DELETE",
            "/cancel-market-orders",
            Some(&body),
        )
        .await?;

        let response: CancelOrdersResponse = self
            .http_client
            .delete(self.url.join("cancel-market-orders").expect("A valid url"))
            .headers(l2_headers)
            .body(&body)
            .send()
            .await?;

        Ok(response)
    }

    pub async fn trades(
        &self,
        condition_id: B256,
    ) -> Result<PaginatedStream<HistoricalTrade>, APIError> {
        let url = self.url.join("data/trades/").expect("A valid url");

        let mut headers =
            create_l2_headers::<_, ()>(&self.signer, &self.api_creds, "GET", "/data/trades", None)
                .await?;
        headers.insert("condition_id", condition_id.to_string());

        Ok(PaginatedStream::new(
            self.http_client.clone(),
            url,
            headers
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        ))
    }

    async fn get_api_creds(
        client: &ErrorDebugClient,
        signer: &S,
        url: &Url,
    ) -> Result<ApiCreds, APIError> {
        let l1_headers = create_l1_headers(signer, None).await?;

        let response = client
            .post(url.join("auth/api-key").expect("A valid url"))
            .headers(l1_headers)
            .send()
            .await?;

        Ok(response)
    }

    async fn derive_api_creds(
        client: &ErrorDebugClient,
        signer: &S,
        url: &Url,
    ) -> Result<ApiCreds, APIError> {
        let l1_headers = create_l1_headers(signer, None).await?;

        let response = client
            .get(url.join("auth/derive-api-key").expect("A valid url"))
            .headers(l1_headers)
            .send()
            .await?;

        Ok(response)
    }

    fn api_key(&self) -> String {
        self.api_creds.api_key.clone()
    }
}

impl<S> ClobClient<S> {
    pub async fn price_history(
        &self,
        token_id: U256,
        interval: HistoricalInterval,
        fidelity_minutes: u32,
    ) -> Result<PolymarketHistoryResponse, APIError> {
        let url = self.url.join("prices-history").expect("A valid url");

        let req = self
            .http_client
            .get(url)
            .extend_query(interval.as_query_param())
            .query("fidelity", fidelity_minutes.to_string())
            .query("market", token_id.to_string());

        Ok(req.send().await?)
    }

    pub fn get_market_rewards(&self) -> PaginatedStream<MarketReward> {
        let url = Url::parse(POLYMARKET_API_URL)
            .expect("A valid url")
            .join("rewards/markets")
            .expect("A valid url");

        PaginatedStream::new(self.http_client.clone(), url, HashMap::new())
    }

    pub async fn get_book(&self, token_id: &str) -> Result<BookResponse, APIError> {
        let url = self.url.join("book").expect("A valid url");

        let response = self
            .http_client
            .get(url)
            .query("token_id", token_id)
            .send()
            .await?;

        Ok(response)
    }

    pub async fn get_mid_price(&self, token_id: &str) -> Result<f64, APIError> {
        #[derive(Deserialize)]
        struct Response {
            mid: String,
        }

        let url = self.url.join("midpoint").expect("A valid url");

        let response: Response = self
            .http_client
            .get(url)
            .query("token_id", token_id)
            .send()
            .await?;

        Ok(response.mid.parse().expect("Failed to parse mid price"))
    }
}

#[derive(Clone)]
pub struct GammaClient {
    http_client: ErrorDebugClient,
    url: Url,
}

impl GammaClient {
    pub fn new() -> Self {
        Self {
            http_client: ErrorDebugClient::new(Client::new()),
            url: Url::parse(GAMMA_URL).expect("A valid gamma url"),
        }
    }

    /// Get the `limit` latest (open) markets starting from the `offset` index.
    pub async fn get_markets(
        &self,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<MarketResponse>, APIError> {
        let url = self.url.join("markets").expect("A valid url");

        let response = self
            .http_client
            .get(url)
            .query("ascending", "false")
            .query("limit", limit.to_string())
            .query("offset", offset.to_string())
            .query("order", "startDate")
            .query("closed", "false")
            .send()
            .await?;

        Ok(response)
    }

    pub async fn get_market(&self, market_id: u64) -> Result<MarketResponse, APIError> {
        let url = self
            .url
            .join(&format!("markets/{}", market_id))
            .expect("A valid url");

        let response = self.http_client.get(url).send().await?;

        Ok(response)
    }
}

#[derive(Clone)]
pub struct ErrorDebugClient {
    http_client: Client,
}

impl ErrorDebugClient {
    pub fn new(http_client: Client) -> Self {
        Self { http_client }
    }

    pub fn get<T: IntoUrl>(&self, url: T) -> ErrorDebugRequest {
        ErrorDebugRequest::new(self.http_client.get(url))
    }

    pub fn post<T: IntoUrl>(&self, url: T) -> ErrorDebugRequest {
        ErrorDebugRequest::new(self.http_client.post(url))
    }

    pub fn delete<T: IntoUrl>(&self, url: T) -> ErrorDebugRequest {
        ErrorDebugRequest::new(self.http_client.delete(url))
    }
}

pub struct ErrorDebugRequest {
    req: RequestBuilder,
}

impl ErrorDebugRequest {
    pub fn new(req: RequestBuilder) -> Self {
        Self { req }
    }

    fn query<K, V>(self, query: K, value: V) -> Self
    where
        K: AsRef<str>,
        V: AsRef<str>,
    {
        Self {
            req: self.req.query(&[(query.as_ref(), value.as_ref())]),
        }
    }

    fn extend_query<K, V>(self, query: HashMap<K, V>) -> Self
    where
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let as_vec = query
            .iter()
            .map(|(k, v)| (k.as_ref(), v.as_ref()))
            .collect::<Vec<_>>();

        Self {
            req: self.req.query(&as_vec),
        }
    }

    fn headers(self, headers: Headers) -> Self {
        Self {
            req: headers
                .into_iter()
                .fold(self.req, |req, (key, value)| req.header(key, value)),
        }
    }

    fn body<T: Serialize>(self, body: &T) -> Self {
        Self {
            req: self.req.json(body),
        }
    }

    pub async fn send<T: DeserializeOwned>(self) -> Result<T, APIError> {
        let mut delay = Duration::from_secs_f64(START_RETRY_DELAY_SECS);

        for i in 0..NUM_RETRIES {
            if let Some(clone) = self.req.try_clone() {
                match Self::send_request(clone).await {
                    Ok(response) => return Ok(response),
                    Err(APIError::HTTPError(http_error)) => {
                        tracing::trace!("API Client got error, {:?}", http_error);

                        let jitter = rand::rng().random_range(0.8..1.2);

                        // If we have a retryable error, we should retry.
                        if http_error.is_timeout()
                            || http_error.is_connect()
                            || http_error.is_request()
                        {
                            tokio::time::sleep(delay.mul_f64(jitter)).await;
                            delay = delay.mul_f64(START_RETRY_DELAY_SECS.powf(i as f64 + 1.0));

                            if i == NUM_RETRIES - 1 {
                                return Err(APIError::HTTPError(http_error));
                            }
                        } else {
                            return Err(APIError::HTTPError(http_error));
                        }
                    }
                    Err(APIError::ErrorStatusCode(StatusCode::TOO_MANY_REQUESTS)) => {
                        tracing::trace!("API Client got error, too many requests");

                        let jitter = rand::rng().random_range(0.8..1.2);

                        tokio::time::sleep(delay.mul_f64(jitter)).await;
                        delay = delay.mul_f64(START_RETRY_DELAY_SECS.powf(i as f64 + 1.0));

                        if i == NUM_RETRIES - 1 {
                            return Err(APIError::ErrorStatusCode(StatusCode::TOO_MANY_REQUESTS));
                        }
                    }
                    Err(e) => {
                        return Err(e);
                    }
                }
            } else {
                // The inner request cant be cloned, so we should just return the error, if any.
                return Self::send_request(self.req).await;
            }
        }

        unreachable!("Loop should have returned before this point");
    }

    async fn send_request<T: DeserializeOwned>(req: RequestBuilder) -> Result<T, APIError> {
        let raw = req.send().await?;

        let status = raw.status();

        let body = raw.text().await?;
        if !status.is_success() {
            tracing::error!("Failed to send request: {}", body);

            return Err(APIError::ErrorStatusCode(status));
        }

        match serde_json::from_str(&body) {
            Ok(response) => Ok(response),
            Err(e) => {
                tracing::error!("Failed to parse response: {} \n Body: {}", e, body);
                Err(APIError::SerdeError(e))
            }
        }
    }
}

const POLY_ADDR_HEADER: &str = "poly_address";
const POLY_SIG_HEADER: &str = "poly_signature";
const POLY_TS_HEADER: &str = "poly_timestamp";
const POLY_NONCE_HEADER: &str = "poly_nonce";
const POLY_API_KEY_HEADER: &str = "poly_api_key";
const POLY_PASS_HEADER: &str = "poly_passphrase";

pub async fn create_l1_headers<S: Signer + Sync>(
    signer: &S,
    nonce: Option<U256>,
) -> Result<Headers, APIError> {
    let timestamp = crate::get_current_unix_time_secs().to_string();
    let nonce = nonce.unwrap_or(U256::ZERO);

    let signature = sign_clob_auth_message(signer, timestamp.clone(), nonce).await?;
    let signature = format!("0x{}", alloy::primitives::hex::encode(signature.as_bytes()));
    let address = signer.address().to_checksum(None);

    Ok(HashMap::from([
        (POLY_ADDR_HEADER, address),
        (POLY_SIG_HEADER, signature),
        (POLY_TS_HEADER, timestamp),
        (POLY_NONCE_HEADER, nonce.to_string()),
    ]))
}

pub async fn create_l2_headers<S, T>(
    signer: &S,
    api_creds: &ApiCreds,
    method: &str,
    req_path: &str,
    body: Option<&T>,
) -> Result<Headers, APIError>
where
    S: Signer + Sync,
    T: ?Sized + Serialize,
{
    let address = encode_prefixed(signer.address().as_slice());
    let timestamp = crate::get_current_unix_time_secs();

    let hmac_signature =
        build_hmac_signature(&api_creds.secret, timestamp, method, req_path, body)?;

    Ok(HashMap::from([
        (POLY_ADDR_HEADER, address),
        (POLY_SIG_HEADER, hmac_signature),
        (POLY_TS_HEADER, timestamp.to_string()),
        (POLY_API_KEY_HEADER, api_creds.api_key.clone()),
        (POLY_PASS_HEADER, api_creds.passphrase.clone()),
    ]))
}

pub fn build_hmac_signature<T>(
    secret: &str,
    timestamp: u64,
    method: &str,
    req_path: &str,
    body: Option<&T>,
) -> Result<String, APIError>
where
    T: ?Sized + Serialize,
{
    let decoded = URL_SAFE.decode(secret)?;

    let message = match body {
        None => format!("{timestamp}{method}{req_path}"),
        Some(s) => {
            let s = serde_json::to_string(s)?;
            format!("{timestamp}{method}{req_path}{s}")
        }
    };

    let mut mac = Hmac::<Sha256>::new_from_slice(&decoded).expect("HMAC init error");
    mac.update(message.as_bytes());

    let result = mac.finalize();

    Ok(URL_SAFE.encode(&result.into_bytes()[..]))
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ApiCreds {
    #[serde(rename = "apiKey")]
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct PaginatedResponse<T> {
    data: Vec<T>,
    next_cursor: String,
}

/// A stream of paginated responses from the API,
/// the next page is eagerly fetched.
pub struct PaginatedStream<T> {
    client: ErrorDebugClient,
    url: Url,
    query: HashMap<String, String>,
    buffer: Vec<T>,
    in_flight: JoinHandle<Result<PaginatedResponse<T>, APIError>>,
    done: bool,
}

impl<T: DeserializeOwned + Send + 'static> PaginatedStream<T> {
    pub fn new(client: ErrorDebugClient, url: Url, query: HashMap<String, String>) -> Self {
        let fut = Self::get_fut(&client, &url, &query, START_CURSOR.to_string());

        Self {
            client,
            url,
            query,
            buffer: Vec::new(),
            in_flight: fut,
            done: false,
        }
    }

    fn get_fut(
        client: &ErrorDebugClient,
        url: &Url,
        query: &HashMap<String, String>,
        next_cursor: String,
    ) -> JoinHandle<Result<PaginatedResponse<T>, APIError>> {
        let req = client
            .get(url.clone())
            .extend_query(query.clone())
            .query("nextCursor", next_cursor);

        tokio::task::spawn(req.send())
    }
}

impl<T: DeserializeOwned + Send + 'static> Stream for PaginatedStream<T> {
    type Item = Result<T, APIError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Safety: We dont move any fields so we can safely get a mutable reference to the stream.
        let this = unsafe { self.get_unchecked_mut() };

        match this.buffer.pop() {
            Some(item) => Poll::Ready(Some(Ok(item))),
            None => {
                if this.done {
                    return Poll::Ready(None);
                }

                let pinned = Pin::new(&mut this.in_flight);
                let response = ready!(pinned.poll(cx))??;
                // Incase the response is empty, we are done.
                if response.data.is_empty() {
                    this.done = true;
                    return Poll::Ready(None);
                }

                // Add the data to the buffer.
                this.buffer.extend(response.data);

                // If we have reached the end, we are done.
                if response.next_cursor == END_CURSOR {
                    this.done = true;
                } else {
                    this.in_flight =
                        Self::get_fut(&this.client, &this.url, &this.query, response.next_cursor);
                }

                // Safety: We know that the buffer is not empty as its checked above.
                Poll::Ready(Some(Ok(unsafe { this.buffer.pop().unwrap_unchecked() })))
            }
        }
    }
}
