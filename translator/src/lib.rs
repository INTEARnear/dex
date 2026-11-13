use near_sdk::near;
use wasmi::Engine;

#[near(contract_state)]
pub struct DexTranslator;

#[near]
impl DexTranslator {
    /// Validate the Wasm code of a dex, which is the input as it is, and return its translation
    /// as it is, which the engine's wasmi runs if it's of the same version and config
    pub fn translate() {
        let code = near_sdk::env::input().expect("The input is the code of the dex");
        let translated_code = Engine::default()
            .precompile_module(&code)
            .unwrap_or_else(|err| near_sdk::env::panic_str(&format!("Invalid dex code: {err}")));
        near_sdk::env::value_return(&translated_code);
    }
}
