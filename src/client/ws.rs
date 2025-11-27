use std::{
    collections::{HashMap, hash_map::Entry},
    marker::PhantomData,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, ready},
    time::Duration,
};

use crate::api::{PriceChangeResponse, TickSizeChange, TradeStreamItem};

use super::api::ApiCreds;
use super::types::api::{LastTradePrice, MarketUpdate};
use alloy::primitives::{B256, U256};
use futures::{SinkExt, Stream, StreamExt};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tungstenite::{Message, Utf8Bytes};

const WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com";
const MULTIPLEXER_CAPACITY: usize = 100;
const PING_DURATION: Duration = Duration::from_millis(100);

type WSStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type BroadcastMessage = Option<Result<Utf8Bytes, WSClientError>>;

lazy_static::lazy_static! {
    static ref MULTIPLEXER: Mutex<HashMap<Channel, Multiplexer>> = Mutex::new(HashMap::new());
}

/// An error type thats cheap to disseminate.
#[derive(Clone, Debug, Error)]
pub enum WSClientError {
    #[error("Failed to connect to WS: {0}")]
    ConnectError(Arc<String>),
    #[error("Failed to send message: {0}")]
    SendError(Arc<String>),
    #[error("Failed to receive message: {0}")]
    RecvError(Arc<String>),
    #[error("Failed to deserialize message: {0}")]
    SerializeError(Arc<String>),
    #[error("Bad message from client")]
    TooManyMalformedMessages,
}

/// A client that can be used to recieve messages of type [`T`] from the websocket, with authentication.
pub async fn user_client<T: PolymarketWSAuthedMessage>(
    market: B256,
    auth: ApiCreds,
) -> Result<PolymarketWSClient<T>, WSClientError> {
    let channel = Channel::User(market);

    let mut lock = MULTIPLEXER
        .lock()
        .expect("Failed to lock MULTIPLEXERS");

    match lock.entry(channel) {
        Entry::Occupied(entry) => {
            // A multiplexer already exists for this market, return the existing client.
            return Ok(PolymarketWSClient {
                rx: BroadcastStream::new(entry.get().broadcast.subscribe()),
                channel,
                _phantom: PhantomData,
            });
        }
        Entry::Vacant(entry) => {
            let (broadcast_rx, multiplexer) = start_multiplexer(channel, Some(auth));
            entry.insert(multiplexer);

            Ok(PolymarketWSClient {
                rx: BroadcastStream::new(broadcast_rx),
                channel,
                _phantom: PhantomData,
            })
        }
    }
}

/// A client that can be used to recieve messages of type [`T`] from the websocket, without authentication.
pub async fn market_client<T: PolymarketWSMessage>(
    asset_id: U256,
) -> Result<PolymarketWSClient<T>, WSClientError> {
    debug_assert!(
        T::EVENT_TYPE == "book"
            || T::EVENT_TYPE == "last_trade_price"
            || T::EVENT_TYPE == "price_change"
            || T::EVENT_TYPE == "tick_size_change",
        "Invalid event type for market client: {}",
        T::EVENT_TYPE
    );

    let channel = Channel::Market(asset_id);

    let mut lock = MULTIPLEXER
        .lock()
        .expect("Failed to lock MULTIPLEXERS");

    match lock.entry(channel) {
        Entry::Occupied(entry) => Ok(PolymarketWSClient {
            rx: BroadcastStream::new(entry.get().broadcast.subscribe()),
            channel,
            _phantom: PhantomData,
        }),
        Entry::Vacant(entry) => {
            let (broadcast_rx, multiplexer) = start_multiplexer(channel, None);
            entry.insert(multiplexer);

            Ok(PolymarketWSClient {
                rx: BroadcastStream::new(broadcast_rx),
                channel,
                _phantom: PhantomData,
            })
        }
    }
}

/// A client that can be used to recieve messages of type [`T`] from the websocket.
pub struct PolymarketWSClient<T> {
    rx: BroadcastStream<BroadcastMessage>,
    channel: Channel,
    _phantom: PhantomData<T>,
}

impl<'a, T: Send + PolymarketWSMessage> PolymarketWSClient<T> {
    pub fn recv(&'a mut self) -> PolymarketWSFuture<'a, T> {
        PolymarketWSFuture {
            rx: &mut self.rx,
            channel: &self.channel,
            phantom: PhantomData,
        }
    }

    pub fn into_stream(self) -> impl Stream<Item = Result<T, WSClientError>> + Send {
        futures::stream::unfold(self, |mut client| async move {
            let result = client.recv().await;

            Some((result, client))
        })
    }
}

