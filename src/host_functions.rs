use wasmi::Caller;

use crate::{CallType, RunnerData};
use intear_dex_types::DexEventData;
use near_sdk::{AccountId, PublicKeyHandle};

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
            .map_err(|err| panic!("Failed to create host function: {err}"))
            .ok();
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
        $crate::impl_host_function!($var, caller);
        $crate::impl_host_function!($var, dex_id);
        $crate::impl_host_function!($var, engine_contract_id);
        $crate::impl_host_function!($var, chain_id);
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
        $crate::impl_host_function!($var, sha3_256);
        $crate::impl_host_function!($var, sha3_384);
        $crate::impl_host_function!($var, sha3_512);
        $crate::impl_host_function!($var, ripemd160);
        $crate::impl_host_function!($var, ecrecover);
        $crate::impl_host_function!($var, ed25519_verify);
        $crate::impl_host_function!($var, p256_verify);
        $crate::impl_host_function!($var, ml_dsa_verify);
        $crate::impl_host_function!($var, dex_event);
        $crate::impl_host_function!($var, oracle_request);
    };
}

pub fn register_len(caller: Caller<'_, RunnerData>, register_id: u64) -> Result<u64, wasmi::Error> {
    Ok(caller
        .data()
        .registers
        .get(&register_id)
        .map(|v| v.len() as u64)
        .unwrap_or(u64::MAX))
}

pub fn read_register(
    mut caller: Caller<'_, RunnerData>,
    register_id: u64,
    ptr: u64,
) -> Result<(), wasmi::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let buf = caller
        .data()
        .registers
        .get(&register_id)
        .ok_or_else(|| wasmi::Error::new("Invalid register"))?
        .clone();
    memory
        .write(&mut caller, ptr as usize, &buf)
        .map_err(|err| wasmi::Error::new(format!("Failed to write data to guest memory: {err}")))?;
    Ok(())
}

pub fn write_register(
    mut caller: Caller<'_, RunnerData>,
    register_id: u64,
    data_len: u64,
    data_ptr: u64,
) -> Result<(), wasmi::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut buf = vec![0; data_len as usize];
    memory
        .read(&caller, data_ptr as usize, &mut buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read data from guest memory: {err}"))
        })?;
    caller.data_mut().registers.insert(register_id, buf);
    Ok(())
}

pub fn input(mut caller: Caller<'_, RunnerData>, register_id: u64) -> Result<(), wasmi::Error> {
    let request = caller.data().request.clone();
    caller.data_mut().registers.insert(register_id, request);
    Ok(())
}

pub fn caller(mut caller: Caller<'_, RunnerData>, register_id: u64) -> Result<(), wasmi::Error> {
    let trader = match &caller.data().call_type {
        CallType::Call { caller: trader, .. } | CallType::Trade { trader, .. } => trader.clone(),
        CallType::View { .. } => {
            return Err(wasmi::Error::new(
                "trader is only allowed in trades or call functions",
            ));
        }
    };
    let buf = near_sdk::borsh::to_vec(&trader)
        .map_err(|err| wasmi::Error::new(format!("Failed to serialize trader: {err}")))?;
    caller.data_mut().registers.insert(register_id, buf);
    Ok(())
}

pub fn dex_id(mut caller: Caller<'_, RunnerData>, register_id: u64) -> Result<(), wasmi::Error> {
    let dex_id = near_sdk::borsh::to_vec(&caller.data().dex_id)
        .map_err(|err| wasmi::Error::new(format!("Failed to serialize dex id: {err}")))?;
    caller.data_mut().registers.insert(register_id, dex_id);
    Ok(())
}

pub fn engine_contract_id(
    mut caller: Caller<'_, RunnerData>,
    register_id: u64,
) -> Result<(), wasmi::Error> {
    let engine_contract_id = near_sdk::env::current_account_id().to_string().into_bytes();
    caller
        .data_mut()
        .registers
        .insert(register_id, engine_contract_id);
    Ok(())
}

pub fn chain_id(mut caller: Caller<'_, RunnerData>, register_id: u64) -> Result<(), wasmi::Error> {
    let chain_id = near_sdk::env::chain_id().into_bytes();
    caller.data_mut().registers.insert(register_id, chain_id);
    Ok(())
}

pub fn value_return(
    mut caller: Caller<'_, RunnerData>,
    value_len: u64,
    value_ptr: u64,
) -> Result<(), wasmi::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut buf = vec![0; value_len as usize];
    memory
        .read(&caller, value_ptr as usize, &mut buf)
        .map_err(|err| wasmi::Error::new(format!("Failed to get return value: {err}")))?;
    caller.data_mut().response = Some(buf);
    Ok(())
}

