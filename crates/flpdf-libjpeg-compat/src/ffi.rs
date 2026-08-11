use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uchar, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::slice;

use super::DecodeError;

const SUCCESS: c_int = 0;
const CODEC_ERROR: c_int = 1;
const CALLBACK_ERROR: c_int = 2;
const CALLBACK_FAILURE: c_int = 1;

extern "C" {
    fn flpdf_jpeg_decode_scanlines(
        data: *const c_uchar,
        data_len: usize,
        callback: unsafe extern "C" fn(*mut c_void, *const c_uchar, usize) -> c_int,
        user: *mut c_void,
        error_message: *mut c_char,
        error_message_len: usize,
    ) -> c_int;
}

enum CallbackFailure<E> {
    Error(E),
    Panicked,
    Message(String),
}

struct CallbackState<'a, E> {
    callback: &'a mut dyn FnMut(&[u8]) -> Result<(), E>,
    failure: Option<CallbackFailure<E>>,
}

unsafe extern "C" fn jpeg_scanline_callback<E>(
    user: *mut c_void,
    row: *const c_uchar,
    row_len: usize,
) -> c_int {
    if user.is_null() {
        return CALLBACK_FAILURE;
    }

    let state = unsafe { &mut *(user.cast::<CallbackState<'_, E>>()) };
    let result = catch_unwind(AssertUnwindSafe(|| {
        if state.failure.is_some() {
            return CALLBACK_FAILURE;
        }
        if row.is_null() && row_len != 0 {
            state.failure = Some(CallbackFailure::Message(
                "compatibility backend returned a null scanline".to_owned(),
            ));
            return CALLBACK_FAILURE;
        }

        let row = if row_len == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(row, row_len) }
        };
        match (state.callback)(row) {
            Ok(()) => SUCCESS,
            Err(error) => {
                state.failure = Some(CallbackFailure::Error(error));
                CALLBACK_FAILURE
            }
        }
    }));

    match result {
        Ok(result) => result,
        Err(_) => {
            if state.failure.is_none() {
                state.failure = Some(CallbackFailure::Panicked);
            }
            CALLBACK_FAILURE
        }
    }
}

pub(super) fn decode_scanlines<E>(
    data: &[u8],
    callback: &mut dyn FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), DecodeError<E>> {
    let mut state = CallbackState {
        callback,
        failure: None,
    };
    let mut error_message = [0 as c_char; 256];
    let result = unsafe {
        flpdf_jpeg_decode_scanlines(
            data.as_ptr(),
            data.len(),
            jpeg_scanline_callback::<E>,
            (&mut state as *mut CallbackState<'_, E>).cast::<c_void>(),
            error_message.as_mut_ptr(),
            error_message.len(),
        )
    };
    let callback_failure = state.failure.take();
    drop(state);

    if result == CALLBACK_ERROR {
        return match callback_failure {
            Some(CallbackFailure::Error(error)) => Err(DecodeError::Callback(error)),
            Some(CallbackFailure::Panicked) => Err(DecodeError::CallbackPanicked),
            Some(CallbackFailure::Message(message)) => Err(DecodeError::CallbackFailure(message)),
            None => Err(DecodeError::CallbackFailure(
                "compatibility backend callback failed without a downstream error".to_owned(),
            )),
        };
    }

    if result == SUCCESS {
        return Ok(());
    }

    let diagnostic = unsafe { CStr::from_ptr(error_message.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    if result == CODEC_ERROR {
        return Err(DecodeError::Codec(if diagnostic.is_empty() {
            "libjpeg decode failed".to_owned()
        } else {
            diagnostic
        }));
    }

    Err(DecodeError::CallbackFailure(format!(
        "compatibility backend returned unknown status {result}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    #[test]
    fn callback_rejects_invalid_abi_inputs() {
        let mut callback = |_row: &[u8]| Ok::<(), &'static str>(());
        let null_user =
            unsafe { jpeg_scanline_callback::<&'static str>(ptr::null_mut(), ptr::null(), 0) };
        assert_eq!(null_user, CALLBACK_FAILURE);

        let mut state = CallbackState {
            callback: &mut callback,
            failure: Some(CallbackFailure::Error("existing downstream error")),
        };
        let existing_error_result = unsafe {
            jpeg_scanline_callback::<&'static str>(
                (&mut state as *mut CallbackState<'_, &'static str>).cast::<c_void>(),
                ptr::null(),
                0,
            )
        };
        assert_eq!(existing_error_result, CALLBACK_FAILURE);
        assert!(matches!(
            state.failure,
            Some(CallbackFailure::Error("existing downstream error"))
        ));

        let mut null_row_state = CallbackState {
            callback: &mut callback,
            failure: None,
        };
        let null_row_result = unsafe {
            jpeg_scanline_callback::<&'static str>(
                (&mut null_row_state as *mut CallbackState<'_, &'static str>).cast::<c_void>(),
                ptr::null(),
                1,
            )
        };
        assert_eq!(null_row_result, CALLBACK_FAILURE);
        assert!(matches!(
            null_row_state.failure,
            Some(CallbackFailure::Message(message))
                if message == "compatibility backend returned a null scanline"
        ));
    }

    #[test]
    fn callback_forwards_empty_scanline() {
        let mut rows = Vec::new();
        let mut callback = |row: &[u8]| {
            rows.push(row.to_vec());
            Ok::<(), ()>(())
        };
        let mut state = CallbackState {
            callback: &mut callback,
            failure: None,
        };

        let result = unsafe {
            jpeg_scanline_callback::<()>(
                (&mut state as *mut CallbackState<'_, ()>).cast::<c_void>(),
                ptr::null(),
                0,
            )
        };

        assert_eq!(result, SUCCESS);
        assert!(state.failure.is_none());
        drop(state);
        assert_eq!(rows, vec![Vec::<u8>::new()]);
    }

    #[test]
    fn callback_converts_downstream_panic_to_contained_failure() {
        let mut callback = |_row: &[u8]| -> Result<(), ()> {
            panic!("downstream test panic");
        };
        let mut state = CallbackState {
            callback: &mut callback,
            failure: None,
        };

        let result = unsafe {
            jpeg_scanline_callback::<()>(
                (&mut state as *mut CallbackState<'_, ()>).cast::<c_void>(),
                ptr::null(),
                0,
            )
        };

        assert_eq!(result, CALLBACK_FAILURE);
        assert!(matches!(state.failure, Some(CallbackFailure::Panicked)));
    }
}