fn start_multiplexer(
    channel: Channel,
    auth: Option<ApiCreds>,
) -> (broadcast::Receiver<BroadcastMessage>, Multiplexer) {
    tracing::trace!("Starting multiplexer for channel: {:?}", channel);
    
    let (broadcast_tx, broadcast_rx) = broadcast::channel(MULTIPLEXER_CAPACITY);

    let cloned_sender = broadcast_tx.clone();
    tokio::spawn(async move {
        // Start the client and do the initial handshake.
        let mut client = match connect_with_retry(channel, &auth).await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Failed to connect with retry: {:?}", e);

                let _ = cloned_sender.send(Some(Err(e)));

                return;
            }
        };

        let mut tick = tokio::time::interval(PING_DURATION);
        loop {
            tokio::select! {
                msg = client.next() => {
                    match msg {
                        Some(Ok(message)) => {
                            match message {
                                Message::Text(text) => {
                                    let _ = cloned_sender.send(Some(Ok(text)));
                                }
                                Message::Pong(_) => {
                                    // Do nothing.
                                }
                                _ => {
                                    tracing::trace!("Received unexpected message type");
                                }
                            }
                        }
                        Some(Err(e)) => {
                            tracing::trace!("Got an error reading from the websocket: {:?}", e);
                        }
                        None => {
                            tracing::trace!("Got an error reading from the websocket, attmepting to reconnect...");

                            match connect_with_retry(channel, &auth).await {
                                Ok(new_client) => {
                                    client = new_client;
                                },
                                Err(e) => {
                                    // If for some reason we still cant connect, lets propagate the error to the subscribers!.
                                    tracing::trace!("Failed to connect with retry: {:?}", e);
                                    let _ = cloned_sender.send(Some(Err(e)));
                                }
                            };
                        }
                    }
                }
                _ = tick.tick() => {
                    if let Err(e) = client.send(Message::Ping("ping".into())).await {
                        tracing::trace!("Failed to send ping: {:?}", e);
                    }
                }
                _ = cloned_sender.closed() => {
                    tracing::trace!("Broadcast closed, exiting loop...");
                    
                    let mut lock = MULTIPLEXER.lock().expect("Failed to lock MULTIPLEXER");

                    // Now that were holding the lock, lets make sure no new connections have started since we last checked.
                    let multiplexer = lock.get(&channel).expect("Channel not found in multiplexer");
                    if multiplexer.broadcast.receiver_count() == 0 {
                        lock.remove(&channel);
                        break;
                    }
                }
            }
        }
    });

    (
        broadcast_rx,
        Multiplexer {
            broadcast: broadcast_tx,
        },
    )
}

async fn do_handshake(
    client: &mut WSStream,
    channel: Channel,
    auth: &Option<ApiCreds>,
) -> Result<(), WSClientError> {
    let handshake = channel.into_handshake_message(auth);

    let handshake = serde_json::to_string(&handshake).expect("Failed to serialize handshake");

    client
        .send(Message::Text(handshake.into()))
        .await
        .map_err(WSClientError::send_error)?;

    Ok(())
}

async fn connect_with_retry(
    channel: Channel,
    auth: &Option<ApiCreds>,
) -> Result<WSStream, WSClientError> {
    const START_DELAY: Duration = Duration::from_secs(1);
    const MAX_DELAY: Duration = Duration::from_secs(30);

    let mut delay = START_DELAY;
    let mut client = loop {
        match connect_async(channel.url()).await {
            Ok((client, _)) => break client,
            Err(e) => {
                delay = delay.mul_f32(2.0).min(MAX_DELAY);
                if delay == MAX_DELAY {
                    return Err(WSClientError::connect_error(e));
                }

                tracing::trace!("Failed to do handshake: {:?} \n retrying in {:?}", e, delay);
                tokio::time::sleep(delay).await;
            }
        }
    };

    do_handshake(&mut client, channel, &auth).await?;

    Ok(client)
}

/// A future that expects a text message from the websocket and deserializes it into a `T`.
pub struct PolymarketWSFuture<'a, T> {
    rx: &'a mut BroadcastStream<BroadcastMessage>,
    channel: &'a Channel,
    phantom: PhantomData<T>,
}

impl<'a, T: PolymarketWSMessage> Future for PolymarketWSFuture<'a, T> {
    type Output = Result<T, WSClientError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Saftey: We dont move any fields so we can safely get a mutable reference to the stream.
        let this: &mut PolymarketWSFuture<'a, T> = unsafe { self.get_unchecked_mut() };

        loop {
            let msg = ready!(this.rx.poll_next_unpin(cx));

            // We unwrap here as the broadcast channel should never close.
            let msg = match msg.expect("Multiplexer broadcast sender dropped") {
                Ok(msg) => msg,
                // Lagged, we can just ignore any messages.
                Err(_) => {
                    tracing::trace!(
                        "Multiplexer broadcast sender lagged for type: {:?}, ignoring message...",
                        T::EVENT_TYPE
                    );
                    continue;
                }
            };

            match msg {
                Some(Ok(message)) => {
                    #[derive(Deserialize)]
                    struct EventType<'a> {
                        #[serde(borrow)]
                        event_type: &'a str,
                    }

                    let Ok(header): Result<EventType, _> = serde_json::from_str(&message) else {
                        tracing::trace!("Failed to deserialize message from polymarket websocket");
                        continue;
                    };

                    // If this is not the expected event type, we ignore the message for this reciever.
                    if header.event_type != T::EVENT_TYPE {
                        continue;
                    }

                    // Deserialize the message into the expected type.
                    let message: T = serde_json::from_str(&message)?;

                    // If the message does not contain the expected ids, we ignore the message for this reciever.
                    if !message.contains_id(this.channel) {
                        continue;
                    }

                    // We have a valid message, return it to the caller.
                    return Poll::Ready(Ok(message));
                }
                // Error from ws.
                Some(Err(e)) => return Poll::Ready(Err(e.into())),
                // Stream is closed, this also probably should never happen.
                None => unreachable!("WS multiplexer closed, this should never happen."),
            }
        }
    }
}