pub fn panic(caller: Caller<'_, RunnerData>) -> Result<(), wasmi::Error> {
    let dex_id = caller.data().dex_id.clone();
    Err(wasmi::Error::new(format!("[{dex_id}] Dex panicked")))
}

pub fn panic_utf8(caller: Caller<'_, RunnerData>, len: u64, ptr: u64) -> Result<(), wasmi::Error> {
    let dex_id = caller.data().dex_id.clone();
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut buf = vec![0; len as usize];
    memory
        .read(&caller, ptr as usize, &mut buf)
        .map_err(|err| wasmi::Error::new(format!("Failed to read panic message: {err}")))?;
    let message = String::from_utf8(buf)
        .map_err(|err| wasmi::Error::new(format!("Failed to parse panic message: {err}")))?;
    Err(wasmi::Error::new(format!(
        "[{dex_id}] Dex panicked: {message}"
    )))
}

pub fn storage_write(
    mut caller: Caller<'_, RunnerData>,
    key_len: u64,
    key_ptr: u64,
    value_len: u64,
    value_ptr: u64,
    register_id: u64,
) -> Result<u64, wasmi::Error> {
    let dex_id = caller.data().dex_id.clone();
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut key_buf = vec![0; key_len as usize];
    memory
        .read(&caller, key_ptr as usize, &mut key_buf)
        .map_err(|err| wasmi::Error::new(format!("Failed to read key from guest memory: {err}")))?;
    let mut value_buf = vec![0; value_len as usize];
    memory
        .read(&caller, value_ptr as usize, &mut value_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read value from guest memory: {err}"))
        })?;

    let old_value = caller
        .data_mut()
        .call_type
        .dex_storage_insert(dex_id, key_buf, value_buf)?;

    if let Some(old_val) = old_value {
        caller.data_mut().registers.insert(register_id, old_val);
        Ok(1)
    } else {
        Ok(0)
    }
}

pub fn storage_read(
    mut caller: Caller<'_, RunnerData>,
    key_len: u64,
    key_ptr: u64,
    register_id: u64,
) -> Result<u64, wasmi::Error> {
    let dex_id = caller.data().dex_id.clone();
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut key_buf = vec![0; key_len as usize];
    memory
        .read(&caller, key_ptr as usize, &mut key_buf)
        .map_err(|err| wasmi::Error::new(format!("Failed to read key from guest memory: {err}")))?;

    if let Some(value) = caller
        .data()
        .call_type
        .dex_storage_get(dex_id, key_buf)
        .cloned()
    {
        caller.data_mut().registers.insert(register_id, value);
        Ok(1)
    } else {
        Ok(0)
    }
}

pub fn storage_remove(
    mut caller: Caller<'_, RunnerData>,
    key_len: u64,
    key_ptr: u64,
    register_id: u64,
) -> Result<u64, wasmi::Error> {
    let dex_id = caller.data().dex_id.clone();
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut key_buf = vec![0; key_len as usize];
    memory
        .read(&caller, key_ptr as usize, &mut key_buf)
        .map_err(|err| wasmi::Error::new(format!("Failed to read key from guest memory: {err}")))?;

    if let Some(old_value) = caller
        .data_mut()
        .call_type
        .dex_storage_remove(dex_id, key_buf)?
    {
        caller.data_mut().registers.insert(register_id, old_value);
        Ok(1)
    } else {
        Ok(0)
    }
}

pub fn storage_has_key(
    caller: Caller<'_, RunnerData>,
    key_len: u64,
    key_ptr: u64,
) -> Result<u64, wasmi::Error> {
    let dex_id = caller.data().dex_id.clone();
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut key_buf = vec![0; key_len as usize];
    memory
        .read(&caller, key_ptr as usize, &mut key_buf)
        .map_err(|err| wasmi::Error::new(format!("Failed to read key from guest memory: {err}")))?;

    if caller
        .data()
        .call_type
        .dex_storage_contains_key(dex_id, key_buf)
    {
        Ok(1)
    } else {
        Ok(0)
    }
}

pub fn block_index(_caller: Caller<'_, RunnerData>) -> Result<u64, wasmi::Error> {
    Ok(near_sdk::env::block_height())
}

pub fn block_timestamp(_caller: Caller<'_, RunnerData>) -> Result<u64, wasmi::Error> {
    Ok(near_sdk::env::block_timestamp())
}

