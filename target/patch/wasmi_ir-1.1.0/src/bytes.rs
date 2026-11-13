//! Conversions of [`Op`] from and to the bytes that make it up in memory.
//!
//! # Note
//!
//! This allows to store translated Wasm functions and to execute them later directly
//! from where their bytes are stored without having to translate their Wasm again.
//!
//! Since [`Op`] is a `#[repr(u16)]` enum its layout is its `u16` discriminant followed by
//! the fields of its variant, laid out as a `#[repr(C)]` struct with the same fields would.
//! The fields of all [`Op`] variants are `#[repr(transparent)]` wrappers around integers or
//! `#[repr(C)]` compositions of them.

#[cfg(feature = "simd")]
use crate::core::simd::{ImmLaneIdx, ImmLaneIdx16, ImmLaneIdx2, ImmLaneIdx4, ImmLaneIdx8};
use crate::{
    core::TrapCode,
    index::{Data, Elem, Func, FuncType, Global, InternalFunc, Memory, Slot, Table},
    Address32,
    AnyConst16,
    AnyConst32,
    BlockFuel,
    BoundedSlotSpan,
    BranchOffset,
    BranchOffset16,
    Const16,
    Const32,
    Error,
    FixedSlotSpan,
    Offset16,
    Offset64Lo,
    Offset8,
    Op,
    ShiftAmount,
    Sign,
    SlotSpan,
};
use core::{
    mem::{align_of, offset_of, size_of},
    num::{NonZeroI32, NonZeroI64, NonZeroU32, NonZeroU64},
    slice,
};

/// Implemented by all types that are fields of [`Op`] variants.
trait OpField: Copy {
    /// Writes the bytes of `self` in memory to `bytes`.
    ///
    /// # Note
    ///
    /// The length of `bytes` is the size of `Self`.
    fn write_bytes(self, bytes: &mut [u8]);

    /// Returns `true` if `bytes` are a valid value of `Self` in memory.
    ///
    /// # Note
    ///
    /// - The length of `bytes` is the size of `Self`.
    /// - By default all bytes are valid since most fields wrap integers.
    #[inline]
    fn is_valid(_bytes: &[u8]) -> bool {
        true
    }
}

macro_rules! impl_op_field_for_int {
    ( $( $ty:ty ),* $(,)? ) => {
        $(
            impl OpField for $ty {
                #[inline]
                fn write_bytes(self, bytes: &mut [u8]) {
                    bytes.copy_from_slice(&self.to_ne_bytes());
                }
            }
        )*
    };
}
impl_op_field_for_int!(u8, i8, u16, i16, u32, i32);

macro_rules! impl_op_field_via {
    ( $( $ty:ty => $repr:ty ),* $(,)? ) => {
        $(
            impl OpField for $ty {
                #[inline]
                fn write_bytes(self, bytes: &mut [u8]) {
                    <$repr>::from(self).write_bytes(bytes);
                }
            }
        )*
    };
}
impl_op_field_via! {
    Slot => i16,
    Func => u32,
    FuncType => u32,
    InternalFunc => u32,
    Global => u32,
    Memory => u32,
    Table => u32,
    Data => u32,
    Elem => u32,
    AnyConst16 => u16,
    AnyConst32 => u32,
}

impl<const N: usize> OpField for [Slot; N] {
    #[inline]
    fn write_bytes(self, bytes: &mut [u8]) {
        for (slot, bytes) in self
            .into_iter()
            .zip(bytes.chunks_exact_mut(size_of::<Slot>()))
        {
            slot.write_bytes(bytes);
        }
    }
}

impl OpField for TrapCode {
    #[inline]
    fn write_bytes(self, bytes: &mut [u8]) {
        u8::from(self).write_bytes(bytes);
    }

    #[inline]
    fn is_valid(bytes: &[u8]) -> bool {
        matches!(bytes, [trap_code] if TrapCode::try_from(*trap_code).is_ok())
    }
}

impl<T> OpField for Sign<T> {
    #[inline]
    fn write_bytes(self, bytes: &mut [u8]) {
        u8::from(self.is_positive).write_bytes(bytes);
    }

    #[inline]
    fn is_valid(bytes: &[u8]) -> bool {
        // Note: `Sign` wraps a `bool`.
        matches!(bytes, [0 | 1])
    }
}

impl OpField for BlockFuel {
    #[inline]
    fn write_bytes(self, bytes: &mut [u8]) {
        // Note: `BlockFuel` wraps a `u32`.
        (self.to_u64() as u32).write_bytes(bytes);
    }
}

impl OpField for BranchOffset16 {
    #[inline]
    fn write_bytes(self, bytes: &mut [u8]) {
        self.to_i16().write_bytes(bytes);
    }
}

