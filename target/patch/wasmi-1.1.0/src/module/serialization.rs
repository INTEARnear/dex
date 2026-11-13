use super::{parser::ModuleParser, Module};
use crate::{engine::read_u32, ir, ir::Op, Config, Engine, Error};
use alloc::{format, vec::Vec};
use core::{
    error::Error as CoreError,
    fmt::{self, Display},
    iter,
    mem::{align_of, size_of},
};
use wasmparser::BinaryReader;

/// Errors that may occur upon serializing or deserializing a [`Module`].
#[derive(Debug, PartialEq, Eq)]
pub enum SerializationError {
    /// The translated Wasm module is too large to be serialized.
    ModuleTooLarge,
    /// The bytes are not a serialized [`Module`].
    MalformedBytes,
    /// The bytes are not aligned to the instructions of the serialized [`Module`].
    UnalignedBytes,
    /// The [`Module`] was serialized by another Wasmi version or for an [`Engine`] that
    /// translates Wasm differently.
    IncompatibleEngine,
    /// The [`Engine`] already has the functions of other modules.
    EngineNotEmpty,
}

impl CoreError for SerializationError {}

impl Display for SerializationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ModuleTooLarge => write!(f, "module is too large to be serialized"),
            Self::MalformedBytes => write!(f, "encountered malformed serialized module"),
            Self::UnalignedBytes => write!(f, "serialized module is not aligned"),
            Self::IncompatibleEngine => {
                write!(
                    f,
                    "serialized module is incompatible with the Wasmi version or engine config"
                )
            }
            Self::EngineNotEmpty => {
                write!(
                    f,
                    "serialized modules can only be deserialized in an empty engine"
                )
            }
        }
    }
}

/// The version of the format of serialized [`Module`]s.
///
/// # Note
///
/// Must be incremented whenever the format changes without the Wasmi version changing.
const FORMAT_VERSION: u32 = 1;
/// The length of the magic number and version that every Wasm module starts with.
const WASM_PREAMBLE_LEN: usize = 8;
/// The identifier of a Wasm custom section.
const CUSTOM_SECTION_ID: u8 = 0;
/// The identifier of the Wasm code section.
const CODE_SECTION_ID: u8 = 10;

// Note: the `u32` fields of serialized modules keep the [`Op`]s that follow them aligned.
const _: () = assert!(size_of::<u32>() % align_of::<Op>() == 0);

impl Module {
    /// Returns the serialized `self` that was created from `wasm`.
    ///
    /// # Note
    ///
    /// A serialized [`Module`] consists of:
    ///
    /// - The length of the [`header`] of the [`Engine`] of `self` as `u32`.
    /// - The [`header`], padded to the alignment of [`Op`].
    /// - The length of `wasm` without its code and custom sections as `u32`.
    /// - The `wasm` without its code and custom sections, padded to the alignment of [`Op`].
    /// - The number of functions of `self` as `u32`.
    /// - The end offset of each serialized function as `u32`.
    /// - The serialized functions.
    ///
    /// # Errors
    ///
    /// - If translation of a function of `self` failed.
    /// - If `self` is too large to be serialized.
    ///
    /// # Panics
    ///
    /// If `self` is not the first [`Module`] of its [`Engine`].
    pub(crate) fn serialize(&self, wasm: &[u8]) -> Result<Vec<u8>, Error> {
        let engine_funcs = self.module_header().engine_funcs;
        assert!(
            engine_funcs.is_first(),
            "serialized modules must be the first module of their engine"
        );
        let wasm = without_code_and_custom_sections(wasm)?;
        let mut bytes = Vec::new();
        push_padded(&mut bytes, &header(self.engine().config()))?;
        push_padded(&mut bytes, &wasm)?;
        push_u32(&mut bytes, engine_funcs.len() as usize)?;
        let ends = bytes.len();
        bytes.resize(ends + engine_funcs.len() as usize * size_of::<u32>(), 0x00);
        let funcs = bytes.len();
        for (n, func) in engine_funcs.iter().enumerate() {
            self.engine().serialize_func(func, &mut bytes)?;
            let end = u32::try_from(bytes.len() - funcs)
                .map_err(|_| SerializationError::ModuleTooLarge)?;
            let end_offset = ends + n * size_of::<u32>();
            bytes[end_offset..][..size_of::<u32>()].copy_from_slice(&end.to_le_bytes());
        }
        Ok(bytes)
    }

