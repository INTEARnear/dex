mod common;

use common::{Cli, start_sandbox_with_xyk_pools};

#[tokio::test]
async fn views() {
    let sandbox = start_sandbox_with_xyk_pools().await;
    let cli = Cli::connected_to(&sandbox);

    // The code of the engine and the dex takes storage, and the engine earns a
    // share of the gas burned in it, so these change with every build
    insta::with_settings!({filters => vec![
        (r"(Dex storage +\S+ NEAR total, )\S+ NEAR", "$1[BUILD DEPENDENT] NEAR"),
        (r"(Untracked NEAR +)\S+ NEAR", "$1[BUILD DEPENDENT] NEAR"),
        (
            r#"("dexes": \{\s+"total": \{[^}]*\},\s+"available": \{\s+"raw": )"\d+",(\s+"decimal": )"[\d.]+""#,
            r#"$1"[BUILD DEPENDENT]",$2"[BUILD DEPENDENT]""#,
        ),
        (
            r#"("untracked_near": \{\s+"raw": )"\d+",(\s+"decimal": )"[\d.]+""#,
            r#"$1"[BUILD DEPENDENT]",$2"[BUILD DEPENDENT]""#,
        ),
    ]}, {
        insta::assert_snapshot!("engine_info", cli.view(&["engine", "info"]));
        insta::assert_snapshot!("engine_info_json", cli.view(&["--json", "engine", "info"]));
    });

    insta::assert_snapshot!(
        "engine_balance_of_account",
        cli.view(&["engine", "balance", "slimedragon.near", "all"])
    );
    insta::assert_snapshot!(
        "engine_balance_of_account_json",
        cli.view(&["--json", "engine", "balance", "alice.near", "all"])
    );
    insta::assert_snapshot!(
        "engine_balance_of_listed_assets",
        cli.view(&[
            "engine",
            "balance",
            "alice.near",
            "near,nep141:usdt.tether-token.near,nep141:wrap.near",
        ])
    );
    insta::assert_snapshot!(
        "engine_balance_of_dex",
        cli.view(&[
            "engine",
            "balance",
            "slimedragon.near/xyk",
            "near,nep141:usdt.tether-token.near,nep141:intel.tkn.near",
        ])
    );
    insta::assert_snapshot!(
        "engine_balance_of_dex_with_all_assets",
        cli.view(&["engine", "balance", "slimedragon.near/xyk", "all"])
    );
    insta::assert_snapshot!(
        "engine_balance_of_unregistered_account",
        cli.view(&["engine", "balance", "bob.near", "all"])
    );

    insta::assert_snapshot!(
        "engine_storage_show",
        cli.view(&["engine", "storage", "show", "alice.near"])
    );
    insta::assert_snapshot!(
        "engine_storage_show_json",
        cli.view(&["--json", "engine", "storage", "show", "alice.near"])
    );
    insta::assert_snapshot!(
        "engine_storage_show_unregistered_account",
        cli.view(&["engine", "storage", "show", "bob.near"])
    );

    insta::assert_snapshot!(
        "engine_assets_status",
        cli.view(&[
            "engine",
            "assets",
            "status",
            "alice.near",
            "near,nep141:intel.tkn.near,nep141:wrap.near",
        ])
    );
    insta::assert_snapshot!(
        "engine_assets_status_json",
        cli.view(&[
            "--json",
            "engine",
            "assets",
            "status",
            "slimedragon.near/xyk",
            "near,nep141:wrap.near",
        ])
    );

    insta::assert_snapshot!("xyk_pools_count", cli.view(&["xyk", "pools", "count"]));
    insta::assert_snapshot!(
        "xyk_pools_count_json",
        cli.view(&["--json", "xyk", "pools", "count"])
    );
    insta::assert_snapshot!("xyk_pools_list", cli.view(&["xyk", "pools", "list"]));
    insta::assert_snapshot!(
        "xyk_pools_list_json",
        cli.view(&["--json", "xyk", "pools", "list"])
    );
    insta::assert_snapshot!(
        "xyk_pools_list_with_asset",
        cli.view(&[
            "xyk",
            "pools",
            "list",
            "--asset",
            "nep141:usdt.tether-token.near"
        ])
    );

    for pool_id in ["0", "1", "2", "3"] {
        insta::assert_snapshot!(
            format!("xyk_pool_show_{pool_id}"),
            cli.view(&["xyk", "pool", "show", pool_id])
        );
    }
    insta::assert_snapshot!(
        "xyk_pool_show_json",
        cli.view(&["--json", "xyk", "pool", "show", "1"])
    );
    insta::assert_snapshot!(
        "xyk_pool_show_missing_pool",
        cli.view(&["xyk", "pool", "show", "4"])
    );

    insta::assert_snapshot!(
        "xyk_pools_count_of_missing_dex",
        cli.view(&["xyk", "--dex", "alice.near/xyk", "pools", "count"])
    );
}

#[tokio::test]
async fn views_without_engine() {
    let sandbox = near_workspaces::sandbox().await.unwrap();
    let cli = Cli::connected_to(&sandbox);

    insta::assert_snapshot!("engine_info_without_engine", cli.view(&["engine", "info"]));
}