pub fn epoch_height(_caller: Caller<'_, RunnerData>) -> Result<u64, wasmi::Error> {
    Ok(near_sdk::env::epoch_height())
}

pub fn storage_usage(mut caller: Caller<'_, RunnerData>) -> Result<u64, wasmi::Error> {
    caller.data_mut().call_type.dex_storage_flush();
    let storage_usage_during_transaction = i64::try_from(near_sdk::env::storage_usage())
        .map_err(|err| wasmi::Error::new(format!("Storage usage overflow: {err}")))?
        .checked_sub(
            i64::try_from(caller.data().dex_storage_usage_before_transaction)
                .map_err(|err| wasmi::Error::new(format!("Storage usage overflow: {err}")))?,
        )
        .ok_or_else(|| wasmi::Error::new("Storage usage underflow"))?
        .checked_add(
            caller
                .data()
                .call_type
                .pending_storage_usage(&caller.data().dex_id)?,
        )
        .ok_or_else(|| wasmi::Error::new("Storage usage overflow"))?;
    let data_used_before_transaction = caller
        .data()
        .dex_storage_balances
        .get_bytes_used(&caller.data().dex_id);
    let total_usage = i64::try_from(data_used_before_transaction)
        .map_err(|err| wasmi::Error::new(format!("Data used before transaction overflow: {err}")))?
        .checked_add(storage_usage_during_transaction)
        .ok_or_else(|| {
            wasmi::Error::new("Result of storage usage calculation is not within i64 range")
        })?;
    total_usage.try_into().map_err(|err| {
        wasmi::Error::new(format!(
            "Result of storage usage calculation is not within u64 range: {err}"
        ))
    })
}

pub fn prepaid_gas(_caller: Caller<'_, RunnerData>) -> Result<u64, wasmi::Error> {
    Ok(near_sdk::env::prepaid_gas().as_gas())
}

pub fn used_gas(_caller: Caller<'_, RunnerData>) -> Result<u64, wasmi::Error> {
    Ok(near_sdk::env::used_gas().as_gas())
}

pub fn random_seed(
    mut caller: Caller<'_, RunnerData>,
    register_id: u64,
) -> Result<(), wasmi::Error> {
    let seed = near_sdk::env::random_seed();
    caller
        .data_mut()
        .registers
        .insert(register_id, seed.to_vec());
    Ok(())
}

pub fn sha256(
    mut caller: Caller<'_, RunnerData>,
    value_len: u64,
    value_ptr: u64,
    register_id: u64,
) -> Result<(), wasmi::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut value_buf = vec![0; value_len as usize];
    memory
        .read(&caller, value_ptr as usize, &mut value_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read value from guest memory: {err}"))
        })?;
    let hash = near_sdk::env::sha256_array(&value_buf);
    caller
        .data_mut()
        .registers
        .insert(register_id, hash.to_vec());
    Ok(())
}

pub fn keccak256(
    mut caller: Caller<'_, RunnerData>,
    value_len: u64,
    value_ptr: u64,
    register_id: u64,
) -> Result<(), wasmi::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut value_buf = vec![0; value_len as usize];
    memory
        .read(&caller, value_ptr as usize, &mut value_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read value from guest memory: {err}"))
        })?;
    let hash = near_sdk::env::keccak256_array(&value_buf);
    caller
        .data_mut()
        .registers
        .insert(register_id, hash.to_vec());
    Ok(())
}

pub fn keccak512(
    mut caller: Caller<'_, RunnerData>,
    value_len: u64,
    value_ptr: u64,
    register_id: u64,
) -> Result<(), wasmi::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut value_buf = vec![0; value_len as usize];
    memory
        .read(&caller, value_ptr as usize, &mut value_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read value from guest memory: {err}"))
        })?;
    let hash = near_sdk::env::keccak512_array(&value_buf);
    caller
        .data_mut()
        .registers
        .insert(register_id, hash.to_vec());
    Ok(())
}

pub fn sha3_256(
    mut caller: Caller<'_, RunnerData>,
    value_len: u64,
    value_ptr: u64,
    register_id: u64,
) -> Result<(), wasmi::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut value_buf = vec![0; value_len as usize];
    memory
        .read(&caller, value_ptr as usize, &mut value_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read value from guest memory: {err}"))
        })?;
    let hash = near_sdk::env::sha3_256(&value_buf);
    caller
        .data_mut()
        .registers
        .insert(register_id, hash.to_vec());
    Ok(())
}

