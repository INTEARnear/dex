mod common;

use common::{
    Cli, start_sandbox_with_xyk_pools, swap_through_engine, transfer_on_engine,
    xyk_transaction_paying_storage,
};
use intear_dex_types::{AssetId, SwapRequestAmount};
use near_sdk::json_types::U128;

const XYK_DEX_ID: &str = "slimedragon.near/xyk";
const ONE_NEAR: u128 = 10u128.pow(24);
const ONE_USDT: u128 = 10u128.pow(6);
const ONE_INTEL: u128 = 10u128.pow(18);

/// Every xyk command of the old manage CLI but its trades, which this CLI
/// doesn't make, and the failures each one refuses before signing. The
/// swaps that produce fees go through the engine directly. Pools #0 to #3
/// are the fixture's; this creates #4 (private, alice.near), #5 (public,
/// bob.near) and #6 (launch, alice.near). Fee schedules use absolute times,
/// since `now` is the time of the block.
#[tokio::test]
async fn xyk_writes() {
    let sandbox = start_sandbox_with_xyk_pools().await;
    let cli = Cli::connected_to(&sandbox);
    let usdt = AssetId::Nep141("usdt.tether-token.near".parse().unwrap());
    let intel = AssetId::Nep141("intel.tkn.near".parse().unwrap());

    // referrers set-settings and referrers register-fee-assets of the old CLI
    insta::assert_snapshot!(
        "referrer_show_without_settings",
        cli.view(&["xyk", "referrer", "show", "carol.near"])
    );
    insta::assert_snapshot!(
        "referrer_set_fees_too_high",
        cli.transaction(
            "carol.near",
            &["xyk", "referrer", "set-fees", "carol.near", "5%", "1%"]
        )
        .await
    );
    insta::assert_snapshot!(
        "referrer_set_fees",
        xyk_transaction_paying_storage(
            &sandbox,
            &cli,
            "carol.near",
            XYK_DEX_ID,
            &["xyk", "referrer", "set-fees", "carol.near", "0.1%", "0.05%"]
        )
        .await
    );
    insta::assert_snapshot!(
        "referrer_register_fee_assets",
        xyk_transaction_paying_storage(
            &sandbox,
            &cli,
            "carol.near",
            XYK_DEX_ID,
            &[
                "xyk",
                "referrer",
                "register-fee-assets",
                "carol.near",
                "near,nep141:usdt.tether-token.near"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "referrer_register_fee_assets_already_registered",
        cli.transaction(
            "carol.near",
            &[
                "xyk",
                "referrer",
                "register-fee-assets",
                "carol.near",
                "near"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "referrer_show",
        cli.view(&["xyk", "referrer", "show", "carol.near"])
    );
    insta::assert_snapshot!(
        "referrer_show_json",
        cli.view(&["--json", "xyk", "referrer", "show", "carol.near"])
    );
    // Fees for the fee commands: alice.near collects 0.05% of pool #0's
    // swaps, and carol.near its referral fee in NEAR. After each change made
    // around the CLI, the views of later commands wait for its block to be
    // final.
    swap_through_engine(
        &sandbox,
        "alice.near",
        0,
        (AssetId::Near, usdt.clone()),
        SwapRequestAmount::ExactIn(U128(ONE_NEAR)),
        Some("carol.near"),
    )
    .await;
    swap_through_engine(
        &sandbox,
        "alice.near",
        0,
        (usdt.clone(), AssetId::Near),
        SwapRequestAmount::ExactIn(U128(ONE_USDT)),
        None,
    )
    .await;
    sandbox.fast_forward(10).await.unwrap();

    // get-pending-fees and withdraw-fees of the old CLI
    insta::assert_snapshot!(
        "fees_pending",
        cli.view(&["xyk", "fees", "pending", "alice.near", "all"])
    );
    insta::assert_snapshot!(
        "fees_pending_json",
        cli.view(&["--json", "xyk", "fees", "pending", "alice.near", "all"])
    );
    insta::assert_snapshot!(
        "fees_pending_of_account_without_fees",
        cli.view(&["xyk", "fees", "pending", "bob.near", "all"])
    );
    insta::assert_snapshot!(
        "fees_withdraw_to_balance",
        cli.transaction(
            "alice.near",
            &["xyk", "fees", "withdraw", "alice.near", "all", "to-balance"]
        )
        .await
    );
    insta::assert_snapshot!(
        "fees_withdraw_when_nothing_is_pending",
        cli.transaction(
            "alice.near",
            &["xyk", "fees", "withdraw", "alice.near", "all", "to-wallet"]
        )
        .await
    );
    insta::assert_snapshot!(
        "fees_withdraw_to_wallet",
        cli.transaction(
            "carol.near",
            &["xyk", "fees", "withdraw", "carol.near", "near", "to-wallet"]
        )
        .await
    );

    // create-pool of the old CLI
    insta::assert_snapshot!(
        "create_pool_private",
        xyk_transaction_paying_storage(
            &sandbox,
            &cli,
            "alice.near",
            XYK_DEX_ID,
            &[
                "xyk",
                "create-pool",
                "private",
                "alice.near",
                "near",
                "nep141:intel.tkn.near",
                "fees",
                "pool=0.3%"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "create_pool_public_with_scheduled_fees",
        xyk_transaction_paying_storage(
            &sandbox,
            &cli,
            "bob.near",
            XYK_DEX_ID,
            &[
                "xyk",
                "create-pool",
                "public",
                "bob.near",
                "nep141:usdt.tether-token.near",
                "nep141:intel.tkn.near",
                "fees",
                "bob.near=0.2%..0.1%@2100-01-01T00:00:00Z..2100-07-01T00:00:00Z,pool=0.1%"
            ]
        )
        .await
    );
    // INTEL for alice.near's pools
    transfer_on_engine(
        &sandbox,
        "slimedragon.near",
        "alice.near",
        intel.clone(),
        5_000 * ONE_INTEL,
    )
    .await;
    sandbox.fast_forward(10).await.unwrap();
    insta::assert_snapshot!(
        "create_pool_launch",
        xyk_transaction_paying_storage(
            &sandbox,
            &cli,
            "alice.near",
            XYK_DEX_ID,
            &[
                "xyk",
                "create-pool",
                "launch",
                "alice.near",
                "nep141:intel.tkn.near",
                "1000 INTEL",
                "phantom-liquidity",
                "5 NEAR",
                "fees",
                "community:carol.near=1%"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "create_pool_of_one_asset",
        cli.transaction(
            "alice.near",
            &[
                "xyk",
                "create-pool",
                "private",
                "alice.near",
                "near",
                "near",
                "fees",
                "none"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "create_pool_with_community_fee_outside_launch_pool",
        cli.transaction(
            "alice.near",
            &[
                "xyk",
                "create-pool",
                "public",
                "alice.near",
                "near",
                "nep141:usdt.tether-token.near",
                "fees",
                "community:carol.near=1%"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "create_pool_with_fees_too_high",
        cli.transaction(
            "alice.near",
            &[
                "xyk",
                "create-pool",
                "private",
                "alice.near",
                "near",
                "nep141:usdt.tether-token.near",
                "fees",
                "pool=30%,alice.near=25%"
            ]
        )
        .await
    );

    // add-liquidity and remove-liquidity of the old CLI
    insta::assert_snapshot!(
        "liquidity_add_to_private_pool",
        cli.transaction(
            "alice.near",
            &[
                "xyk",
                "liquidity",
                "add",
                "alice.near",
                "4",
                "0.1 NEAR",
                "2000 INTEL",
                "max-slippage",
                "1%"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "liquidity_add_to_pool_of_another_owner",
        cli.transaction(
            "alice.near",
            &[
                "xyk",
                "liquidity",
                "add",
                "alice.near",
                "0",
                "1 NEAR",
                "3 USDt",
                "max-slippage",
                "1%"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "liquidity_add_to_public_pool_with_registration",
        xyk_transaction_paying_storage(
            &sandbox,
            &cli,
            "alice.near",
            XYK_DEX_ID,
            &[
                "xyk",
                "liquidity",
                "add",
                "alice.near",
                "2",
                "0.01 NEAR",
                "200 INTEL",
                "max-slippage",
                "1%"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "liquidity_add_to_launch_pool",
        cli.transaction(
            "alice.near",
            &[
                "xyk",
                "liquidity",
                "add",
                "alice.near",
                "6",
                "0.1 NEAR",
                "10 INTEL",
                "max-slippage",
                "1%"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "liquidity_show",
        cli.view(&["xyk", "liquidity", "show", "alice.near"])
    );
    insta::assert_snapshot!(
        "liquidity_show_json",
        cli.view(&["--json", "xyk", "liquidity", "show", "alice.near"])
    );
    insta::assert_snapshot!(
        "liquidity_show_of_account_without_liquidity",
        cli.view(&["xyk", "liquidity", "show", "carol.near"])
    );
    insta::assert_snapshot!(
        "liquidity_remove_half_of_shares",
        cli.transaction(
            "alice.near",
            &[
                "xyk",
                "liquidity",
                "remove",
                "alice.near",
                "2",
                "50%",
                "max-slippage",
                "1%"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "liquidity_remove_from_locked_pool",
        cli.transaction(
            "slimedragon.near",
            &[
                "xyk",
                "liquidity",
                "remove",
                "slimedragon.near",
                "1",
                "50%",
                "max-slippage",
                "1%"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "liquidity_remove_without_shares",
        cli.transaction(
            "bob.near",
            &[
                "xyk",
                "liquidity",
                "remove",
                "bob.near",
                "2",
                "all",
                "max-slippage",
                "1%"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "liquidity_remove_all_of_private_pool",
        cli.transaction(
            "alice.near",
            &[
                "xyk",
                "liquidity",
                "remove",
                "alice.near",
                "4",
                "all",
                "max-slippage",
                "1%"
            ]
        )
        .await
    );

    // upgrade-pool and edit-fees of the old CLI
    insta::assert_snapshot!(
        "pool_edit_fees_with_schedule_of_old_pool",
        cli.transaction(
            "slimedragon.near",
            &[
                "xyk",
                "pool",
                "edit-fees",
                "slimedragon.near",
                "0",
                "pool=0.3%..0.2%@2100-01-01T00:00:00Z..2100-07-01T00:00:00Z"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "pool_upgrade",
        xyk_transaction_paying_storage(
            &sandbox,
            &cli,
            "alice.near",
            XYK_DEX_ID,
            &["xyk", "pool", "upgrade", "alice.near", "0"]
        )
        .await
    );
    insta::assert_snapshot!(
        "pool_upgrade_of_latest_pool",
        cli.transaction("alice.near", &["xyk", "pool", "upgrade", "alice.near", "0"])
            .await
    );
    insta::assert_snapshot!(
        "pool_edit_fees_by_account_other_than_owner",
        cli.transaction(
            "alice.near",
            &["xyk", "pool", "edit-fees", "alice.near", "0", "pool=0.1%"]
        )
        .await
    );
    insta::assert_snapshot!(
        "pool_edit_fees",
        xyk_transaction_paying_storage(
            &sandbox,
            &cli,
            "slimedragon.near",
            XYK_DEX_ID,
            &[
                "xyk",
                "pool",
                "edit-fees",
                "slimedragon.near",
                "0",
                "pool=0.3%..0.2%@2100-01-01T00:00:00Z..2100-07-01T00:00:00Z,bob.near=0.05%"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "pool_show_after_upgrade_and_fee_change",
        cli.view(&["xyk", "pool", "show", "0"])
    );
    insta::assert_snapshot!(
        "pool_edit_fees_of_locked_pool",
        cli.transaction(
            "slimedragon.near",
            &[
                "xyk",
                "pool",
                "edit-fees",
                "slimedragon.near",
                "1",
                "pool=0.1%"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "pool_edit_fees_of_public_pool",
        cli.transaction(
            "slimedragon.near",
            &[
                "xyk",
                "pool",
                "edit-fees",
                "slimedragon.near",
                "2",
                "pool=0.1%"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "pool_lock",
        cli.transaction("alice.near", &["xyk", "pool", "lock", "alice.near", "4"])
            .await
    );
    insta::assert_snapshot!(
        "pool_lock_of_locked_pool",
        cli.transaction("alice.near", &["xyk", "pool", "lock", "alice.near", "4"])
            .await
    );
    insta::assert_snapshot!(
        "pool_lock_of_public_pool",
        cli.transaction(
            "slimedragon.near",
            &["xyk", "pool", "lock", "slimedragon.near", "2"]
        )
        .await
    );

    // Community fees, which the old CLI had no command for
    // A purchase on alice.near's launch pool pays carol.near its community fee
    swap_through_engine(
        &sandbox,
        "slimedragon.near",
        6,
        (AssetId::Near, intel.clone()),
        SwapRequestAmount::ExactOut(U128(10 * ONE_INTEL)),
        None,
    )
    .await;
    sandbox.fast_forward(10).await.unwrap();
    insta::assert_snapshot!(
        "community_fees_show",
        cli.view(&["xyk", "community-fees", "show", "carol.near"])
    );
    insta::assert_snapshot!(
        "community_fees_withdraw",
        cli.transaction(
            "bob.near",
            &[
                "xyk",
                "community-fees",
                "withdraw",
                "bob.near",
                "carol.near"
            ]
        )
        .await
    );
    insta::assert_snapshot!(
        "community_fees_withdraw_when_there_are_none",
        cli.transaction(
            "bob.near",
            &[
                "xyk",
                "community-fees",
                "withdraw",
                "bob.near",
                "carol.near"
            ]
        )
        .await
    );
}
