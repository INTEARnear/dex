use intear_dex_common::expect;
use near_sdk::{CryptoHash, Gas, GasWeight, NearToken, Promise, near};

const UPLOADED_CODE_KEY: &[u8] = b"uploaded_code";

#[near(contract_state)]
pub struct ChunkedUpgrade;

#[near]
impl ChunkedUpgrade {
    #[private]
    pub fn store_code_chunk(
        #[serializer(borsh)] offset: u32,
        #[serializer(borsh)] chunk: Vec<u8>,
        #[serializer(borsh)] code_hash_so_far: CryptoHash,
    ) {
        let mut code = near_sdk::env::storage_read(UPLOADED_CODE_KEY).unwrap_or_default();
        expect!(
            offset as usize <= code.len(),
            "Only {} bytes of the code are stored, so a chunk can't start at byte {offset}",
            code.len()
        );
        code.truncate(offset as usize);
        code.extend(chunk);
        expect!(
            near_sdk::env::sha256_array(&code) == code_hash_so_far,
            "The code stored so far doesn't have the hash it should have"
        );
        near_sdk::env::storage_write(UPLOADED_CODE_KEY, &code);
    }

    #[private]
    pub fn deploy_code_chunks(
        #[serializer(borsh)] offset: u32,
        #[serializer(borsh)] last_chunk: Vec<u8>,
        #[serializer(borsh)] code_hash: CryptoHash,
        #[serializer(borsh)] then_call: String,
        #[serializer(borsh)] then_call_args: Vec<u8>,
    ) -> Promise {
        Self::store_code_chunk(offset, last_chunk, code_hash);
        let code =
            near_sdk::env::storage_read(UPLOADED_CODE_KEY).expect("The code was just stored");
        near_sdk::env::storage_remove(UPLOADED_CODE_KEY);
        Promise::new(near_sdk::env::current_account_id())
            .deploy_contract(code)
            .function_call_weight(
                then_call,
                then_call_args,
                NearToken::from_yoctonear(0),
                Gas::from_gas(0),
                GasWeight(1),
            )
    }
}
