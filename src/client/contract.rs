use alloy::primitives::{Address, U256, address};
use alloy::signers::{Signature, Signer};
use alloy::sol;
use alloy::sol_types::eip712_domain;

pub const POLYMARKET_PARTITION: [U256; 2] = [
    U256::from_be_bytes({
        let mut arr = [0; 32];
        arr[31] = 1;
        arr
    }),
    U256::from_be_bytes({
        let mut arr = [0; 32];
        arr[31] = 2;
        arr
    }),
];

sol! {
    #[sol(rpc)]
    contract ConditionalTokens {
        function splitPosition(
            address collateralToken,
            bytes32 parentCollectionId,
            bytes32 conditionId,
            uint[] calldata partition,
            uint amount
        ) external;

        function mergePositions(
            address collateralToken,
            bytes32 parentCollectionId,
            bytes32 conditionId,
            uint[] calldata partition,
            uint amount
        ) external;

        function redeemPositions(address collateralToken, bytes32 parentCollectionId, bytes32 conditionId, uint[] calldata indexSets) external;

        function balanceOf(address owner, uint256 positionId) external view returns (uint);

        function setApprovalForAll(address operator, bool approved) external;
    }

    #[sol(rpc)]
    contract ERC20 {
        function approve(address spender, uint amount) external returns (bool);
        function balanceOf(address owner) external view returns (uint);
    }
    struct ClobAuth {
        address address;
        string timestamp;
        uint256 nonce;
        string message;
    }

    struct Order {
        uint256 salt;
        address maker;
        address signer;
        address taker;
        uint256 tokenId;
        uint256 makerAmount;
        uint256 takerAmount;
        uint256 expiration;
        uint256 nonce;
        uint256 feeRateBps;
        uint8 side;
        uint8 signatureType;
    }

}

#[derive(Debug, Clone)]
pub struct ContractConfig {
    pub exchange: Address,
    pub collateral: Address,
    pub conditional_tokens: Address,
    pub neg_risk_conditional_tokens: Address,
    pub neg_risk_collateral: Address,
    pub neg_risk_exchange: Address,
    pub neg_risk_adapter: Address,
}

impl ContractConfig {
    pub fn from_chain_id(chain_id: u64) -> Option<ContractConfig> {
        if chain_id == 137 {
            return Some(ContractConfig {
                exchange: address!("4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E"),
                collateral: address!("2791bca1f2de4661ed88a30c99a7a9449aa84174"),
                conditional_tokens: address!("4D97DCd97eC945f40cF65F87097ACe5EA0476045"),
                neg_risk_exchange: address!("C5d563A36AE78145C45a50134d48A1215220f80a"),
                neg_risk_collateral: address!("2791Bca1f2de4661ED88A30C99A7a9449Aa84174"),
                neg_risk_conditional_tokens: address!("4D97DCd97eC945f40cF65F87097ACe5EA0476045"),
                neg_risk_adapter: address!("d91E80cF2E7be2e162c6513ceD06f1dD0dA35296"),
            });
        } else if chain_id == 80002 {
            return Some(ContractConfig {
                exchange: address!("dFE02Eb6733538f8Ea35D585af8DE5958AD99E40"),
                collateral: address!("9c4e1703476e875070ee25b56a58b008cfb8fa78"),
                conditional_tokens: address!("69308FB512518e39F9b16112fA8d994F4e2Bf8bB"),
                neg_risk_exchange: address!("C5d563A36AE78145C45a50134d48A1215220f80a"),
                neg_risk_collateral: address!("9c4e1703476e875070ee25b56a58b008cfb8fa78"),
                neg_risk_conditional_tokens: address!("69308FB512518e39F9b16112fA8d994F4e2Bf8bB"),
                neg_risk_adapter: address!("d91E80cF2E7be2e162c6513ceD06f1dD0dA35296"),
            });
        }
        None
    }

    /// Get the CTF and collateral addresses for the given neg risk.
    /// 
    /// # WARNING
    /// This method retuns the __adapter__ for the conditional token for neg risk.
    /// 
    /// If you need to approvals, you should use the real conditional token address instead.
    pub const fn get_ctf_and_collateral(&self, neg_risk: bool) -> (Address, Address) {
        if neg_risk {
            (self.neg_risk_adapter, self.neg_risk_collateral)
        } else {
            (self.conditional_tokens, self.collateral)
        }
    }

    pub const fn get_exchange(&self, neg_risk: bool) -> Address {
        if neg_risk {
            self.neg_risk_exchange
        } else {
            self.exchange
        }
    }
}

pub async fn sign_clob_auth_message<S: Signer + Sync>(
    signer: &S,
    timestamp: String,
    nonce: U256,
) -> Result<Signature, alloy::signers::Error> {
    let message = "This message attests that I control the given wallet".to_owned();
    let polygon = 137;

    let my_struct = ClobAuth {
        address: signer.address(),
        timestamp,
        nonce,
        message,
    };

    let my_domain = eip712_domain!(
        name: "ClobAuthDomain",
        version: "1",
        chain_id: polygon,
    );

    signer.sign_typed_data(&my_struct, &my_domain).await
}

pub async fn sign_order_message<S: Signer + Sync>(
    signer: &S,
    order: Order,
    chain_id: u64,
    verifying_contract: Address,
) -> Result<Signature, alloy::signers::Error> {
    let domain = eip712_domain!(
        name: "Polymarket CTF Exchange",
        version: "1",
        chain_id: chain_id,
        verifying_contract: verifying_contract,
    );

    signer.sign_typed_data(&order, &domain).await
}
