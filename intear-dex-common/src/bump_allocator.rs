use core::alloc::{GlobalAlloc, Layout};
use core::arch::wasm32;
use core::ptr::null_mut;

const PAGE_SIZE: usize = 0x10000;
/// Like the default allocator: the engine deserializes translations of dex code in place from
/// byte vectors, which have to be aligned to wasmi's instructions
const MIN_ALIGN: usize = 8;

/// Hands out memory past the heap base and never frees it, since every receipt and every dex
/// call runs in an instance of its own: it runs faster and is smaller than an allocator that
/// frees. New memory of wasm is zeroed, and none is handed out twice.
pub struct BumpAllocator;

static mut HEAP_END: usize = 0;

unsafe extern "C" {
    static __heap_base: u8;
}

unsafe impl GlobalAlloc for BumpAllocator {
    // Inlined into every allocation, it makes the swap code of amp 15 KB bigger
    #[inline(never)]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let heap_end = match unsafe { HEAP_END } {
            0 => core::ptr::addr_of!(__heap_base) as usize,
            heap_end => heap_end,
        };
        let Some(start) = heap_end.checked_next_multiple_of(layout.align().max(MIN_ALIGN)) else {
            return null_mut();
        };
        let Some(end) = start.checked_add(layout.size()) else {
            return null_mut();
        };
        let memory_end = wasm32::memory_size(0).saturating_mul(PAGE_SIZE);
        if end > memory_end
            && wasm32::memory_grow(0, end.abs_diff(memory_end).div_ceil(PAGE_SIZE)) == usize::MAX
        {
            return null_mut();
        }
        unsafe { HEAP_END = end };
        start as *mut u8
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        unsafe { self.alloc(layout) }
    }
}
