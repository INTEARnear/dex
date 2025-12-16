use std::collections::HashMap;

use near_sdk::{json_types::U128, near, store::LookupMap, AccountId, BorshStorageKey, NearToken};
use tear_sdk::{AssetId, SwapRequest, SwapRequestAmount, SwapResponse};
use wasmi::{Caller, Engine, Func, Linker, Module, Store};

macro_rules! declare_unimplemented_host_functions {
    (
        $var: ident: $(
            $(#[$attr:meta])*
            pub fn $name:ident($($arg:ident: $arg_ty:ty),* $(,)?) $(-> $ret:tt)?;
        )*
    ) => {
        $(
            $var.func_wrap(
                "env",
                stringify!($name),
                |_caller: Caller<'_, RunnerData<_, _>>, $(#[allow(unused_variables)] $arg: $arg_ty),*| -> unimplemented_host_functions_return_type!(@return_type $($ret)?) {
                    unimplemented!(concat!("Function ", stringify!($name), " is not implemented"))
                },
            )
            .expect("Failed to create host function");
        )*
    };
}

macro_rules! unimplemented_host_functions_return_type {
    (@return_type !) => {
        ()
    };
    (@return_type $ret:ty) => {
        $ret
    };
    (@return_type) => {
        ()
    };
}

#[derive(PartialEq, Eq, Hash, Clone, PartialOrd, Ord)]
#[near(serializers=[json, borsh])]
pub struct DexId {
    pub deployer: AccountId,
    pub id: String,
}

type DexPersistentStorage = HashMap<Vec<u8>, Vec<u8>>;

#[near(contract_state)]
pub struct SandboxedDexEngine {
    codes: LookupMap<DexId, Vec<u8>>,
    balances: LookupMap<(DexId, AssetId), U128>,
    storage: LookupMap<DexId, DexPersistentStorage>,
}

#[derive(BorshStorageKey)]
#[near(serializers=[borsh])]
enum StorageKey {
    DexBalances,
    DexStorage,
    DexCodes,
}

impl Default for SandboxedDexEngine {
    fn default() -> Self {
        Self {
            balances: LookupMap::new(StorageKey::DexBalances),
            storage: LookupMap::new(StorageKey::DexStorage),
            codes: LookupMap::new(StorageKey::DexCodes),
        }
    }
}

struct RunnerData<'a, Request, Response> {
    request: Request,
    response: Option<Response>,
    registers: HashMap<u64, Vec<u8>>,
    persistent_storage: &'a mut DexPersistentStorage,
    predecessor_id: AccountId,
}

#[near]
impl SandboxedDexEngine {
    pub fn deploy_code(&mut self, id: String, code: Vec<u8>) {
        let dex_id = DexId {
            deployer: near_sdk::env::predecessor_account_id(),
            id,
        };
        self.codes.insert(dex_id, code);
    }

    #[payable]
    pub fn swap(&mut self, dex_id: DexId) -> U128 {
        near_sdk::assert_one_yocto();

        let code = self.codes.get(&dex_id).expect("Dex code not found");
        let engine = Engine::default();
        let module = match Module::new(&engine, &code) {
            Ok(module) => module,
            Err(err) => panic!("Failed to load module: {err:?}"),
        };
        let swap_request = SwapRequest {
            pool_id: "1".to_string(),
            asset_in: AssetId::Near,
            asset_out: AssetId::Nep141("wrap.near".parse().unwrap()),
            amount: SwapRequestAmount::ExactIn(U128(1000000000000000000000000)),
        };
        let mut storage = HashMap::new();
        let mut store = Store::new(
            &engine,
            RunnerData::<SwapRequest, SwapResponse> {
                request: swap_request,
                response: None,
                registers: HashMap::new(),
                persistent_storage: &mut storage,
                predecessor_id: near_sdk::env::predecessor_account_id(),
            },
        );
        let mut linker = Linker::new(&engine);

        linker
            .func_wrap(
                "env",
                "register_len",
                |caller: Caller<'_, RunnerData<SwapRequest, SwapResponse>>, register_id: u64| {
                    caller
                        .data()
                        .registers
                        .get(&register_id)
                        .map(|v| v.len() as u64)
                        .unwrap_or(u64::MAX)
                },
            )
            .expect("Failed to create host function");
        linker
            .func_wrap(
                "env",
                "read_register",
                |mut caller: Caller<'_, RunnerData<SwapRequest, SwapResponse>>,
                 register_id: u64,
                 ptr: u64| {
                    let memory = caller
                        .get_export("memory")
                        .and_then(|m| m.into_memory())
                        .expect("Failed to get memory");
                    let buf = caller
                        .data()
                        .registers
                        .get(&register_id)
                        .expect("Invalid register")
                        .clone();
                    memory
                        .write(&mut caller, ptr as usize, &buf)
                        .expect("Failed to write data to guest memory");
                },
            )
            .expect("Failed to create host function");
        linker
            .func_wrap(
                "env",
                "write_register",
                |mut caller: Caller<'_, RunnerData<SwapRequest, SwapResponse>>,
                 register_id: u64,
                 data_len: u64,
                 data_ptr: u64| {
                    let memory = caller
                        .get_export("memory")
                        .and_then(|m| m.into_memory())
                        .expect("Failed to get memory");
                    let mut buf = vec![0; data_len as usize];
                    memory
                        .read(&caller, data_ptr as usize, &mut buf)
                        .expect("Failed to read data from guest memory");
                    caller.data_mut().registers.insert(register_id, buf);
                },
            )
            .expect("Failed to create host function");

