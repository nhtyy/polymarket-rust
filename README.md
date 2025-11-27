# Rust Polymarket Client 

This library supports the following operations
- Submitting orders to a market
- (Multiplexed) websocket reads
- Some functionality from the Gamma client
- Some functionality from the CLOB client.
- On chain interationsL: split, merge, redeem.
    - Neg risk also supported

### This library implicity is designed around using an EOA, not the default Safe wallet deployed by the Polymarket UI. Note that positions held by an EOA do not appear on the UI.

## Example

```rust

let market_id = ...;

// Setup the client with an Alloy provider.

let rpc_url = std::env::var("RPC_URL").expect("RPC_URL must be set");
let signer_private_key = std::env::var("PRIVATE_KEY").expect("SIGNER_PRIVATE_KEY must be set");
let signer = LocalSigner::from_str(&signer_private_key).expect("Failed to parse private key");
let provider = ProviderBuilder::new()
    .wallet(signer.clone())
    .connect(&rpc_url)
    .await
    .expect("Failed to connect to provider");

let client = PolymarketClient::connect_with_provider(provider, signer)
    .await
    .expect("Failed to connect to Polymarket");

// Get the market metadata

let market = client
        .get_market(market_id)
        .await
        .expect("Failed to get market");

// Get balances for a given market.

let inventory = client
    .ctf_balance(market.clob_token_ids[0], market.neg_risk)
    .await
    .expect("Failed to get inventory");

let complement_inventory = client
    .ctf_balance(market.clob_token_ids[1], market.neg_risk)
    .await
    .expect("Failed to get complement inventory");

let usdc_balance = client
    .collateral_balance()
    .await
    .expect("Failed to get usdc balance");

// Merge as many tokens as possible

let amount_to_merge = inventory.min(complement_inventory);

if amount_to_merge.is_zero() {
    println!("No inventory to merge");
    return;
}

println!(
    "Merging inventory: {}",
    format_units(amount_to_merge, 6).expect("Failed to format amount to merge")
);
client
    .merge(&market, amount_to_merge)
    .await
    .expect("Failed to merge inventory");

// Place a limit order 

let order = PolymarketOrder::Bid {
    token_id: market.clob_token_ids[0],
    size: 0.1
    price: 0.5 
};

client.post_gtc_orders(&market, &[order]).await.expect("Failed to place order");

```