pub fn sha3_384(
    mut caller: Caller<'_, RunnerData>,
    value_len: u64,
    value_ptr: u64,
    register_id: u64,
) -> Result<(), wasmi::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut value_buf = vec![0; value_len as usize];
    memory
        .read(&caller, value_ptr as usize, &mut value_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read value from guest memory: {err}"))
        })?;
    let hash = near_sdk::env::sha3_384(&value_buf);
    caller
        .data_mut()
        .registers
        .insert(register_id, hash.to_vec());
    Ok(())
}

pub fn sha3_512(
    mut caller: Caller<'_, RunnerData>,
    value_len: u64,
    value_ptr: u64,
    register_id: u64,
) -> Result<(), wasmi::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut value_buf = vec![0; value_len as usize];
    memory
        .read(&caller, value_ptr as usize, &mut value_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read value from guest memory: {err}"))
        })?;
    let hash = near_sdk::env::sha3_512(&value_buf);
    caller
        .data_mut()
        .registers
        .insert(register_id, hash.to_vec());
    Ok(())
}

pub fn ripemd160(
    mut caller: Caller<'_, RunnerData>,
    value_len: u64,
    value_ptr: u64,
    register_id: u64,
) -> Result<(), wasmi::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut value_buf = vec![0; value_len as usize];
    memory
        .read(&caller, value_ptr as usize, &mut value_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read value from guest memory: {err}"))
        })?;
    let hash = near_sdk::env::ripemd160_array(&value_buf);
    caller
        .data_mut()
        .registers
        .insert(register_id, hash.to_vec());
    Ok(())
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
) -> Result<u64, wasmi::Error> {
    if v >= 4 {
        return Err(wasmi::Error::new(format!(
            "Invalid recovery ID passed to ecrecover: {v}"
        )));
    }
    if hash_len != 32 || sig_len != 64 {
        return Err(wasmi::Error::new(
            "ecrecover takes a 32-byte hash and a 64-byte signature",
        ));
    }
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut hash_buf = vec![0; hash_len as usize];
    memory
        .read(&caller, hash_ptr as usize, &mut hash_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read hash from guest memory: {err}"))
        })?;
    let mut sig_buf = vec![0; sig_len as usize];
    memory
        .read(&caller, sig_ptr as usize, &mut sig_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read signature from guest memory: {err}"))
        })?;

    let maybe_public_key = near_sdk::env::ecrecover(
        &hash_buf,
        &sig_buf,
        v as u8,
        match malleability_flag {
            0 => false,
            1 => true,
            _ => {
                return Err(wasmi::Error::new(format!(
                    "Invalid malleability flag passed to ecrecover: {malleability_flag}"
                )));
            }
        },
    );
    if let Some(public_key) = maybe_public_key {
        caller
            .data_mut()
            .registers
            .insert(register_id, public_key.to_vec());
        Ok(1)
    } else {
        Ok(0)
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
) -> Result<u64, wasmi::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    if signature_len != 64 || public_key_len != 32 {
        return Ok(0);
    }
    let mut sig_buf = [0u8; 64];
    memory
        .read(&caller, signature_ptr as usize, &mut sig_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read signature from guest memory: {err}"))
        })?;
    let mut msg_buf = vec![0; message_len as usize];
    memory
        .read(&caller, message_ptr as usize, &mut msg_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read message from guest memory: {err}"))
        })?;
    let mut pub_key_buf = [0u8; 32];
    memory
        .read(&caller, public_key_ptr as usize, &mut pub_key_buf)
        .map_err(|err| {
            wasmi::Error::new(format!(
                "Failed to read public key from guest memory: {err}"
            ))
        })?;
    if near_sdk::env::ed25519_verify(&sig_buf, &msg_buf, &pub_key_buf) {
        Ok(1)
    } else {
        Ok(0)
    }
}

pub fn p256_verify(
    caller: Caller<'_, RunnerData>,
    signature_len: u64,
    signature_ptr: u64,
    message_len: u64,
    message_ptr: u64,
    public_key_len: u64,
    public_key_ptr: u64,
) -> Result<u64, wasmi::Error> {
    if signature_len != 64 || public_key_len != 33 {
        return Err(wasmi::Error::new(
            "p256_verify takes a 64-byte signature and a 33-byte compressed public key",
        ));
    }
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut sig_buf = [0u8; 64];
    memory
        .read(&caller, signature_ptr as usize, &mut sig_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read signature from guest memory: {err}"))
        })?;
    let mut msg_buf = vec![0; message_len as usize];
    memory
        .read(&caller, message_ptr as usize, &mut msg_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read message from guest memory: {err}"))
        })?;
    let mut pub_key_buf = [0u8; 33];
    memory
        .read(&caller, public_key_ptr as usize, &mut pub_key_buf)
        .map_err(|err| {
            wasmi::Error::new(format!(
                "Failed to read public key from guest memory: {err}"
            ))
        })?;
    Ok(unsafe {
        near_sdk::sys::p256_verify(
            sig_buf.len() as u64,
            sig_buf.as_ptr() as u64,
            msg_buf.len() as u64,
            msg_buf.as_ptr() as u64,
            pub_key_buf.len() as u64,
            pub_key_buf.as_ptr() as u64,
        )
    })
}