        linker
            .func_wrap(
                "env",
                "input",
                |mut caller: Caller<'_, RunnerData<SwapRequest, SwapResponse>>,
                 register_id: u64| {
                    let buf = near_sdk::serde_json::json!({
                        "request": caller.data().request
                    })
                    .to_string()
                    .into_bytes();
                    caller.data_mut().registers.insert(register_id, buf);
                },
            )
            .expect("Failed to create host function");
        linker
            .func_wrap(
                "env",
                "attached_deposit",
                |mut caller: Caller<'_, RunnerData<SwapRequest, SwapResponse>>,
                 balance_ptr: u64| {
                    let attached_deposit = NearToken::default().as_yoctonear(); // always 0
                    let memory = caller
                        .get_export("memory")
                        .and_then(|m| m.into_memory())
                        .expect("Failed to get memory");
                    memory
                        .write(
                            &mut caller,
                            balance_ptr as usize,
                            &attached_deposit.to_le_bytes(),
                        )
                        .expect("Failed to write data to guest memory");
                },
            )
            .expect("Failed to create host function");
        linker
            .func_wrap(
                "env",
                "predecessor_account_id",
                |mut caller: Caller<'_, RunnerData<SwapRequest, SwapResponse>>,
                 register_id: u64| {
                    let buf = caller.data().predecessor_id.to_string().into_bytes();
                    caller.data_mut().registers.insert(register_id, buf);
                },
            )
            .expect("Failed to create host function");

        linker
            .func_wrap(
                "env",
                "value_return",
                |mut caller: Caller<'_, RunnerData<SwapRequest, SwapResponse>>,
                 value_len: u64,
                 value_ptr: u64| {
                    let memory = caller
                        .get_export("memory")
                        .and_then(|m| m.into_memory())
                        .expect("Failed to get memory");
                    let mut buf = vec![0; value_len as usize];
                    memory
                        .read(&caller, value_ptr as usize, &mut buf)
                        .expect("Failed to get return value");
                    let swap_response: SwapResponse = near_sdk::serde_json::from_slice(&buf)
                        .expect("Failed to parse return value as SwapResponse");
                    caller.data_mut().response = Some(swap_response);
                },
            )
            .expect("Failed to create host function");

