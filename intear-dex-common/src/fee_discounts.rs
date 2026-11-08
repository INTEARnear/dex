use near_sdk::{AccountId, BlockHeight, PublicKey, near};

use crate::{AssetId, DexId, FeeFraction, SwapRequestAmount};

/// 100% = 1000000
#[near(serializers=[borsh])]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(debug_assertions, derive(Debug))]
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

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct SignedFeeDiscount<Route> {
    pub discount: FeeDiscount<Route>,
    /// signature of sha256(borsh([`FeeDiscountSignaturePayload`])) by the
    /// dex's fee discount signer
    pub signature: Vec<u8>,
}

#[near(serializers=[borsh])]
#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
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
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct FeeDiscountSignaturePayload<Route> {
    pub chain_id: String,
    pub engine_contract_id: AccountId,
    pub dex_id: DexId,
    pub discount: FeeDiscount<Route>,
}

#[near(serializers=[borsh])]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct SetFeeDiscountSignerArgs {
    /// The key that signs fee discounts from now on, or `None` to disable them
    pub fee_discount_signer_public_key: Option<PublicKey>,
}

#[cfg(feature = "dex")]
impl<Route: PartialEq + Clone + near_sdk::borsh::BorshSerialize> SignedFeeDiscount<Route> {
    /// The discount, once it's checked that `signer_public_key`, the dex's fee discount signer,
    /// signed it for `request` on `route`
    pub fn verify(
        self,
        signer_public_key: Option<&PublicKey>,
        route: &Route,
        request: &crate::SwapRequest,
    ) -> FeeDiscountFraction {
        use near_sdk::CurveType;

        use crate::{Caller, env, expect};

        let Self {
            discount,
            signature,
        } = self;
        let Some(signer_public_key) = signer_public_key else {
            near_sdk::env::panic_str("Fee discounts are disabled on this dex");
        };
        let signature_payload = FeeDiscountSignaturePayload {
            chain_id: near_sdk::env::chain_id(),
            engine_contract_id: env::engine_contract_id(),
            dex_id: env::dex_id(),
            discount: discount.clone(),
        };
        let hash =
            near_sdk::env::sha256_array(near_sdk::borsh::to_vec(&signature_payload).unwrap());
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
