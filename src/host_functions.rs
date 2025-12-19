use wasmi::Caller;

use crate::{CallType, IntearDexEvent, RunnerData};
use near_sdk::NearToken;

#[macro_export]
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
                |_caller: ::wasmi::Caller<'_, RunnerData>, $(#[allow(unused_variables)] $arg: $arg_ty),*| -> $crate::unimplemented_host_functions_return_type!(@return_type $($ret)?) {
                    unimplemented!(concat!("Function ", stringify!($name), " is not implemented"))
                },
            )
            .expect("Failed to create host function");
        )*
    };
}

#[macro_export]
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

#[macro_export]
macro_rules! impl_unsupported_host_functions {
    ($var: ident) => {
        $crate::declare_unimplemented_host_functions! {
            $var:

            // ####################
            // # Unsupported APIs #
            // ####################
            pub fn current_account_id(register_id: u64);
            pub fn current_contract_code(register_id: u64) -> u64;
            pub fn refund_to_account_id(register_id: u64);
            pub fn signer_account_id(register_id: u64);
            pub fn signer_account_pk(register_id: u64);
            pub fn account_balance(balance_ptr: u64);
            pub fn account_locked_balance(balance_ptr: u64);
            pub fn validator_stake(account_id_len: u64, account_id_ptr: u64, stake_ptr: u64);
            pub fn validator_total_stake(stake_ptr: u64);
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
            pub fn promise_results_count() -> u64;
            pub fn promise_result(result_idx: u64, register_id: u64) -> u64;
            pub fn promise_return(promise_id: u64);
            // ##########################
            // # Deprecated Storage API #
            // ##########################
            pub fn storage_iter_prefix(prefix_len: u64, prefix_ptr: u64) -> u64;
            pub fn storage_iter_range(start_len: u64, start_ptr: u64, end_len: u64, end_ptr: u64) -> u64;
            pub fn storage_iter_next(iterator_id: u64, key_register_id: u64, value_register_id: u64)
                -> u64;
            // ###########################################
            // # Math that is not used by most contracts #
            // ###########################################
            pub fn alt_bn128_g1_multiexp(value_len: u64, value_ptr: u64, register_id: u64);
            pub fn alt_bn128_g1_sum(value_len: u64, value_ptr: u64, register_id: u64);
            pub fn alt_bn128_pairing_check(value_len: u64, value_ptr: u64) -> u64;
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
    };
}

#[macro_export]
macro_rules! impl_host_function {
    ($var: ident, $name: ident) => {
        $var.func_wrap("env", stringify!($name), $crate::host_functions::$name)
            .expect("Failed to create host function");
    };
}

#[macro_export]
macro_rules! impl_supported_host_functions {
    ($var: ident) => {
        $crate::impl_host_function!($var, register_len);
        $crate::impl_host_function!($var, read_register);
        $crate::impl_host_function!($var, write_register);
        $crate::impl_host_function!($var, input);
        $crate::impl_host_function!($var, attached_deposit);
        $crate::impl_host_function!($var, predecessor_account_id);
        $crate::impl_host_function!($var, value_return);
        $crate::impl_host_function!($var, panic);
        $crate::impl_host_function!($var, panic_utf8);
        $crate::impl_host_function!($var, storage_write);
        $crate::impl_host_function!($var, storage_read);
        $crate::impl_host_function!($var, storage_remove);
        $crate::impl_host_function!($var, storage_has_key);
        $crate::impl_host_function!($var, block_index);
        $crate::impl_host_function!($var, block_timestamp);
        $crate::impl_host_function!($var, epoch_height);
        $crate::impl_host_function!($var, storage_usage);
        $crate::impl_host_function!($var, prepaid_gas);
        $crate::impl_host_function!($var, used_gas);
        $crate::impl_host_function!($var, random_seed);
        $crate::impl_host_function!($var, sha256);
        $crate::impl_host_function!($var, keccak256);
        $crate::impl_host_function!($var, keccak512);
        $crate::impl_host_function!($var, ripemd160);
        $crate::impl_host_function!($var, ecrecover);
        $crate::impl_host_function!($var, ed25519_verify);
        $crate::impl_host_function!($var, log_utf8);
        $crate::impl_host_function!($var, log_utf16);
    };
}

pub fn register_len(caller: Caller<'_, RunnerData>, register_id: u64) -> u64 {
    caller
        .data()
        .registers
        .get(&register_id)
        .map(|v| v.len() as u64)
        .unwrap_or(u64::MAX)
}

pub fn read_register(mut caller: Caller<'_, RunnerData>, register_id: u64, ptr: u64) {
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
}

pub fn write_register(
    mut caller: Caller<'_, RunnerData>,
    register_id: u64,
    data_len: u64,
    data_ptr: u64,
) {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .expect("Failed to get memory");
    let mut buf = vec![0; data_len as usize];
    memory
        .read(&caller, data_ptr as usize, &mut buf)
        .expect("Failed to read data from guest memory");
    caller.data_mut().registers.insert(register_id, buf);
}

pub fn input(mut caller: Caller<'_, RunnerData>, register_id: u64) {
    let request = caller.data().request.clone();
    caller.data_mut().registers.insert(register_id, request);
}

pub fn attached_deposit(mut caller: Caller<'_, RunnerData>, balance_ptr: u64) {
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
}

pub fn predecessor_account_id(mut caller: Caller<'_, RunnerData>, register_id: u64) {
    let CallType::Call { predecessor_id, .. } = &caller.data().call_type else {
        panic!("predecessor_account_id is not allowed in view functions");
    };
    let buf = predecessor_id.to_string().into_bytes();
    caller.data_mut().registers.insert(register_id, buf);
}

pub fn value_return(mut caller: Caller<'_, RunnerData>, value_len: u64, value_ptr: u64) {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .expect("Failed to get memory");
    let mut buf = vec![0; value_len as usize];
    memory
        .read(&caller, value_ptr as usize, &mut buf)
        .expect("Failed to get return value");
    caller.data_mut().response = Some(buf);
}

pub fn panic(caller: Caller<'_, RunnerData>) {
    let dex_id = caller.data().dex_id.clone();
    panic!("[{dex_id}] Dex panicked");
}

pub fn panic_utf8(caller: Caller<'_, RunnerData>, len: u64, ptr: u64) {
    let dex_id = caller.data().dex_id.clone();
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .expect("Failed to get memory");
    let mut buf = vec![0; len as usize];
    memory
        .read(&caller, ptr as usize, &mut buf)
        .expect("Failed to read panic message");
    let message = String::from_utf8(buf).expect("Failed to parse panic message");
    panic!("[{dex_id}] Dex panicked: {message}");
}

pub fn storage_write(
    mut caller: Caller<'_, RunnerData>,
    key_len: u64,
    key_ptr: u64,
    value_len: u64,
    value_ptr: u64,
    register_id: u64,
) -> u64 {
    let dex_id = caller.data().dex_id.clone();
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

    let CallType::Call {
        dex_storage_mut, ..
    } = &mut caller.data_mut().call_type
    else {
        panic!("storage_write is not allowed in view functions");
    };
    let old_value = dex_storage_mut.insert((dex_id, key_buf), value_buf);

    if let Some(old_val) = old_value {
        caller.data_mut().registers.insert(register_id, old_val);
        1
    } else {
        0
    }
}

pub fn storage_read(
    mut caller: Caller<'_, RunnerData>,
    key_len: u64,
    key_ptr: u64,
    register_id: u64,
) -> u64 {
    let dex_id = caller.data().dex_id.clone();
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .expect("Failed to get memory");
    let mut key_buf = vec![0; key_len as usize];
    memory
        .read(&caller, key_ptr as usize, &mut key_buf)
        .expect("Failed to read key from guest memory");

    if let Some(value) = caller
        .data()
        .call_type
        .dex_storage()
        .get(&(dex_id, key_buf))
        .cloned()
    {
        caller.data_mut().registers.insert(register_id, value);
        1
    } else {
        0
    }
}

pub fn storage_remove(
    mut caller: Caller<'_, RunnerData>,
    key_len: u64,
    key_ptr: u64,
    register_id: u64,
) -> u64 {
    let dex_id = caller.data().dex_id.clone();
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .expect("Failed to get memory");
    let mut key_buf = vec![0; key_len as usize];
    memory
        .read(&caller, key_ptr as usize, &mut key_buf)
        .expect("Failed to read key from guest memory");

    let CallType::Call {
        dex_storage_mut, ..
    } = &mut caller.data_mut().call_type
    else {
        panic!("storage_write is not allowed in view functions");
    };
    if let Some(old_value) = dex_storage_mut.remove(&(dex_id, key_buf)) {
        caller.data_mut().registers.insert(register_id, old_value);
        1
    } else {
        0
    }
}

pub fn storage_has_key(caller: Caller<'_, RunnerData>, key_len: u64, key_ptr: u64) -> u64 {
    let dex_id = caller.data().dex_id.clone();
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .expect("Failed to get memory");
    let mut key_buf = vec![0; key_len as usize];
    memory
        .read(&caller, key_ptr as usize, &mut key_buf)
        .expect("Failed to read key from guest memory");

    if caller
        .data()
        .call_type
        .dex_storage()
        .contains_key(&(dex_id, key_buf))
    {
        1
    } else {
        0
    }
}

pub fn block_index(_caller: Caller<'_, RunnerData>) -> u64 {
    near_sdk::env::block_height()
}

pub fn block_timestamp(_caller: Caller<'_, RunnerData>) -> u64 {
    near_sdk::env::block_timestamp()
}

pub fn epoch_height(_caller: Caller<'_, RunnerData>) -> u64 {
    near_sdk::env::epoch_height()
}

pub fn storage_usage(mut caller: Caller<'_, RunnerData>) -> u64 {
    if let CallType::Call {
        dex_storage_mut, ..
    } = &mut caller.data_mut().call_type
    {
        dex_storage_mut.flush();
    };
    let storage_usage_now = near_sdk::env::storage_usage();
    let storage_usage_during_transaction = i64::try_from(storage_usage_now)
        .expect("Storage usage overflow")
        .checked_sub(
            i64::try_from(caller.data().dex_storage_usage_before_transaction)
                .expect("Storage usage overflow"),
        )
        .expect("Storage usage underflow");
    let data_used_before_transaction = caller
        .data()
        .dex_storage_balances
        .get_bytes_used(&caller.data().dex_id);
    i64::try_from(data_used_before_transaction)
        .expect("Data used before transaction overflow")
        .checked_add(storage_usage_during_transaction)
        .expect("Result of storage usage calculation is not within i64 range")
        .try_into()
        .expect("Result of storage usage calculation is not within u64 range")
}

pub fn prepaid_gas(_caller: Caller<'_, RunnerData>) -> u64 {
    near_sdk::env::prepaid_gas().as_gas()
}

pub fn used_gas(_caller: Caller<'_, RunnerData>) -> u64 {
    near_sdk::env::used_gas().as_gas()
}

pub fn random_seed(mut caller: Caller<'_, RunnerData>, register_id: u64) {
    let seed = near_sdk::env::random_seed();
    caller
        .data_mut()
        .registers
        .insert(register_id, seed.to_vec());
}

pub fn sha256(
    mut caller: Caller<'_, RunnerData>,
    value_len: u64,
    value_ptr: u64,
    register_id: u64,
) {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .expect("Failed to get memory");
    let mut value_buf = vec![0; value_len as usize];
    memory
        .read(&caller, value_ptr as usize, &mut value_buf)
        .expect("Failed to read value from guest memory");
    let hash = near_sdk::env::sha256_array(&value_buf);
    caller
        .data_mut()
        .registers
        .insert(register_id, hash.to_vec());
}

pub fn keccak256(
    mut caller: Caller<'_, RunnerData>,
    value_len: u64,
    value_ptr: u64,
    register_id: u64,
) {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .expect("Failed to get memory");
    let mut value_buf = vec![0; value_len as usize];
    memory
        .read(&caller, value_ptr as usize, &mut value_buf)
        .expect("Failed to read value from guest memory");
    let hash = near_sdk::env::keccak256_array(&value_buf);
    caller
        .data_mut()
        .registers
        .insert(register_id, hash.to_vec());
}

pub fn keccak512(
    mut caller: Caller<'_, RunnerData>,
    value_len: u64,
    value_ptr: u64,
    register_id: u64,
) {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .expect("Failed to get memory");
    let mut value_buf = vec![0; value_len as usize];
    memory
        .read(&caller, value_ptr as usize, &mut value_buf)
        .expect("Failed to read value from guest memory");
    let hash = near_sdk::env::keccak512_array(&value_buf);
    caller
        .data_mut()
        .registers
        .insert(register_id, hash.to_vec());
}

pub fn ripemd160(
    mut caller: Caller<'_, RunnerData>,
    value_len: u64,
    value_ptr: u64,
    register_id: u64,
) {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .expect("Failed to get memory");
    let mut value_buf = vec![0; value_len as usize];
    memory
        .read(&caller, value_ptr as usize, &mut value_buf)
        .expect("Failed to read value from guest memory");
    let hash = near_sdk::env::ripemd160_array(&value_buf);
    caller
        .data_mut()
        .registers
        .insert(register_id, hash.to_vec());
}

#[allow(clippy::too_many_arguments)]
pub fn ecrecover(
    mut caller: Caller<'_, RunnerData>,
    hash_len: u64,
    hash_ptr: u64,
    sig_len: u64,
    sig_ptr: u64,
    v: u64,
    malleability_flag: u64,
    register_id: u64,
) -> u64 {
    if v >= 4 {
        panic!("Invalid recovery ID passed to ecrecover: {v}");
    }
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .expect("Failed to get memory");
    let mut hash_buf = vec![0; hash_len as usize];
    memory
        .read(&caller, hash_ptr as usize, &mut hash_buf)
        .expect("Failed to read hash from guest memory");
    let mut sig_buf = vec![0; sig_len as usize];
    memory
        .read(&caller, sig_ptr as usize, &mut sig_buf)
        .expect("Failed to read signature from guest memory");

    let maybe_public_key = near_sdk::env::ecrecover(
        &hash_buf,
        &sig_buf,
        v as u8,
        match malleability_flag {
            0 => false,
            1 => true,
            _ => panic!("Invalid malleability flag passed to ecrecover: {malleability_flag}"),
        },
    );
    if let Some(public_key) = maybe_public_key {
        caller
            .data_mut()
            .registers
            .insert(register_id, public_key.to_vec());
        1
    } else {
        0
    }
}

pub fn ed25519_verify(
    caller: Caller<'_, RunnerData>,
    signature_len: u64,
    signature_ptr: u64,
    message_len: u64,
    message_ptr: u64,
    public_key_len: u64,
    public_key_ptr: u64,
) -> u64 {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .expect("Failed to get memory");
    if signature_len != 64 || public_key_len != 32 {
        return 0;
    }
    let mut sig_buf = [0u8; 64];
    memory
        .read(&caller, signature_ptr as usize, &mut sig_buf)
        .expect("Failed to read signature from guest memory");
    let mut msg_buf = vec![0; message_len as usize];
    memory
        .read(&caller, message_ptr as usize, &mut msg_buf)
        .expect("Failed to read message from guest memory");
    let mut pub_key_buf = [0u8; 32];
    memory
        .read(&caller, public_key_ptr as usize, &mut pub_key_buf)
        .expect("Failed to read public key from guest memory");
    if near_sdk::env::ed25519_verify(&sig_buf, &msg_buf, &pub_key_buf) {
        1
    } else {
        0
    }
}

pub fn log_utf8(caller: Caller<'_, RunnerData>, len: u64, ptr: u64) {
    let dex_id = caller.data().dex_id.clone();
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .expect("Failed to get memory");
    let msg_bytes = if len == u64::MAX {
        panic!("log_utf8: unterminated log strings are not supported");
    } else {
        let mut buf = vec![0; len as usize];
        memory
            .read(&caller, ptr as usize, &mut buf)
            .expect("Failed to read log_utf8 buffer from guest memory");
        buf
    };
    let message = String::from_utf8(msg_bytes).expect("log_utf8 received invalid UTF-8");
    if let Some(event) = message.strip_prefix("EVENT_JSON:") {
        if let Ok(event) = near_sdk::serde_json::from_str(event) {
            IntearDexEvent::DexEvent {
                dex_id: caller.data().dex_id.clone(),
                event,
            }
            .emit();
            return;
        }
    }

    near_sdk::env::log_str(&format!("[{dex_id}] {message}"));
}

pub fn log_utf16(caller: Caller<'_, RunnerData>, len: u64, ptr: u64) {
    let dex_id = caller.data().dex_id.clone();
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .expect("Failed to get memory");
    let utf16: Vec<u16> = if len == u64::MAX {
        panic!("log_utf16: unterminated log strings are not supported");
    } else {
        if len % 2 != 0 {
            panic!("log_utf16 length must be even (u16 units)");
        }
        let mut buf = vec![0; len as usize];
        memory
            .read(&caller, ptr as usize, &mut buf)
            .expect("Failed to read log_utf16 buffer from guest memory");
        buf.chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect()
    };
    let message = String::from_utf16(&utf16).expect("log_utf16 received invalid UTF-16");
    near_sdk::env::log_str(&format!("[{dex_id}] {message}"));
}