    /// Creates a new Wasm [`Module`] from its serialized `bytes`.
    ///
    /// # Note
    ///
    /// - Serialized [`Module`]s are created via [`Engine::precompile_module`].
    /// - The functions of the [`Module`] are deserialized when they are called the first time.
    /// - The deserialized functions refer to `bytes` instead of copying them. Thus `bytes` must be
    ///   aligned to the instructions of the functions.
    /// - Serialized [`Module`]s of other Wasmi versions or of an [`Engine`] that translates Wasm
    ///   differently than `engine` are rejected.
    ///
    /// # Safety
    ///
    /// - `bytes` must have been returned by [`Engine::precompile_module`] unchanged.
    /// - Violating the above rule is undefined behavior.
    ///
    /// # Errors
    ///
    /// - If `bytes` are not a serialized [`Module`] or are not aligned.
    /// - If `bytes` were serialized by another Wasmi version or for an incompatible [`Engine`].
    /// - If `engine` already has the functions of other modules.
    pub unsafe fn deserialize(engine: &Engine, bytes: &'static [u8]) -> Result<Self, Error> {
        if bytes.as_ptr().align_offset(align_of::<Op>()) != 0 {
            return Err(Error::from(SerializationError::UnalignedBytes));
        }
        let (serialized_header, rest) = read_padded(bytes)?;
        if serialized_header != header(engine.config()) {
            return Err(Error::from(SerializationError::IncompatibleEngine));
        }
        let (wasm, rest) = read_padded(rest)?;
        let (len_funcs, rest) = read_u32(rest)?;
        let Some((mut ends, funcs)) = rest.split_at_checked(len_funcs as usize * size_of::<u32>())
        else {
            return Err(Error::from(SerializationError::MalformedBytes));
        };
        // Safety: the caller guarantees that `bytes` have been created from a valid Wasm module.
        let module =
            unsafe { ModuleParser::new_without_code(engine).parse_buffered_unchecked(wasm)? };
        let engine_funcs = module.module_header().engine_funcs;
        if !engine_funcs.is_first() {
            return Err(Error::from(SerializationError::EngineNotEmpty));
        }
        if engine_funcs.len() != len_funcs {
            return Err(Error::from(SerializationError::MalformedBytes));
        }
        let mut start = 0;
        for func in engine_funcs.iter() {
            let (end, rest) = read_u32(ends)?;
            ends = rest;
            let Some(func_bytes) = funcs.get(start..end as usize) else {
                return Err(Error::from(SerializationError::MalformedBytes));
            };
            engine.init_serialized_func(func, func_bytes);
            start = end as usize;
        }
        Ok(module)
    }
}

/// Appends `value` as `u32` to `bytes`.
///
/// # Errors
///
/// If `value` does not fit into a `u32`.
fn push_u32(bytes: &mut Vec<u8>, value: usize) -> Result<(), SerializationError> {
    let value = u32::try_from(value).map_err(|_| SerializationError::ModuleTooLarge)?;
    bytes.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

/// Appends the length of `data` as `u32` and `data` padded to the alignment of [`Op`] to `bytes`.
///
/// # Errors
///
/// If the length of `data` does not fit into a `u32`.
fn push_padded(bytes: &mut Vec<u8>, data: &[u8]) -> Result<(), SerializationError> {
    push_u32(bytes, data.len())?;
    bytes.extend_from_slice(data);
    let padding = data.len().next_multiple_of(align_of::<Op>()) - data.len();
    bytes.extend(iter::repeat_n(0x00, padding));
    Ok(())
}

/// Reads the data appended by [`push_padded`] from `bytes`.
///
/// Returns the data and the bytes that follow it.
///
/// # Errors
///
/// If `bytes` end before the data.
fn read_padded(bytes: &[u8]) -> Result<(&[u8], &[u8]), SerializationError> {
    let (len, rest) = read_u32(bytes)?;
    let padded_len = (len as usize).next_multiple_of(align_of::<Op>());
    match (rest.get(..len as usize), rest.get(padded_len..)) {
        (Some(data), Some(rest)) => Ok((data, rest)),
        _ => Err(SerializationError::MalformedBytes),
    }
}

/// Returns the header of the [`Module`]s serialized for an [`Engine`] with `config`.
///
/// # Note
///
/// The [`Op`]s of serialized [`Module`]s are only meaningful to the same Wasmi version on the
/// same target and to [`Engine`]s with a [`Config`] that translates Wasm the same way.
/// The header identifies all of these so that incompatible [`Module`]s are rejected.
fn header(config: &Config) -> Vec<u8> {
    format!(
        "wasmi {} serialized module, format: {FORMAT_VERSION}, wasmi_ir: {}, simd: {}, \
        little endian: {}, wasm features: {:#x}, fuel costs: {:?}",
        env!("CARGO_PKG_VERSION"),
        ir::VERSION,
        cfg!(feature = "simd"),
        cfg!(target_endian = "little"),
        config.wasm_features().bits(),
        config.get_consume_fuel().then(|| config.fuel_costs()),
    )
    .into_bytes()
}

/// Returns the `wasm` module without its code and custom sections.
///
/// # Note
///
/// Serialized modules replace the code section with the serialized function translations
/// and do not need custom sections since they do not affect execution.
///
/// # Errors
///
/// If `wasm` is malformed.
fn without_code_and_custom_sections(wasm: &[u8]) -> Result<Vec<u8>, Error> {
    let mut reader = BinaryReader::new(wasm, 0);
    reader.read_bytes(WASM_PREAMBLE_LEN)?;
    let mut bytes = wasm[..WASM_PREAMBLE_LEN].to_vec();
    while !reader.eof() {
        let start = reader.original_position();
        let id = reader.read_u8()?;
        let len = reader.read_var_u32()?;
        reader.read_bytes(len as usize)?;
        if id != CODE_SECTION_ID && id != CUSTOM_SECTION_ID {
            bytes.extend_from_slice(&wasm[start..reader.original_position()]);
        }
    }
    Ok(bytes)
}
