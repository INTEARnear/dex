use near_sdk::json_types::U128;
use serde_json::json;
use tokio::process::Command;

#[tokio::test]
async fn test_contract_is_operational() -> Result<(), Box<dyn std::error::Error>> {
    let contract_wasm = near_workspaces::compile_project("./").await?;

    assert!(Command::new("cargo")
        .args(&[
            "build",
            "--package=example-dex",
            "--release",
            "--target",
            "wasm32-unknown-unknown"
        ])
        .status()
        .await
        .expect("Failed to run cargo build")
        .success());

    let dex_wasm = std::fs::read("./target/wasm32-unknown-unknown/release/example_dex.wasm")
        .expect("Failed to read wasm file");

    test_basics_on(&contract_wasm, &dex_wasm).await?;
    Ok(())
}

async fn test_basics_on(
    contract_wasm: &[u8],
    dex_wasm: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let sandbox = near_workspaces::sandbox().await?;
    let contract = sandbox.dev_deploy(contract_wasm).await?;

    let user_account = sandbox.dev_create_account().await?;

    let outcome = user_account
        .call(contract.id(), "swap")
        .max_gas()
        .args_json(json!({
            "code": dex_wasm,
        }))
        .transact()
        .await?;
    println!("{}", outcome.total_gas_burnt);
    assert_eq!(outcome.json::<U128>()?, 42.into());

    assert!(false);
    Ok(())
}
