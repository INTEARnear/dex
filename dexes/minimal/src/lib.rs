#![no_std]
#![deny(clippy::arithmetic_side_effects)]

extern crate alloc;
use alloc::{vec, vec::Vec};
use intear_dex_types::{SwapRequest, SwapRequestAmount, SwapResponse};

#[global_allocator]
static ALLOCATOR: talc::Talck<talc::locking::AssumeUnlockable, talc::ClaimOnOom> = {
    static mut MEMORY: [u8; 0x1000] = [0; 0x1000]; // 4KB
    let span = talc::Span::from_array(core::ptr::addr_of!(MEMORY).cast_mut());
    talc::Talc::new(unsafe { talc::ClaimOnOom::new(span) }).lock()
};

mod sys {
    #[cfg_attr(target_arch = "wasm32", link(wasm_import_module = "env"))]
    unsafe extern "C" {
        pub fn value_return(value_len: u64, value_ptr: u64);
        pub fn input(register_id: u64);
        pub fn register_len(register_id: u64) -> u64;
        pub fn read_register(register_id: u64, ptr: u64);
    }
}

fn return_value(value: impl AsRef<[u8]>) {
    let value = value.as_ref();
    unsafe {
        sys::value_return(value.len() as u64, value.as_ptr() as u64);
    }
}

const ATOMIC_REGISTER_ID: u64 = u64::MAX;

fn read(load: unsafe extern "C" fn(u64)) -> Vec<u8> {
    unsafe { load(ATOMIC_REGISTER_ID) };
    let len = unsafe { sys::register_len(ATOMIC_REGISTER_ID) };
    let mut buf = vec![0; len as usize];
    unsafe {
        sys::read_register(ATOMIC_REGISTER_ID, buf.as_mut_ptr() as u64);
    }
    buf
}

fn input() -> Vec<u8> {
    read(sys::input)
}

#[unsafe(no_mangle)]
fn swap() {
    let input = input();
    let request: SwapRequest = borsh::from_slice(&input).expect("Invalid request");
    let amount = match request.amount {
        SwapRequestAmount::ExactIn(amount) => amount,
        SwapRequestAmount::ExactOut(amount) => amount,
    };
    assert_eq!(
        request.asset_in, request.asset_out,
        "Asset in and asset out must be the same, since this dex is a no-op"
    );
    let response = SwapResponse {
        amount_in: amount,
        amount_out: amount,
    };
    let response = borsh::to_vec(&response).expect("Failed to serialize response");
    return_value(&response);
}
