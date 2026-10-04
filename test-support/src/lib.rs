use std::path::{Path, PathBuf};
use tokio::process::Command;
use tokio::sync::OnceCell;

pub struct CompiledWasms {
    pub contract_wasm: Vec<u8>,
    pub simple_amm_dex_wasm: Vec<u8>,
    pub minimal_dex_wasm: Vec<u8>,
    pub otc_dex_wasm: Vec<u8>,
    pub xyk_dex_wasm: Vec<u8>,
    pub ft_wasm: Vec<u8>,
}

static COMPILED_WASMS: OnceCell<CompiledWasms> = OnceCell::const_new();

pub async fn get_compiled_wasms() -> &'static CompiledWasms {
    COMPILED_WASMS
        .get_or_init(|| async {
            let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();

            let cargo_metadata_output = Command::new("cargo")
                .current_dir(workspace_root)
                .args(["metadata", "--format-version=1", "--no-deps"])
                .output()
                .await
                .unwrap();
            assert!(
                cargo_metadata_output.status.success(),
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&cargo_metadata_output.stderr)
            );
            let cargo_metadata: serde_json::Value =
                serde_json::from_slice(&cargo_metadata_output.stdout).unwrap();
            let target_directory =
                PathBuf::from(cargo_metadata["target_directory"].as_str().unwrap());

            println!("Compiling intear-dex");
            assert!(
                Command::new("cargo")
                    .current_dir(workspace_root)
                    .args([
                        "near",
                        "build",
                        "non-reproducible-wasm",
                    ])
                    .status()
                    .await
                    .unwrap()
                    .success()
            );
            let contract_wasm =
                std::fs::read(target_directory.join("near/intear_dex.wasm")).unwrap();

            let release_wasm_directory = target_directory.join("wasm32-unknown-unknown/release");

            let simple_amm_dex_wasm =
                compile_dex(workspace_root, &release_wasm_directory, "simple-amm-dex").await;
            let minimal_dex_wasm =
                compile_dex(workspace_root, &release_wasm_directory, "minimal-dex").await;
            let otc_dex_wasm =
                compile_dex(workspace_root, &release_wasm_directory, "otc-dex").await;
            let xyk_dex_wasm =
                compile_dex(workspace_root, &release_wasm_directory, "xyk-dex").await;

            println!("Compilation complete");

            CompiledWasms {
                contract_wasm,
                simple_amm_dex_wasm,
                minimal_dex_wasm,
                otc_dex_wasm,
                xyk_dex_wasm,
                ft_wasm: include_bytes!("../assets/ft.wasm").to_vec(),
            }
        })
        .await
}

async fn compile_dex(
    workspace_root: &Path,
    release_wasm_directory: &Path,
    package: &str,
) -> Vec<u8> {
    println!("Compiling {package}");
    assert!(
        Command::new("cargo")
            .current_dir(workspace_root)
            .args([
                "build",
                &format!("--package={package}"),
                "--release",
                "--target",
                "wasm32-unknown-unknown",
            ])
            .status()
            .await
            .unwrap()
            .success()
    );
    let wasm_path = release_wasm_directory.join(format!("{}.wasm", package.replace('-', "_")));
    assert!(
        Command::new("wasm-opt")
            .arg("-O")
            .arg(&wasm_path)
            .arg("-o")
            .arg(&wasm_path)
            .status()
            .await
            .unwrap()
            .success()
    );
    std::fs::read(&wasm_path).unwrap()
}