        linker
            .func_wrap(
                "env",
                "panic",
                |mut caller: Caller<'_, RunnerData<SwapRequest, SwapResponse>>| {
                    near_sdk::env::log_str("panicked");
                    caller.data_mut().response = Some(SwapResponse::Error {
                        message: "panicked".to_string(),
                    });
                },
            )
            .expect("Failed to create host function");
        linker
            .func_wrap(
                "env",
                "panic_utf8",
                |mut caller: Caller<'_, RunnerData<SwapRequest, SwapResponse>>,
                 len: u64,
                 ptr: u64| {
                    let memory = caller
                        .get_export("memory")
                        .and_then(|m| m.into_memory())
                        .expect("Failed to get memory");
                    let mut buf = vec![0; len as usize];
                    memory
                        .read(&caller, ptr as usize, &mut buf)
                        .expect("Failed to read panic message");
                    let message = String::from_utf8(buf).expect("Failed to parse panic message");
                    near_sdk::env::log_str(&format!("panicked: {message}"));
                    caller.data_mut().response = Some(SwapResponse::Error { message });
                },
            )
            .expect("Failed to create host function");

        linker
            .func_wrap(
                "env",
                "storage_write",
                |mut caller: Caller<'_, RunnerData<SwapRequest, SwapResponse>>,
                 key_len: u64,
                 key_ptr: u64,
                 value_len: u64,
                 value_ptr: u64,
                 register_id: u64|
                 -> u64 {
                    let memory = caller
                        .get_export("memory")
                        .and_then(|m| m.into_memory())
                        .expect("Failed to get memory");
                    let mut key_buf = vec![0; key_len as usize];
                    memory
                        .read(&caller, key_ptr as usize, &mut key_buf)
                        .expect("Failed to read key from guest memory");
                    let mut value_buf = vec![0; value_len as usize];
                    memory
                        .read(&caller, value_ptr as usize, &mut value_buf)
                        .expect("Failed to read value from guest memory");
                    let old_value = caller
                        .data_mut()
                        .persistent_storage
                        .insert(key_buf, value_buf);

                    if let Some(old_val) = old_value {
                        caller.data_mut().registers.insert(register_id, old_val);
                        1
                    } else {
                        0
                    }
                },
            )
            .expect("Failed to create host function");
        linker
            .func_wrap(
                "env",
                "storage_read",
                |mut caller: Caller<'_, RunnerData<SwapRequest, SwapResponse>>,
                 key_len: u64,
                 key_ptr: u64,
                 register_id: u64|
                 -> u64 {
                    let memory = caller
                        .get_export("memory")
                        .and_then(|m| m.into_memory())
                        .expect("Failed to get memory");
                    let mut key_buf = vec![0; key_len as usize];
                    memory
                        .read(&caller, key_ptr as usize, &mut key_buf)
                        .expect("Failed to read key from guest memory");

                    if let Some(value) = caller.data().persistent_storage.get(&key_buf).cloned() {
                        caller.data_mut().registers.insert(register_id, value);
                        1
                    } else {
                        0
                    }
                },
            )
            .expect("Failed to create host function");
        linker
            .func_wrap(
                "env",
                "storage_remove",
                |mut caller: Caller<'_, RunnerData<SwapRequest, SwapResponse>>,
                 key_len: u64,
                 key_ptr: u64,
                 register_id: u64|
                 -> u64 {
                    let memory = caller
                        .get_export("memory")
                        .and_then(|m| m.into_memory())
                        .expect("Failed to get memory");
                    let mut key_buf = vec![0; key_len as usize];
                    memory
                        .read(&caller, key_ptr as usize, &mut key_buf)
                        .expect("Failed to read key from guest memory");

                    if let Some(old_value) = caller.data_mut().persistent_storage.remove(&key_buf) {
                        caller.data_mut().registers.insert(register_id, old_value);
                        1
                    } else {
                        0
                    }
                },
            )
            .expect("Failed to create host function");
        linker
            .func_wrap(
                "env",
                "storage_has_key",
                |caller: Caller<'_, RunnerData<SwapRequest, SwapResponse>>,
                 key_len: u64,
                 key_ptr: u64|
                 -> u64 {
                    let memory = caller
                        .get_export("memory")
                        .and_then(|m| m.into_memory())
                        .expect("Failed to get memory");
                    let mut key_buf = vec![0; key_len as usize];
                    memory
                        .read(&caller, key_ptr as usize, &mut key_buf)
                        .expect("Failed to read key from guest memory");

                    if caller.data().persistent_storage.contains_key(&key_buf) {
                        1
                    } else {
                        0
                    }
                },
            )
            .expect("Failed to create host function");

        declare_unimplemented_host_functions! {
            linker:

            // ###############
            // # Context API #
            // ###############
            pub fn current_account_id(register_id: u64);
            pub fn current_contract_code(register_id: u64) -> u64;
            pub fn refund_to_account_id(register_id: u64);
            pub fn signer_account_id(register_id: u64);
            pub fn signer_account_pk(register_id: u64);
            pub fn block_index() -> u64;
            pub fn block_timestamp() -> u64;
            pub fn epoch_height() -> u64;
            pub fn storage_usage() -> u64;
            // #################
            // # Economics API #
            // #################
            pub fn account_balance(balance_ptr: u64);
            pub fn account_locked_balance(balance_ptr: u64);
            pub fn prepaid_gas() -> u64;
            pub fn used_gas() -> u64;
            // ############
            // # Math API #
            // ############
            pub fn random_seed(register_id: u64);
            pub fn sha256(value_len: u64, value_ptr: u64, register_id: u64);
            pub fn keccak256(value_len: u64, value_ptr: u64, register_id: u64);
            pub fn keccak512(value_len: u64, value_ptr: u64, register_id: u64);
            pub fn ripemd160(value_len: u64, value_ptr: u64, register_id: u64);
            pub fn ecrecover(
                hash_len: u64,
                hash_ptr: u64,
                sig_len: u64,
                sig_ptr: u64,
                v: u64,
                malleability_flag: u64,
                register_id: u64,
            ) -> u64;
            pub fn ed25519_verify(
                sig_len: u64,
                sig_ptr: u64,
                msg_len: u64,
                msg_ptr: u64,
                pub_key_len: u64,
                pub_key_ptr: u64,
            ) -> u64;
            // #####################
            // # Miscellaneous API #
            // #####################
            pub fn log_utf8(len: u64, ptr: u64);
            pub fn log_utf16(len: u64, ptr: u64);
            pub fn abort(msg_ptr: u32, filename_ptr: u32, line: u32, col: u32) -> !;
            // ################
            // # Promises API #
            // ################
            pub fn promise_create(
                account_id_len: u64,
                account_id_ptr: u64,
                function_name_len: u64,
                function_name_ptr: u64,
                arguments_len: u64,
                arguments_ptr: u64,
                amount_ptr: u64,
                gas: u64,
            ) -> u64;
            pub fn promise_then(
                promise_index: u64,
                account_id_len: u64,
                account_id_ptr: u64,
                function_name_len: u64,
                function_name_ptr: u64,
                arguments_len: u64,
                arguments_ptr: u64,
                amount_ptr: u64,
                gas: u64,
            ) -> u64;
            pub fn promise_and(promise_idx_ptr: u64, promise_idx_count: u64) -> u64;
            pub fn promise_batch_create(account_id_len: u64, account_id_ptr: u64) -> u64;
            pub fn promise_batch_then(promise_index: u64, account_id_len: u64, account_id_ptr: u64) -> u64;
            // #######################
            // # Promise API actions #
            // #######################
            pub fn promise_set_refund_to(promise_index: u64, account_id_len: u64, account_id_ptr: u64);
            pub fn promise_batch_action_state_init(
                promise_index: u64,
                code_len: u64,
                code_ptr: u64,
                amount_ptr: u64,
            ) -> u64;
            pub fn promise_batch_action_state_init_by_account_id(
                promise_index: u64,
                account_id_len: u64,
                account_id_ptr: u64,
                amount_ptr: u64,
            ) -> u64;
            pub fn set_state_init_data_entry(
                promise_index: u64,
                action_index: u64,
                key_len: u64,
                key_ptr: u64,
                value_len: u64,
                value_ptr: u64,
            );
            pub fn promise_batch_action_create_account(promise_index: u64);
            pub fn promise_batch_action_deploy_contract(promise_index: u64, code_len: u64, code_ptr: u64);
            pub fn promise_batch_action_function_call(
                promise_index: u64,
                function_name_len: u64,
                function_name_ptr: u64,
                arguments_len: u64,
                arguments_ptr: u64,
                amount_ptr: u64,
                gas: u64,
            );
            pub fn promise_batch_action_function_call_weight(
                promise_index: u64,
                function_name_len: u64,
                function_name_ptr: u64,
                arguments_len: u64,
                arguments_ptr: u64,
                amount_ptr: u64,
                gas: u64,
                weight: u64,
            );
            pub fn promise_batch_action_transfer(promise_index: u64, amount_ptr: u64);
            pub fn promise_batch_action_stake(
                promise_index: u64,
                amount_ptr: u64,
                public_key_len: u64,
                public_key_ptr: u64,
            );
            pub fn promise_batch_action_add_key_with_full_access(
                promise_index: u64,
                public_key_len: u64,
                public_key_ptr: u64,
                nonce: u64,
            );
            pub fn promise_batch_action_add_key_with_function_call(
                promise_index: u64,
                public_key_len: u64,
                public_key_ptr: u64,
                nonce: u64,
                allowance_ptr: u64,
                receiver_id_len: u64,
                receiver_id_ptr: u64,
                function_names_len: u64,
                function_names_ptr: u64,
            );
            pub fn promise_batch_action_delete_key(
                promise_index: u64,
                public_key_len: u64,
                public_key_ptr: u64,
            );
            pub fn promise_batch_action_delete_account(
                promise_index: u64,
                beneficiary_id_len: u64,
                beneficiary_id_ptr: u64,
            );
            // #########################
            // # Global Contract API   #
            // #########################
            pub fn promise_batch_action_deploy_global_contract(
                promise_index: u64,
                code_len: u64,
                code_ptr: u64,
            );
            pub fn promise_batch_action_deploy_global_contract_by_account_id(
                promise_index: u64,
                code_len: u64,
                code_ptr: u64,
            );
            pub fn promise_batch_action_use_global_contract(
                promise_index: u64,
                code_hash_len: u64,
                code_hash_ptr: u64,
            );
            pub fn promise_batch_action_use_global_contract_by_account_id(
                promise_index: u64,
                account_id_len: u64,
                account_id_ptr: u64,
            );
            pub fn promise_yield_create(
                function_name_len: u64,
                function_name_ptr: u64,
                arguments_len: u64,
                arguments_ptr: u64,
                gas: u64,
                gas_weight: u64,
                register_id: u64,
            ) -> u64;
            pub fn promise_yield_resume(
                data_id_len: u64,
                data_id_ptr: u64,
                payload_len: u64,
                payload_ptr: u64,
            ) -> u32;
            // #######################
            // # Promise API results #
            // #######################
            pub fn promise_results_count() -> u64;
            pub fn promise_result(result_idx: u64, register_id: u64) -> u64;
            pub fn promise_return(promise_id: u64);
            // ###############
            // # Storage API #
            // ###############
            pub fn storage_iter_prefix(prefix_len: u64, prefix_ptr: u64) -> u64;
            pub fn storage_iter_range(start_len: u64, start_ptr: u64, end_len: u64, end_ptr: u64) -> u64;
            pub fn storage_iter_next(iterator_id: u64, key_register_id: u64, value_register_id: u64)
                -> u64;
            // ###############
            // # Validator API #
            // ###############
            pub fn validator_stake(account_id_len: u64, account_id_ptr: u64, stake_ptr: u64);
            pub fn validator_total_stake(stake_ptr: u64);
            // #############
            // # Alt BN128 #
            // #############
            pub fn alt_bn128_g1_multiexp(value_len: u64, value_ptr: u64, register_id: u64);
            pub fn alt_bn128_g1_sum(value_len: u64, value_ptr: u64, register_id: u64);
            pub fn alt_bn128_pairing_check(value_len: u64, value_ptr: u64) -> u64;

            // #############
            // # BLS12-381 #
            // #############
            pub fn bls12381_p1_sum(value_len: u64, value_ptr: u64, register_id: u64) -> u64;
            pub fn bls12381_p2_sum(value_len: u64, value_ptr: u64, register_id: u64) -> u64;
            pub fn bls12381_g1_multiexp(value_len: u64, value_ptr: u64, register_id: u64) -> u64;
            pub fn bls12381_g2_multiexp(value_len: u64, value_ptr: u64, register_id: u64) -> u64;
            pub fn bls12381_map_fp_to_g1(value_len: u64, value_ptr: u64, register_id: u64) -> u64;
            pub fn bls12381_map_fp2_to_g2(value_len: u64, value_ptr: u64, register_id: u64) -> u64;
            pub fn bls12381_pairing_check(value_len: u64, value_ptr: u64) -> u64;
            pub fn bls12381_p1_decompress(value_len: u64, value_ptr: u64, register_id: u64) -> u64;
            pub fn bls12381_p2_decompress(value_len: u64, value_ptr: u64, register_id: u64) -> u64;
        }

        let instance = match linker.instantiate_and_start(&mut store, &module) {
            Ok(i) => i,
            Err(err) => panic!("Failed to instantiate module: {err:?}"),
        };
        let swap_func: Func = match instance.get_func(&mut store, "swap") {
            Some(f) => f,
            None => panic!("Failed to get function"),
        };
        match swap_func.call(&mut store, &[], &mut []) {
            Ok(()) => (),
            Err(err) => panic!("Failed to call function: {err:?}"),
        };

        match &store.data().response {
            // TODO subtract amount_out from dex balance, add amount_in to dex balance, send amount_out to the trader
            Some(SwapResponse::Ok {
                amount_in: _,
                amount_out,
            }) => *amount_out,
            Some(SwapResponse::Error { message }) => panic!("Error returned from wasm: {message}"),
            None => panic!("No response from swap"),
        }
    }
}
