//! qpdf correspondence: `QUtil::get_max_memory_usage`.
//! (`libqpdf/QUtil.cc:1941-2002`).
//!
//! qpdf uses glibc's `malloc_info` XML on Linux and returns zero when that
//! development-only allocator report is unavailable. Keep the same narrow
//! contract here; this value is intended for performance diagnostics, not for
//! accounting or security decisions.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[allow(unsafe_code)]
mod gnu_linux {
    use std::ptr;

    unsafe extern "C" {
        fn malloc_info(options: libc::c_int, stream: *mut libc::FILE) -> libc::c_int;
        fn open_memstream(
            buffer: *mut *mut libc::c_char,
            size: *mut libc::size_t,
        ) -> *mut libc::FILE;
    }

    pub(super) fn max_memory_usage() -> usize {
        let mut buffer = ptr::null_mut();
        let mut size = 0;
        let stream = {
            // SAFETY: `open_memstream` receives pointers to writable local
            // storage and returns an owned C stream or null.
            unsafe { open_memstream(&mut buffer, &mut size) }
        };
        // cov:ignore-start: glibc reports allocation failure only when the process cannot allocate the diagnostic memstream itself
        if stream.is_null() {
            return 0;
        }
        // cov:ignore-end

        let report_status = {
            // SAFETY: `stream` is the live stream returned by
            // `open_memstream`, and option zero is qpdf's call.
            unsafe { malloc_info(0, stream) }
        };
        let close_status = {
            // SAFETY: closing the stream flushes the memstream and finalizes
            // the buffer/size pair documented by `open_memstream`.
            unsafe { libc::fclose(stream) }
        };
        // cov:ignore-start: malloc_info/fclose failure is controlled by glibc and cannot be injected without replacing the process allocator
        if report_status != 0 || close_status != 0 || buffer.is_null() {
            if !buffer.is_null() {
                // SAFETY: a non-null buffer is the allocation transferred by
                // `open_memstream`; no Rust owner exists for it.
                unsafe { libc::free(buffer.cast()) };
            }
            return 0;
        }
        // cov:ignore-end

        let bytes = {
            // SAFETY: `fclose` completed successfully, `buffer` is non-null,
            // and `size` is the byte length supplied by the C memstream.
            unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), size) }
        };
        let result = parse_malloc_info(bytes);
        // SAFETY: the buffer came from libc's memstream allocator and has not
        // been freed since the slice above was consumed.
        unsafe { libc::free(buffer.cast()) };
        result
    }

    fn parse_malloc_info(xml: &[u8]) -> usize {
        let mut in_heap = 0_i32;
        let mut result = 0_usize;
        let mut remainder = xml;
        while let Some(start) = remainder.iter().position(|&byte| byte == b'<') {
            remainder = &remainder[start + 1..];
            let Some(end) = remainder.iter().position(|&byte| byte == b'>') else {
                return 0;
            };
            let tag = &remainder[..end];
            remainder = &remainder[end + 1..];

            let closing = tag.first() == Some(&b'/');
            let name = tag
                .get(usize::from(closing))
                .into_iter()
                .chain(tag.get(usize::from(closing) + 1..).unwrap_or_default())
                .copied()
                .take_while(|byte| byte.is_ascii_alphabetic())
                .collect::<Vec<_>>();
            if closing && name == b"heap" {
                in_heap -= 1;
                continue;
            }
            if !closing && name == b"heap" {
                in_heap += 1;
                continue;
            }
            if in_heap != 0 {
                continue;
            }

            let is_total = name == b"total";
            let is_system_max = name == b"system" && attr_value(tag, b"type") == Some(b"max");
            if !(is_total || is_system_max) {
                continue;
            }
            let Some(size) = attr_value(tag, b"size").and_then(parse_size) else {
                return 0;
            };
            let Some(next) = result.checked_add(size) else {
                return 0;
            };
            result = next;
        }
        result
    }

    fn attr_value<'a>(tag: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
        let mut remainder = tag;
        while let Some(position) = remainder
            .iter()
            .position(|&byte| byte == b' ' || byte == b'\t')
        {
            remainder = &remainder[position + 1..];
            if !remainder.starts_with(name) || remainder.get(name.len()) != Some(&b'=') {
                continue;
            }
            let quoted = remainder.get(name.len() + 1..)?.strip_prefix(b"\"")?;
            let end = quoted.iter().position(|&byte| byte == b'\"')?;
            return Some(&quoted[..end]);
        }
        None
    }

    fn parse_size(value: &[u8]) -> Option<usize> {
        if value.is_empty() {
            return None;
        }
        let mut result = 0_usize;
        for &byte in value {
            if !byte.is_ascii_digit() {
                return None;
            }
            result = result
                .checked_mul(10)?
                .checked_add(usize::from(byte - b'0'))?;
        }
        Some(result)
    }

    #[cfg(test)]
    mod tests {
        use super::{max_memory_usage, parse_malloc_info};

        #[test]
        fn parse_malloc_info_sums_only_top_level_total_and_max_system() {
            let xml = br#"<malloc><heap nr="0"><total size="999"/></heap><total size="123"/><system type="current" size="11"/><system type="max" size="456"/></malloc>"#;
            assert_eq!(parse_malloc_info(xml), 579);
        }

        #[test]
        fn parse_malloc_info_returns_zero_for_malformed_or_unusable_sizes() {
            assert_eq!(parse_malloc_info(b"<malloc"), 0);
            assert_eq!(parse_malloc_info(b"<malloc><total/></malloc>"), 0);
            assert_eq!(parse_malloc_info(b"<malloc><total size=\"\"/></malloc>"), 0);
            assert_eq!(
                parse_malloc_info(b"<malloc><total size=\"x\"/></malloc>"),
                0
            );
            assert_eq!(
                parse_malloc_info(
                    b"<malloc><total size=\"18446744073709551615\"/><total size=\"1\"/></malloc>"
                ),
                0
            );
            assert_eq!(
                parse_malloc_info(b"<malloc><system size=\"1\"/></malloc>"),
                0
            );
        }

        #[test]
        fn max_memory_usage_is_available_on_gnu_linux() {
            assert!(max_memory_usage() > 0);
        }
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(crate) fn max_memory_usage() -> usize {
    gnu_linux::max_memory_usage()
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub(crate) const fn max_memory_usage() -> usize {
    0
}