impl OpField for BranchOffset {
    #[inline]
    fn write_bytes(self, bytes: &mut [u8]) {
        self.to_i32().write_bytes(bytes);
    }
}

impl OpField for Offset64Lo {
    #[inline]
    fn write_bytes(self, bytes: &mut [u8]) {
        self.0.write_bytes(bytes);
    }
}

impl OpField for Offset8 {
    #[inline]
    fn write_bytes(self, bytes: &mut [u8]) {
        self.0.write_bytes(bytes);
    }
}

impl OpField for Offset16 {
    #[inline]
    fn write_bytes(self, bytes: &mut [u8]) {
        self.0.write_bytes(bytes);
    }
}

impl OpField for Address32 {
    #[inline]
    fn write_bytes(self, bytes: &mut [u8]) {
        self.0.write_bytes(bytes);
    }
}

macro_rules! impl_op_field_for_const16 {
    ( $( $ty:ty ),* $(,)? ) => {
        $(
            impl OpField for Const16<$ty> {
                #[inline]
                fn write_bytes(self, bytes: &mut [u8]) {
                    self.inner.write_bytes(bytes);
                }
            }
        )*
    };
}
impl_op_field_for_const16!(i32, u32, i64, u64);

macro_rules! impl_op_field_for_non_zero_const16 {
    ( $( $ty:ty ),* $(,)? ) => {
        $(
            impl OpField for Const16<$ty> {
                #[inline]
                fn write_bytes(self, bytes: &mut [u8]) {
                    self.inner.write_bytes(bytes);
                }

                #[inline]
                fn is_valid(bytes: &[u8]) -> bool {
                    // Note: converting to the non-zero integer relies on this.
                    bytes.iter().any(|byte| *byte != 0)
                }
            }
        )*
    };
}
impl_op_field_for_non_zero_const16!(NonZeroI32, NonZeroU32, NonZeroI64, NonZeroU64);

impl<T> OpField for Const32<T> {
    #[inline]
    fn write_bytes(self, bytes: &mut [u8]) {
        self.inner.write_bytes(bytes);
    }
}

macro_rules! impl_op_field_for_shift_amount {
    ( $( $ty:ty => $max:literal ),* $(,)? ) => {
        $(
            impl OpField for ShiftAmount<$ty> {
                #[inline]
                fn write_bytes(self, bytes: &mut [u8]) {
                    self.value.write_bytes(bytes);
                }

                #[inline]
                fn is_valid(bytes: &[u8]) -> bool {
                    // Note: shift amounts are within `1..BITS` as `IntoShiftAmount` creates them
                    //       where `BITS` is the bit width of the shifted value.
                    let Ok(bytes) = <[u8; 2]>::try_from(bytes) else {
                        return false;
                    };
                    (1..$max).contains(&u16::from_ne_bytes(bytes))
                }
            }
        )*
    };
}
impl_op_field_for_shift_amount! {
    i32 => 32,
    i64 => 64,
    u32 => 64,
}

impl OpField for SlotSpan {
    #[inline]
    fn write_bytes(self, bytes: &mut [u8]) {
        self.head().write_bytes(bytes);
    }
}

impl<const N: u16> OpField for FixedSlotSpan<N> {
    #[inline]
    fn write_bytes(self, bytes: &mut [u8]) {
        self.span().write_bytes(bytes);
    }

    #[inline]
    fn is_valid(bytes: &[u8]) -> bool {
        let Ok(bytes) = <[u8; 2]>::try_from(bytes) else {
            return false;
        };
        let head = Slot::from(i16::from_ne_bytes(bytes));
        Self::new(SlotSpan::new(head)).is_ok()
    }
}

impl OpField for BoundedSlotSpan {
    #[inline]
    fn write_bytes(self, bytes: &mut [u8]) {
        // Note: `BoundedSlotSpan` is `#[repr(C)]`.
        let (span, len) = bytes.split_at_mut(size_of::<SlotSpan>());
        self.span().write_bytes(span);
        self.len().write_bytes(len);
    }
}

#[cfg(feature = "simd")]
impl<const N: u8> OpField for ImmLaneIdx<N> {
    #[inline]
    fn write_bytes(self, bytes: &mut [u8]) {
        u8::from(self).write_bytes(bytes);
    }

    #[inline]
    fn is_valid(bytes: &[u8]) -> bool {
        matches!(bytes, [lane] if Self::try_from(*lane).is_ok())
    }
}

