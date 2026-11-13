use std::collections::HashMap;

use intear_dex_common::{DexId, expect};
use intear_dex_types::IntearDexEvent;
use near_sdk::{
    AccountId, Gas, GasWeight, IntoStorageKey, NearToken, Promise, json_types::Base58CryptoHash,
    near,
};
use wasmi::{Engine, Module};

use crate::{DexEngine, DexEngineExt, StorageKey};

#[near]
impl DexEngine {
    /// Translate the code of dexes again, which the engine needs once the wasmi of the engine and
    /// the translator changes how it translates, since the engine only runs the translations of
    /// the wasmi that it has
    pub fn translate_dex_codes(&mut self, dex_ids: Vec<DexId>) {
        for dex_id in dex_ids {
            let code = self.dex_codes.get(&dex_id).expect("Dex code not found");
            Self::translate_dex_code(dex_id, code.clone(), false).detach();
        }
    }

    /// Store the translation of a dex's code that the translator returned, and the code if it's
    /// being deployed, which the dex pays the storage of
    #[private]
    pub fn after_dex_code_translation(
        &mut self,
        #[serializer(borsh)] dex_id: DexId,
        #[serializer(borsh)] code: Vec<u8>,
        #[serializer(borsh)] deployed: bool,
    ) {
        let translated_code =
            near_sdk::env::promise_result_checked(0, usize::MAX).unwrap_or_else(|err| {
                panic!("The translator didn't translate the code of {dex_id}: {err:?}")
            });
        // A deployment that stored other code while this code was translated is what runs
        expect!(
            deployed || self.dex_codes.get(&dex_id) == Some(&code),
            "The code of {dex_id} changed while it was translated"
        );
        let storage_usage_before = near_sdk::env::storage_usage();
        self.translated_dex_codes.insert(&dex_id, translated_code);
        if deployed {
            let code_hash = near_sdk::env::sha256_array(&code);
            self.dex_codes.insert(dex_id.clone(), code);
            self.dex_codes.flush();
            IntearDexEvent::DexDeployed {
                dex_id: dex_id.clone(),
                code_hash: Base58CryptoHash::from(code_hash),
            }
            .emit();
        }
        self.dex_storage_balances.charge(
            &dex_id,
            storage_usage_before,
            near_sdk::env::storage_usage(),
        );
    }
}

impl DexEngine {
    pub fn translate_dex_code(dex_id: DexId, code: Vec<u8>, deployed: bool) -> Promise {
        Promise::new(translator_account_id())
            .function_call_weight(
                "translate".to_string(),
                code.clone(),
                NearToken::from_yoctonear(0),
                Gas::from_gas(0),
                GasWeight(1),
            )
            .then(
                Self::ext(near_sdk::env::current_account_id())
                    .after_dex_code_translation(dex_id, code, deployed),
            )
    }
}

/// The translator contract, which validates and translates the code of dexes, so that the
/// engine's own code doesn't include wasmi's validator and translator
fn translator_account_id() -> AccountId {
    format!("translator.{}", near_sdk::env::current_account_id())
        .parse()
        .expect("The translator's account id is valid")
}

/// Stores under [`StorageKey::TranslatedDexCodes`] without copying and caches loaded modules in memory.
#[derive(Default)]
pub struct TranslatedDexCodes {
    loaded: HashMap<DexId, Module>,
}

impl TranslatedDexCodes {
    // Also tries to deserialize the code to store
    pub fn insert(&mut self, dex_id: &DexId, translated_code: Vec<u8>) {
        near_sdk::env::storage_write(&translated_dex_code_key(dex_id), &translated_code);
        self.loaded
            .insert(dex_id.clone(), load(dex_id, translated_code));
    }

    pub fn get(&mut self, dex_id: &DexId) -> Module {
        self.loaded
            .entry(dex_id.clone())
            .or_insert_with(|| load(dex_id, read(dex_id)))
            .clone()
    }

    /// The module of a dex, for calls that can't change the engine to keep it loaded
    pub fn load(&self, dex_id: &DexId) -> Module {
        match self.loaded.get(dex_id) {
            Some(module) => module.clone(),
            None => load(dex_id, read(dex_id)),
        }
    }
}

fn read(dex_id: &DexId) -> Vec<u8> {
    near_sdk::env::storage_read(&translated_dex_code_key(dex_id))
        .unwrap_or_else(|| panic!("Dex code not found"))
}

fn load(dex_id: &DexId, translated_code: Vec<u8>) -> Module {
    // Read by the functions of the module for as long as the call runs
    let translated_code: &'static [u8] = translated_code.leak();
    // SAFETY: translated by the translator, which the engine trusts, and stored unchanged.
    // Translations of another version or config of wasmi are rejected.
    unsafe { Module::deserialize(&Engine::default(), translated_code) }.unwrap_or_else(|err| {
        panic!(
            "Failed to load the code of {dex_id}, which translate_dex_codes translates again: {err}"
        )
    })
}

fn translated_dex_code_key(dex_id: &DexId) -> Vec<u8> {
    [
        StorageKey::TranslatedDexCodes.into_storage_key(),
        near_sdk::borsh::to_vec(dex_id).expect("Failed to serialize dex id"),
    ]
    .concat()
}