struct Multiplexer {
    broadcast: broadcast::Sender<BroadcastMessage>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum Channel {
    /// A market channel, used to subscribe to messages for a specific asset id.
    Market(U256),
    /// A user channel, used to subscribe to messages for a specific market.
    /// 
    /// In other words user message will contain messages for both a token and its complement token.
    User(B256),
}

impl Channel {
    fn url(&self) -> String {
        let type_of = self.type_of();

        format!("{}/ws/{}", WS_URL, type_of)
    }

    fn type_of(&self) -> &'static str {
        match self {
            Channel::Market(_) => "market",
            Channel::User(_) => "user",
        }
    }

    fn into_handshake_message(self, auth: &Option<ApiCreds>) -> HandshakeMessage {
        match self {
            Channel::Market(asset_id) => HandshakeMessage {
                markets: None,
                assets_ids: Some(vec![asset_id.to_string()]),
                sub_type: self.type_of().to_string(),
                auth: auth.clone(),
            },
            Channel::User(market) => HandshakeMessage {
                markets: Some(vec![market.to_string()]),
                assets_ids: None,
                sub_type: self.type_of().to_string(),
                auth: auth.clone(),
            },
        }
    }
}

impl From<serde_json::Error> for WSClientError {
    fn from(e: serde_json::Error) -> Self {
        WSClientError::serialize_error(e)
    }
}

impl WSClientError {
    pub fn connect_error(e: tungstenite::Error) -> Self {
        WSClientError::ConnectError(Arc::new(e.to_string()))
    }

    pub fn send_error(e: tungstenite::Error) -> Self {
        WSClientError::SendError(Arc::new(e.to_string()))
    }

    pub fn recv_error(e: tungstenite::Error) -> Self {
        WSClientError::RecvError(Arc::new(e.to_string()))
    }

    pub fn serialize_error(e: serde_json::Error) -> Self {
        WSClientError::SerializeError(Arc::new(e.to_string()))
    }

    pub fn too_many_malformed_messages() -> Self {
        WSClientError::TooManyMalformedMessages
    }
}

#[derive(Serialize)]
struct HandshakeMessage {
    /// The markets we want to subscribe to.
    markets: Option<Vec<String>>,
    /// The asset ids we want to subscribe to.
    assets_ids: Option<Vec<String>>,
    /// The type of subscription we want to subscribe to.
    #[serde(rename = "type")]
    sub_type: String,
    /// The authentication credentials for the user.
    auth: Option<ApiCreds>,
}

pub trait PolymarketWSMessage: DeserializeOwned {
    const EVENT_TYPE: &'static str;

    /// Check if client should accept the message.
    fn contains_id(&self, channel: &Channel) -> bool;
}

/// A market trait for messages that require authentication.
pub trait PolymarketWSAuthedMessage: PolymarketWSMessage {}

impl PolymarketWSMessage for LastTradePrice {
    const EVENT_TYPE: &'static str = "last_trade_price";

    fn contains_id(&self, channel: &Channel) -> bool {
        match channel {
            Channel::Market(asset) => &self.asset_id == asset,
            Channel::User(_) => false,
        }
    }
}

impl PolymarketWSMessage for MarketUpdate {
    const EVENT_TYPE: &'static str = "book";

    fn contains_id(&self, channel: &Channel) -> bool {
        match channel {
            Channel::Market(asset) => &self.asset_id == asset,
            Channel::User(_) => false,
        }
    }
}

impl PolymarketWSMessage for PriceChangeResponse {
    const EVENT_TYPE: &'static str = "price_change";

    fn contains_id(&self, channel: &Channel) -> bool {
        match channel {
            Channel::Market(asset) => self
                .price_changes
                .iter()
                .any(|price_change| &price_change.asset_id == asset),
            Channel::User(_) => false,
        }
    }
}

impl PolymarketWSMessage for TickSizeChange {
    const EVENT_TYPE: &'static str = "tick_size_change";

    fn contains_id(&self, channel: &Channel) -> bool {
        match channel {
            Channel::Market(asset) => &self.asset_id == asset,
            Channel::User(_) => false,
        }
    }
}

impl PolymarketWSMessage for TradeStreamItem {
    const EVENT_TYPE: &'static str = "trade";

    fn contains_id(&self, channel: &Channel) -> bool {
        match channel {
            Channel::Market(_) => false,
            Channel::User(market) => &self.market == market,
        }
    }
}

impl PolymarketWSAuthedMessage for TradeStreamItem {}
