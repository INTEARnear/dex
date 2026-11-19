use near_sdk::{AccountId, BlockHeight, PublicKey, near};

use crate::{AssetId, DexId, FeeFraction, SwapRequestAmount};

/// 100% = 1000000
#[near(serializers=[borsh])]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct FeeDiscountFraction(u32);

pub const FULL_FEE_DISCOUNT: FeeDiscountFraction = FeeDiscountFraction::new(1_000_000);

impl FeeDiscountFraction {
    pub const fn new(millionths: u32) -> Self {
        Self(millionths)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    /// `fee` without the discount, rounded down
    pub fn apply(self, fee: FeeFraction) -> FeeFraction {
        let remaining_millionths = FULL_FEE_DISCOUNT
            .0
            .checked_sub(self.0)
            .expect("Fee discount can't exceed 100%");
        let discounted = u64::from(fee.get())
            .checked_mul(u64::from(remaining_millionths))
            .expect("Fee fraction overflow")
            .checked_div(u64::from(FULL_FEE_DISCOUNT.0))
            .expect("Dividing by the full fee discount");
        FeeFraction::new(
            u32::try_from(discounted)
                .expect("Discounted fee can't be higher than the original fee"),
        )
    }
}

/// The most blocks before a [`TraderFeeDiscount`] expires, about 7 days
pub const MAX_TRADER_FEE_DISCOUNT_BLOCKS: BlockHeight = 1_008_000;

#[near(serializers=[borsh])]
#[borsh(use_discriminant = true)]
#[repr(u8)]
#[derive(Debug)]
pub enum SwapFeeDiscount<Route> {
    None = 0,
    ForSwap(SignedFeeDiscount<Route>) = 1,
    ForTrader(SignedTraderFeeDiscount) = 2,
}

#[near(serializers=[borsh])]
#[derive(Debug)]
pub struct SignedFeeDiscount<Route> {
    pub discount: FeeDiscount<Route>,
    /// signature of sha256(borsh([`FeeDiscountSignaturePayload`])) by the
    /// dex's fee discount signer
    pub signature: Vec<u8>,
}

#[near(serializers=[borsh])]
#[derive(Clone, Debug)]
pub struct FeeDiscount<Route> {
    /// Where the swap goes in the dex, like its pool or routes
    pub route: Route,
    pub asset_in: AssetId,
    pub asset_out: AssetId,
    /// The only alleged trader who can use the discount, anyone can if `None`
    pub trader_id: Option<AccountId>,
    pub amount: SwapRequestAmount,
    /// Reduces pool and protocol fees, but not referral fees.
    /// [`FULL_FEE_DISCOUNT`] waives pool and protocol fees entirely.
    pub fee_discount: FeeDiscountFraction,
    pub expiration_block_height: BlockHeight,
}

/// What the fee discount signer signs for [`SignedFeeDiscount`]
#[near(serializers=[borsh])]
#[derive(Debug)]
pub struct FeeDiscountSignaturePayload<Route> {
    pub chain_id: String,
    pub engine_contract_id: AccountId,
    pub dex_id: DexId,
    pub discount: FeeDiscount<Route>,
}

#[near(serializers=[borsh])]
#[derive(Debug)]
pub struct SignedTraderFeeDiscount {
    pub discount: TraderFeeDiscount,
    /// signature of sha256(borsh([`TraderFeeDiscountSignaturePayload`])) by the
    /// dex's fee discount signer
    pub signature: Vec<u8>,
}

/// A discount on every swap of the trader until it expires, on any route, assets and amount
#[near(serializers=[borsh])]
#[derive(Clone, Debug)]
pub struct TraderFeeDiscount {
    pub trader_id: AccountId,
    pub fee_discount: FeeDiscountFraction,
    /// At most [`MAX_TRADER_FEE_DISCOUNT_BLOCKS`] after the block of the swap
    pub expiration_block_height: BlockHeight,
}

/// What the fee discount signer signs for [`SignedTraderFeeDiscount`]
#[near(serializers=[borsh])]
#[derive(Debug)]
pub struct TraderFeeDiscountSignaturePayload {
    pub chain_id: String,
    pub engine_contract_id: AccountId,
    pub dex_id: DexId,
    pub discount: TraderFeeDiscount,
}

#[near(serializers=[borsh])]
#[derive(Debug)]
pub struct SetFeeDiscountSignerArgs {
    /// The key that signs fee discounts from now on, or `None` to disable them
    pub fee_discount_signer_public_key: Option<PublicKey>,
}

#[cfg(feature = "dex")]
impl<Route: PartialEq + Clone + near_sdk::borsh::BorshSerialize> SwapFeeDiscount<Route> {
    /// The discount, once it's checked that `signer_public_key`, the dex's fee discount signer,
    /// signed it for `request` on `route`, or `None` if the swap has no discount
    pub fn verify(
        self,
        signer_public_key: Option<&PublicKey>,
        route: &Route,
        request: &crate::SwapRequest,
    ) -> Option<FeeDiscountFraction> {
        match self {
            Self::None => None,
            Self::ForSwap(signed_fee_discount) => {
                Some(signed_fee_discount.verify(signer_public_key, route, request))
            }
            Self::ForTrader(signed_trader_fee_discount) => {
                Some(signed_trader_fee_discount.verify(signer_public_key))
            }
        }
    }
}

#[cfg(feature = "dex")]
impl<Route: PartialEq + Clone + near_sdk::borsh::BorshSerialize> SignedFeeDiscount<Route> {
    fn verify(
        self,
        signer_public_key: Option<&PublicKey>,
        route: &Route,
        request: &crate::SwapRequest,
    ) -> FeeDiscountFraction {
        use crate::{Caller, env, expect};

        let Self {
            discount,
            signature,
        } = self;
        verify_fee_discount_signature(
            signer_public_key,
            &FeeDiscountSignaturePayload {
                chain_id: near_sdk::env::chain_id(),
                engine_contract_id: env::engine_contract_id(),
                dex_id: env::dex_id(),
                discount: discount.clone(),
            },
            signature,
        );
        expect!(
            discount.route == *route,
            "Fee discount is for another route"
        );
        expect!(
            discount.asset_in == request.asset_in && discount.asset_out == request.asset_out,
            "Fee discount is for other assets"
        );
        if let Some(trader_id) = &discount.trader_id {
            expect!(
                match env::caller() {
                    Caller::Direct(account_id) => account_id == *trader_id,
                    Caller::OnTransfer {
                        alleged_sender,
                        transfer_of,
                    } => alleged_sender == *trader_id && transfer_of == request.asset_in,
                },
                "Fee discount is for another trader"
            );
        }
        expect!(
            discount.amount == request.amount,
            "Fee discount is for another amount"
        );
        expect!(
            near_sdk::env::block_height() <= discount.expiration_block_height,
            "Fee discount expired"
        );
        discount.fee_discount
    }
}

#[cfg(feature = "dex")]
impl SignedTraderFeeDiscount {
    fn verify(self, signer_public_key: Option<&PublicKey>) -> FeeDiscountFraction {
        use crate::{Caller, env, expect};

        let Self {
            discount,
            signature,
        } = self;
        verify_fee_discount_signature(
            signer_public_key,
            &TraderFeeDiscountSignaturePayload {
                chain_id: near_sdk::env::chain_id(),
                engine_contract_id: env::engine_contract_id(),
                dex_id: env::dex_id(),
                discount: discount.clone(),
            },
            signature,
        );
        let Caller::Direct(caller_id) = env::caller() else {
            near_sdk::env::panic_str(
                "Trader fee discounts can only be used in direct calls, not in transfer calls",
            );
        };
        expect!(
            caller_id == discount.trader_id,
            "Fee discount is for another trader"
        );
        let block_height = near_sdk::env::block_height();
        expect!(
            block_height <= discount.expiration_block_height,
            "Fee discount expired"
        );
        expect!(
            discount.expiration_block_height
                <= block_height
                    .checked_add(MAX_TRADER_FEE_DISCOUNT_BLOCKS)
                    .expect("Overflow"),
            "Trader fee discount expires more than {MAX_TRADER_FEE_DISCOUNT_BLOCKS} blocks from now"
        );
        discount.fee_discount
    }
}

#[cfg(feature = "dex")]
fn verify_fee_discount_signature(
    signer_public_key: Option<&PublicKey>,
    signature_payload: &impl near_sdk::borsh::BorshSerialize,
    signature: Vec<u8>,
) {
    use near_sdk::CurveType;

    use crate::expect;

    let Some(signer_public_key) = signer_public_key else {
        near_sdk::env::panic_str("Fee discounts are disabled on this dex");
    };
    let hash = near_sdk::env::sha256_array(near_sdk::borsh::to_vec(signature_payload).unwrap());
    expect!(
        match signer_public_key.curve_type() {
            CurveType::ED25519 => {
                near_sdk::env::ed25519_verify(
                    &signature.try_into().unwrap(),
                    hash,
                    signer_public_key.as_bytes()[1..].try_into().unwrap(),
                )
            }
            CurveType::SECP256K1 => {
                near_sdk::env::ecrecover(
                    &hash,
                    &signature[..signature.len().saturating_sub(1)],
                    *signature.last().expect("Invalid signature"),
                    true,
                ) == Some(signer_public_key.as_bytes()[1..].try_into().unwrap())
            }
            CurveType::MLDSA65 => near_sdk::env::ml_dsa_verify(
                &signature.try_into().unwrap(),
                hash,
                signer_public_key.as_bytes()[1..].try_into().unwrap(),
            ),
        },
        "Fee discount signature verification failed"
    );
}