macro_rules! define_op_bytes {
    (
        $(
            $( #[doc = $doc:literal] )*
            #[snake_name($snake_name:ident)]
            $name:ident
            $(
                {
                    $(
                        @ $result_name:ident: $result_ty:ty,
                    )?
                    $(
                        $( #[$field_docs:meta] )*
                        $field_name:ident: $field_ty:ty
                    ),*
                    $(,)?
                }
            )?
        ),* $(,)?
    ) => {
        /// The discriminants of all [`Op`] variants.
        #[derive(Copy, Clone)]
        #[repr(u16)]
        enum OpCode {
            $( $name ),*
        }

        impl OpCode {
            /// All [`OpCode`]s ordered by their discriminants.
            const ALL: &'static [Self] = &[ $( Self::$name ),* ];
        }

        /// The memory layouts of all [`Op`] variants.
        mod op_layout {
            $(
                #[allow(dead_code)]
                pub mod $snake_name {
                    use super::super::*;

                    #[repr(C)]
                    pub struct Layout {
                        pub op_code: u16,
                        $(
                            $( pub $result_name: $result_ty, )?
                            $( pub $field_name: $field_ty, )*
                        )?
                    }

                    const _: () = assert!(size_of::<Layout>() <= size_of::<Op>());
                }
            )*
        }

        impl Op {
            /// Returns the bytes of `self` in memory.
            ///
            /// # Note
            ///
            /// Padding bytes are zeroed.
            pub fn to_bytes(&self) -> [u8; size_of::<Op>()] {
                let mut bytes = [0x00_u8; size_of::<Op>()];
                match *self {
                    $(
                        Self::$name $( { $( $result_name, )? $( $field_name ),* } )? => {
                            (OpCode::$name as u16).write_bytes(&mut bytes[..size_of::<u16>()]);
                            $(
                                $(
                                    let offset = offset_of!(op_layout::$snake_name::Layout, $result_name);
                                    $result_name.write_bytes(&mut bytes[offset..][..size_of::<$result_ty>()]);
                                )?
                                $(
                                    let offset = offset_of!(op_layout::$snake_name::Layout, $field_name);
                                    $field_name.write_bytes(&mut bytes[offset..][..size_of::<$field_ty>()]);
                                )*
                            )?
                        }
                    )*
                }
                bytes
            }

            /// Returns `true` if `bytes` are a valid [`Op`] in memory.
            fn is_valid_bytes(bytes: &[u8; size_of::<Op>()]) -> bool {
                let [op_code_lo, op_code_hi, ..] = *bytes;
                let op_code = usize::from(u16::from_ne_bytes([op_code_lo, op_code_hi]));
                let Some(op_code) = OpCode::ALL.get(op_code) else {
                    return false;
                };
                match op_code {
                    $(
                        OpCode::$name => {
                            true
                            $(
                                $(
                                    && {
                                        let offset = offset_of!(op_layout::$snake_name::Layout, $result_name);
                                        <$result_ty>::is_valid(&bytes[offset..][..size_of::<$result_ty>()])
                                    }
                                )?
                                $(
                                    && {
                                        let offset = offset_of!(op_layout::$snake_name::Layout, $field_name);
                                        <$field_ty>::is_valid(&bytes[offset..][..size_of::<$field_ty>()])
                                    }
                                )*
                            )?
                        }
                    )*
                }
            }

            /// Returns `bytes` as the sequence of [`Op`] that they are in memory.
            ///
            /// # Note
            ///
            /// The returned [`Op`]s are not copied but refer to `bytes` directly.
            ///
            /// # Errors
            ///
            /// - If `bytes` are not aligned to [`Op`].
            /// - If the length of `bytes` is not a multiple of the size of [`Op`].
            /// - If any [`Op`] in `bytes` is invalid.
            pub fn slice_from_bytes(bytes: &[u8]) -> Result<&[Self], Error> {
                let ptr = bytes.as_ptr();
                if ptr.align_offset(align_of::<Self>()) != 0 || bytes.len() % size_of::<Self>() != 0 {
                    return Err(Error::InvalidOpBytes);
                }
                for op in bytes.chunks_exact(size_of::<Self>()) {
                    let Ok(op) = <&[u8; size_of::<Op>()]>::try_from(op) else {
                        return Err(Error::InvalidOpBytes);
                    };
                    if !Self::is_valid_bytes(op) {
                        return Err(Error::InvalidOpBytes);
                    }
                }
                // SAFETY: `bytes` are aligned to `Op` and consist of `len` valid `Op`s:
                //
                // - Each starts with the discriminant of one of its variants.
                // - The fields of that variant are where `op_layout` has them,
                //   which is how `#[repr(u16)]` lays out enums with fields.
                // - The bytes of each field are a valid value of its type.
                // - Padding bytes are allowed to be any bytes.
                let len = bytes.len() / size_of::<Self>();
                Ok(unsafe { slice::from_raw_parts(ptr.cast::<Self>(), len) })
            }
        }
    };
}
crate::for_each_op::for_each_op!(define_op_bytes);
