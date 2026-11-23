use std::collections::HashMap;

use intear_dex_common::{Caller, DexId, expect};
use intear_dex_types::{DexCallAfterDeploy, IntearDexEvent};
use near_sdk::{
    AccountId, Gas, GasWeight, IntoStorageKey, NearToken, Promise, json_types::Base58CryptoHash,
    near,
};
use wasmi::{Engine, Module};

use crate::{DexEngine, DexEngineExt, StorageKey, call_events::CallEvents};

#[near]
impl DexEngine {
    /// Translate the code of dexes again, which the engine needs once the wasmi of the engine and
    /// the translator changes how it translates, since the engine only runs the translations of
    /// the wasmi that it has
    pub fn translate_dex_codes(&mut self, dex_ids: Vec<DexId>) {
        for dex_id in dex_ids {
            let code = self.dex_codes.get(&dex_id).expect("Dex code not found");
            let swap_code = self.dex_swap_codes.get(&dex_id).cloned();
            Self::translate_dex_code(dex_id, code.clone(), swap_code, false, None).detach();
        }
    }

    /// Store the translation of a dex's code that the translator returned, and the code if it's
    /// being deployed, which the dex pays the storage of, and then make the call after the deploy
    #[private]
    pub fn after_dex_code_translation(
        &mut self,
        #[serializer(borsh)] dex_id: DexId,
        #[serializer(borsh)] code: Vec<u8>,
        #[serializer(borsh)] swap_code: Option<Vec<u8>>,
        #[serializer(borsh)] deployed: bool,
        #[serializer(borsh)] call_after_deploy: Option<DexCallAfterDeploy>,
    ) {
        let translation = |result_index: u64| {
            near_sdk::env::promise_result_checked(result_index, usize::MAX).unwrap_or_else(|err| {
                panic!(
                    "The translator didn't translate the {result_index} code of {dex_id}: {err:?}"
                )
            })
        };
        let translated_code = translation(0);
        let translated_swap_code = swap_code.as_ref().map(|_| translation(1));
        // A deployment that stored other code while this code was translated is what runs
        expect!(
            deployed
                || (self.dex_codes.get(&dex_id) == Some(&code)
                    && self.dex_swap_codes.get(&dex_id) == swap_code.as_ref()),
            "The code of {dex_id} changed while it was translated"
        );
        let storage_usage_before = near_sdk::env::storage_usage();
        self.translated_dex_codes.insert(&dex_id, translated_code);
        match translated_swap_code {
            Some(translated_swap_code) => self
                .translated_dex_codes
                .insert_swap(&dex_id, translated_swap_code),
            None => self.translated_dex_codes.remove_swap(&dex_id),
        }
        if deployed {
            let code_hash = near_sdk::env::sha256_array(&code);
            let swap_code_hash = swap_code.as_deref().map(near_sdk::env::sha256_array);
            self.dex_codes.insert(dex_id.clone(), code);
            self.dex_codes.flush();
            match swap_code {
                Some(swap_code) => self.dex_swap_codes.insert(dex_id.clone(), swap_code),
                None => self.dex_swap_codes.remove(&dex_id),
            };
            self.dex_swap_codes.flush();
            IntearDexEvent::DexDeployed {
                dex_id: dex_id.clone(),
                code_hash: Base58CryptoHash::from(code_hash),
                swap_code_hash: swap_code_hash.map(Base58CryptoHash::from),
            }
            .emit();
        }
        self.dex_storage_balances.charge(
            &dex_id,
            storage_usage_before,
            near_sdk::env::storage_usage(),
        );
        if let Some(DexCallAfterDeploy {
            method,
            args,
            attached_assets,
        }) = call_after_deploy
        {
            self.assert_not_paused();
            let mut events = CallEvents::default();
            self.internal_dex_call(
                &mut events,
                dex_id.clone(),
                method,
                args,
                attached_assets,
                Caller::Direct(dex_id.deployer),
                None,
                None,
                None,
                None,
            );
            events.emit();
        }
    }
}

