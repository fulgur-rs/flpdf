//! Explicit system-libjpeg compatibility boundary for qpdf-compatible DCT decoding.
//!
//! This crate is used only through flpdf's explicit `qpdf-libjpeg-compat`
//! feature. It links the system `libjpeg` selected by the build environment;
//! it never vendors or selects a runtime backend. The supported ABI is the
//! qpdf 11.9.0 decode subset with `BITS_IN_JSAMPLE == 8` and a
//! `JPEG_LIB_VERSION >= 62` (libjpeg 6b-compatible) header/API surface.
//!
//! The C boundary owns libjpeg's `setjmp` error manager, whole-buffer source
//! manager, and scanline callback ABI. The source manager reports
//! `invalid jpeg data reading from buffer` when the supplied buffer is
//! exhausted and never fabricates an EOI marker. The caller owns downstream
//! pipeline forwarding and `finish` lifecycle decisions.

#![deny(unsafe_code)]

use std::fmt;

/// Failure returned by the compatibility decoder.
#[derive(Debug)]
pub enum DecodeError<E> {
    /// libjpeg rejected the compressed input or could not complete decoding.
    Codec(String),
    /// The scanline callback returned its own downstream error.
    Callback(E),
    /// The scanline callback panicked; the panic was contained at the FFI boundary.
    CallbackPanicked,
    /// The C/Rust callback protocol reported an invalid or incomplete callback result.
    CallbackFailure(String),
}

impl<E: fmt::Display> fmt::Display for DecodeError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(message) | Self::CallbackFailure(message) => formatter.write_str(message),
            Self::Callback(error) => error.fmt(formatter),
            Self::CallbackPanicked => formatter.write_str("scanline callback panicked"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for DecodeError<E> {}

#[cfg(feature = "system-libjpeg")]
#[allow(unsafe_code)]
mod ffi;

/// Decode a complete JPEG buffer and invoke `callback` once for each output scanline.
///
/// The callback is invoked synchronously while `data` is borrowed. The wrapper owns
/// the unsafe libjpeg and C/Rust callback boundary; downstream lifecycle decisions
/// remain with the caller.
#[cfg(feature = "system-libjpeg")]
pub fn decode_scanlines<E>(
    data: &[u8],
    callback: &mut dyn FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), DecodeError<E>> {
    ffi::decode_scanlines(data, callback)
}