pub fn ml_dsa_verify(
    caller: Caller<'_, RunnerData>,
    signature_len: u64,
    signature_ptr: u64,
    message_len: u64,
    message_ptr: u64,
    public_key_len: u64,
    public_key_ptr: u64,
) -> Result<u64, wasmi::Error> {
    // NEAR would abort the whole call
    if signature_len != 3309 || public_key_len != 1952 {
        return Err(wasmi::Error::new(
            "ml_dsa_verify takes a 3309-byte signature and a 1952-byte public key",
        ));
    }
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut sig_buf = [0u8; 3309];
    memory
        .read(&caller, signature_ptr as usize, &mut sig_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read signature from guest memory: {err}"))
        })?;
    let mut msg_buf = vec![0; message_len as usize];
    memory
        .read(&caller, message_ptr as usize, &mut msg_buf)
        .map_err(|err| {
            wasmi::Error::new(format!("Failed to read message from guest memory: {err}"))
        })?;
    let mut pub_key_buf = [0u8; 1952];
    memory
        .read(&caller, public_key_ptr as usize, &mut pub_key_buf)
        .map_err(|err| {
            wasmi::Error::new(format!(
                "Failed to read public key from guest memory: {err}"
            ))
        })?;
    if near_sdk::env::ml_dsa_verify(&sig_buf, &msg_buf, &pub_key_buf) {
        Ok(1)
    } else {
        Ok(0)
    }
}

pub fn dex_event(
    mut caller: Caller<'_, RunnerData>,
    len: u64,
    ptr: u64,
) -> Result<(), wasmi::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut event = vec![0; len as usize];
    memory
        .read(&caller, ptr as usize, &mut event)
        .map_err(|err| {
            wasmi::Error::new(format!(
                "Failed to read dex_event buffer from guest memory: {err}"
            ))
        })?;
    let dex_event = DexEventData {
        dex_id: caller.data().dex_id.clone(),
        event,
        referrer: caller.data().referrer.clone(),
        user: alleged_event_user(caller.data()),
    };
    caller.data_mut().dex_events.push(dex_event);
    Ok(())
}

pub fn oracle_request(
    mut caller: Caller<'_, RunnerData>,
    oracle_key_len: u64,
    oracle_key_ptr: u64,
    request_len: u64,
    request_ptr: u64,
    register_id: u64,
) -> Result<u64, wasmi::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|m| m.into_memory())
        .ok_or_else(|| wasmi::Error::new("Failed to get memory"))?;
    let mut oracle_key = vec![0; oracle_key_len as usize];
    memory
        .read(&caller, oracle_key_ptr as usize, &mut oracle_key)
        .map_err(|err| {
            wasmi::Error::new(format!(
                "Failed to read oracle key from guest memory: {err}"
            ))
        })?;
    let oracle_key: PublicKeyHandle = near_sdk::borsh::from_slice(&oracle_key)
        .map_err(|err| wasmi::Error::new(format!("Invalid oracle key: {err}")))?;
    let mut request = vec![0; request_len as usize];
    memory
        .read(&caller, request_ptr as usize, &mut request)
        .map_err(|err| {
            wasmi::Error::new(format!(
                "Failed to read oracle request from guest memory: {err}"
            ))
        })?;
    let Some(oracle_response) = caller.data().oracle_response.filter(|oracle_response| {
        oracle_response.oracle_key == oracle_key
            && oracle_response.dex_id == caller.data().dex_id
            && oracle_response.request == request
    }) else {
        return Ok(0);
    };
    let response = oracle_response.response.clone();
    caller.data_mut().registers.insert(register_id, response);
    Ok(1)
}

fn alleged_event_user(data: &RunnerData) -> Option<AccountId> {
    match &data.call_type {
        CallType::Call { caller: trader, .. } | CallType::Trade { trader, .. } => {
            Some(trader.alleged_account_id().clone())
        }
        CallType::View { .. } => None,
    }
}