impl DexEngine {
    pub fn translate_dex_code(
        dex_id: DexId,
        code: Vec<u8>,
        swap_code: Option<Vec<u8>>,
        deployed: bool,
        call_after_deploy: Option<DexCallAfterDeploy>,
    ) -> Promise {
        let translate = |code: &[u8]| {
            Promise::new(translator_account_id()).function_call_weight(
                "translate".to_string(),
                code.to_vec(),
                NearToken::from_yoctonear(0),
                Gas::from_gas(0),
                GasWeight(1),
            )
        };
        let translations = match &swap_code {
            Some(swap_code) => translate(&code).and(translate(swap_code)),
            None => translate(&code),
        };
        translations.then(
            Self::ext(near_sdk::env::current_account_id()).after_dex_code_translation(
                dex_id,
                code,
                swap_code,
                deployed,
                call_after_deploy,
            ),
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

/// Stores the translations of the code of dexes under [`StorageKey::TranslatedDexCodes`], and of
/// their swap code under [`StorageKey::TranslatedDexSwapCodes`], without copying, and caches loaded
/// modules in memory
#[derive(Default)]
pub struct TranslatedDexCodes {
    loaded: HashMap<DexId, Module>,
    loaded_for_swaps: HashMap<DexId, Module>,
}

impl TranslatedDexCodes {
    // Also tries to deserialize the code to store
    pub fn insert(&mut self, dex_id: &DexId, translated_code: Vec<u8>) {
        near_sdk::env::storage_write(
            &translated_dex_code_key(StorageKey::TranslatedDexCodes, dex_id),
            &translated_code,
        );
        self.loaded
            .insert(dex_id.clone(), load(dex_id, translated_code));
        self.loaded_for_swaps.remove(dex_id);
    }

    // Also tries to deserialize the code to store
    pub fn insert_swap(&mut self, dex_id: &DexId, translated_swap_code: Vec<u8>) {
        near_sdk::env::storage_write(
            &translated_dex_code_key(StorageKey::TranslatedDexSwapCodes, dex_id),
            &translated_swap_code,
        );
        self.loaded_for_swaps
            .insert(dex_id.clone(), load(dex_id, translated_swap_code));
    }

    pub fn remove_swap(&mut self, dex_id: &DexId) {
        near_sdk::env::storage_remove(&translated_dex_code_key(
            StorageKey::TranslatedDexSwapCodes,
            dex_id,
        ));
        self.loaded_for_swaps.remove(dex_id);
    }

    pub fn get(&mut self, dex_id: &DexId) -> Module {
        self.loaded
            .entry(dex_id.clone())
            .or_insert_with(|| load(dex_id, read(dex_id)))
            .clone()
    }

    pub fn get_for_swaps(&mut self, dex_id: &DexId) -> Module {
        if let Some(module) = self.loaded_for_swaps.get(dex_id) {
            return module.clone();
        }
        let module = match read_swap(dex_id) {
            Some(translated_swap_code) => load(dex_id, translated_swap_code),
            None => self.get(dex_id),
        };
        self.loaded_for_swaps.insert(dex_id.clone(), module.clone());
        module
    }

    /// The module of a dex, for calls that can't change the engine to keep it loaded
    pub fn load(&self, dex_id: &DexId) -> Module {
        match self.loaded.get(dex_id) {
            Some(module) => module.clone(),
            None => load(dex_id, read(dex_id)),
        }
    }

    /// The module that swaps of a dex run, for calls that can't change the engine to keep it
    /// loaded
    pub fn load_for_swaps(&self, dex_id: &DexId) -> Module {
        match self.loaded_for_swaps.get(dex_id) {
            Some(module) => module.clone(),
            None => match read_swap(dex_id) {
                Some(translated_swap_code) => load(dex_id, translated_swap_code),
                None => self.load(dex_id),
            },
        }
    }
}

fn read(dex_id: &DexId) -> Vec<u8> {
    near_sdk::env::storage_read(&translated_dex_code_key(
        StorageKey::TranslatedDexCodes,
        dex_id,
    ))
    .unwrap_or_else(|| panic!("Dex code not found"))
}

fn read_swap(dex_id: &DexId) -> Option<Vec<u8>> {
    near_sdk::env::storage_read(&translated_dex_code_key(
        StorageKey::TranslatedDexSwapCodes,
        dex_id,
    ))
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

fn translated_dex_code_key(prefix: StorageKey, dex_id: &DexId) -> Vec<u8> {
    [
        prefix.into_storage_key(),
        near_sdk::borsh::to_vec(dex_id).expect("Failed to serialize dex id"),
    ]
    .concat()
}
