use std::{
    borrow::Cow,
    ffi::{CStr, OsStr, OsString},
    io::{self, Write},
};

#[cfg(unix)]
use std::ffi::CString;

use flpdf::{Diagnostic, Error, Pdf, PdfOpenOptions};

use crate::common::test_driver_program_name_bytes;

pub(crate) mod handle;
pub(crate) mod test_02_09;
pub(crate) mod test_0_1;
pub(crate) mod test_10_17;
pub(crate) mod test_18_25;
pub(crate) mod test_26_33;
pub(crate) mod test_34_41;
pub(crate) mod test_42_49;
pub(crate) mod test_50_55;
pub(crate) mod test_56_63;
pub(crate) mod test_64_71;
pub(crate) mod test_72_79;
pub(crate) mod test_80_87;
pub(crate) mod test_88_98;

#[cfg(test)]
pub(crate) static CURRENT_DIR_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> =
    std::sync::OnceLock::new();

pub fn run(args: &[OsString], stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8 {
    let whoami = args
        .first()
        .map(OsString::as_os_str)
        .map(os_str_diagnostic_bytes)
        .unwrap_or_else(|| Cow::Borrowed(b"flpdf-test-driver"));
    let whoami = test_driver_program_name_bytes(&whoami);
    if args.len() < 3 || args.len() > 4 {
        let mut usage = b"Usage: ".to_vec();
        usage.extend_from_slice(whoami);
        usage.extend_from_slice(b" n filename1 [arg2]");
        return write_error_bytes(stdout, stderr, &usage);
    }

    let test_number = os_str_diagnostic_bytes(args[1].as_os_str());
    let n = match parse_test_number(&test_number) {
        Ok(n) => n,
        Err(error) => return write_error_bytes(stdout, stderr, &error),
    };
    // qpdf's test 89 builds `pdf` from a JSON export via
    // QPDF::createFromJSON(filename1) (test_driver.cc:3522-3523) instead of
    // parsing a PDF at all -- filename1 is a .json file for this test
    // number, so the ordinary open path below cannot be used.
    // GAP(QPDF::createFromJSON): flpdf has no constructor for building a Pdf
    // from a QPDF JSON export (`document_json.rs`'s own module doc: the
    // input side "has no counterpart here"). `test_88_98::run_test_89`
    // exists, assuming an already-open `pdf`, for when this primitive
    // lands.
    if n == 89 {
        return write_error(
            stdout,
            stderr,
            "test 89 requires QPDF::createFromJSON, which is not implemented in flpdf",
        );
    }

    let filename = args[2].as_os_str();
    let arg2 = args.get(3).map(OsString::as_os_str);

    // qpdf's runtest() (test_driver.cc:3463-3538) picks how to load
    // filename1 based on n:
    //  - n==0: setAttemptRecovery(false) -- already handled below via
    //    `repair: n != 0`.
    //  - (n==35 || n==36) && arg2 present: arg2 is a password, not an
    //    unused second argument -- handled below via `options.password`.
    //  - n==45: handled in the `open_bytes`/`filename_diagnostic` match
    //    below (obfuscated-file XOR decode).
    //  - n in {61,81,83,84,85,86,87,92,95,96}: qpdf never opens filename1 at
    //    all there -- each such test body ignores its `pdf` argument (and
    //    opens its own file(s) via arg2 where relevant) instead. The empty
    //    Pdf used below is the Rust equivalent of qpdf's default-constructed
    //    QPDF for these tests; it must be created without touching filename1.
    //  - n==89: QPDF::createFromJSON -- handled above, before this point.
    //  - everything else: read filename1 into memory. qpdf's n%2/n%4
    //    branching there (processFile(name) vs processFile(FILE*) vs
    //    processMemoryFile) only selects which overload to exercise for its
    //    own internal QTC::TC coverage tracing; all three parse the same
    //    bytes identically, so it has no observable effect and is
    //    intentionally not reproduced here.
    let filename_diagnostic = os_str_diagnostic_bytes(filename).into_owned();
    let (open_bytes, filename_diagnostic): (Option<Vec<u8>>, Vec<u8>) = if n == 45 {
        // qpdf's test 45 (test_driver.cc:3497-3519) reads
        // "<filename1>.obfuscated" through `QUtil::read_file_into_memory`,
        // which opens it via `QUtil::safe_fopen` (`libqpdf/QUtil.cc:1139`,
        // `:490-518`) -- the real ".obfuscated" path is what an open/read
        // failure there reports (`"open " + filename + ": " + strerror(...)`,
        // `QPDFSystemError::createWhat`, `libqpdf/QPDFSystemError.cc:12-28`).
        // Only *after* that read succeeds does qpdf XOR-decode the bytes and
        // process the result AS IF it were "<filename1>.pdf"
        // (`pdf.processMemoryFile((filename1 + ".pdf").c_str(), ...)`,
        // test_driver.cc:3519) -- that fabricated name is what appears in
        // this test's *later* parser diagnostics, never in the open/read
        // failure itself.
        let mut obfuscated_path = filename.to_os_string();
        obfuscated_path.push(".obfuscated");
        let mut pdf_name = filename.to_os_string();
        pdf_name.push(".pdf");
        let pdf_name_diagnostic = os_str_diagnostic_bytes(&pdf_name).into_owned();
        let raw = match std::fs::read(&obfuscated_path) {
            Ok(raw) => raw,
            Err(error) => {
                let crt_message = crt_open_error_message(&obfuscated_path);
                let obfuscated_diagnostic = os_str_diagnostic_bytes(&obfuscated_path);
                return write_error_bytes(
                    stdout,
                    stderr,
                    &open_error_bytes(&obfuscated_diagnostic, crt_message.as_deref(), &error),
                );
            }
        };
        let decoded = raw.into_iter().map(|byte| byte ^ 0xcc).collect();
        (Some(decoded), pdf_name_diagnostic)
    } else if qpdf_ignores_filename(n) {
        (None, filename_diagnostic)
    } else {
        let bytes = match std::fs::read(filename) {
            Ok(bytes) => bytes,
            Err(error) => {
                let crt_message = crt_open_error_message(filename);
                return write_error_bytes(
                    stdout,
                    stderr,
                    &open_error_bytes(&filename_diagnostic, crt_message.as_deref(), &error),
                );
            }
        };
        (Some(bytes), filename_diagnostic)
    };

    let mut options = PdfOpenOptions {
        repair: n != 0,
        // The compatibility driver owns byte-exact warning formatting and
        // routes it through the caller-supplied stdout/stderr writers below.
        suppress_warnings: true,
        // qpdf's processFile/processMemoryFile stores the input name on the
        // document before any lazy warning can reach QPDF::warn. Keep the
        // same description on the canonical resolver so a later live logger
        // route (for example test 12/13's setOutputStreams) includes the
        // filename in QPDFExc-compatible warning text.
        description: String::from_utf8_lossy(&filename_diagnostic).into_owned(),
        ..PdfOpenOptions::default()
    };
    if let (true, Some(password)) = (n == 35 || n == 36, arg2) {
        // qpdf's test_driver.cc:3494-3496: for these two numbers only, when
        // a second argument is supplied, it is a password, not an unused
        // `arg2` value.
        options.password = os_str_diagnostic_bytes(password).into_owned();
    }
    let mut pdf = match open_bytes {
        Some(open_bytes) => match Pdf::open_mem_owned_with_options(open_bytes, options) {
            Ok(pdf) => pdf,
            Err(error) => {
                return write_open_failure(n, &filename_diagnostic, &error, stdout, stderr);
            }
        },
        None => match Pdf::empty() {
            Ok(pdf) => pdf,
            Err(error) => return write_error(stdout, stderr, &error.to_string()),
        },
    };

    let mut diagnostics_written = 0;
    if emit_new_diagnostics(
        &pdf,
        &mut diagnostics_written,
        &filename_diagnostic,
        stdout,
        stderr,
    )
    .is_err()
    {
        return 2;
    }

    let result = match n {
        0 | 1 => test_0_1::run_test_0_1(
            &mut pdf,
            &filename_diagnostic,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        2 => test_02_09::run_test_2(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        3 => test_02_09::run_test_3(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        4 => test_02_09::run_test_4(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        5 => test_02_09::run_test_5(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        6 => test_02_09::run_test_6(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        7 => test_02_09::run_test_7(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        8 => test_02_09::run_test_8(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        9 => test_02_09::run_test_9(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        10 => test_10_17::run_test_10(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        11 => test_10_17::run_test_11(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        12 => test_10_17::run_test_12(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        13 => test_10_17::run_test_13(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        14 => test_10_17::run_test_14(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        15 => test_10_17::run_test_15(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        16 => test_10_17::run_test_16(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        17 => test_10_17::run_test_17(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        18 => test_18_25::run_test_18(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        19 => test_18_25::run_test_19(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        20 => test_18_25::run_test_20(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        21 => test_18_25::run_test_21(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        22 => test_18_25::run_test_22(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        23 => test_18_25::run_test_23(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        24 => test_18_25::run_test_24(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        25 => test_18_25::run_test_25(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        26 => test_26_33::run_test_26(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        27 => test_26_33::run_test_27(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        28 => test_26_33::run_test_28(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        29 => test_26_33::run_test_29(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        30 => test_26_33::run_test_30(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        31 => test_26_33::run_test_31(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        32 => test_26_33::run_test_32(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        33 => test_26_33::run_test_33(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        34 => test_34_41::run_test_34(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        35 => test_34_41::run_test_35(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        36 => test_34_41::run_test_36(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        37 => test_34_41::run_test_37(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        38 => test_34_41::run_test_38(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        39 => test_34_41::run_test_39(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        40 => test_34_41::run_test_40(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        41 => test_34_41::run_test_41(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        42 => test_42_49::run_test_42(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        43 => test_42_49::run_test_43(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        44 => test_42_49::run_test_44(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        45 => test_42_49::run_test_45(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        46 => test_42_49::run_test_46(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        47 => test_42_49::run_test_47(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        48 => test_42_49::run_test_48(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        49 => test_42_49::run_test_49(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        50 => test_50_55::run_test_50(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        51 => test_50_55::run_test_51(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        52 => test_50_55::run_test_52(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        53 => test_50_55::run_test_53(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        54 => test_50_55::run_test_54(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        55 => test_50_55::run_test_55(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        56 => test_56_63::run_test_56(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        57 => test_56_63::run_test_57(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        58 => test_56_63::run_test_58(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        59 => test_56_63::run_test_59(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        60 => test_56_63::run_test_60(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        61 => test_56_63::run_test_61(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        62 => test_56_63::run_test_62(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        63 => test_56_63::run_test_63(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        64 => test_64_71::run_test_64(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        65 => test_64_71::run_test_65(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        66 => test_64_71::run_test_66(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        67 => test_64_71::run_test_67(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        68 => test_64_71::run_test_68(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        69 => test_64_71::run_test_69(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        70 => test_64_71::run_test_70(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        71 => test_64_71::run_test_71(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        72 => test_72_79::run_test_72(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        73 => test_72_79::run_test_73(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        74 => test_72_79::run_test_74(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        75 => test_72_79::run_test_75(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        76 => test_72_79::run_test_76(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        77 => test_72_79::run_test_77(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        78 => test_72_79::run_test_78(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        79 => test_72_79::run_test_79(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        80 => test_80_87::run_test_80(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        81 => test_80_87::run_test_81(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        82 => test_80_87::run_test_82(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        83 => test_80_87::run_test_83(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        84 => test_80_87::run_test_84(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        85 => test_80_87::run_test_85(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        86 => test_80_87::run_test_86(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        87 => test_80_87::run_test_87(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        88 => test_88_98::run_test_88(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        90 => test_88_98::run_test_90(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        91 => test_88_98::run_test_91(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        92 => test_88_98::run_test_92(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        93 => test_88_98::run_test_93(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        94 => test_88_98::run_test_94(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        95 => test_88_98::run_test_95(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        96 => test_88_98::run_test_96(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        97 => test_88_98::run_test_97(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        98 => test_88_98::run_test_98(
            &mut pdf,
            &filename_diagnostic,
            arg2,
            stdout,
            stderr,
            &mut diagnostics_written,
        ),
        _ => return write_error(stdout, stderr, &format!("invalid test {n}")),
    };
    if let Err(error) = result {
        return write_error(stdout, stderr, &error.to_string());
    }
    // qpdf's test_4 exits immediately after writing its QDF output so the
    // ordinary driver footer is not appended to the binary comparison stream
    // (`qpdf/test_driver.cc:368-372`).
    if n != 4 && writeln!(stdout, "test {n} done").is_err() {
        return 2;
    }
    0
}

/// qpdf's `runtest` input dispatch skips `filename1` for these test numbers
/// (`qpdf/test_driver.cc:3463-3490`). Keep this list at the driver boundary so
/// ignored tests do not acquire accidental filesystem or recovery behavior
/// from the Rust adapter.
fn qpdf_ignores_filename(n: i32) -> bool {
    matches!(n, 61 | 81 | 83 | 84 | 85 | 86 | 87 | 92 | 95 | 96)
}

fn open_pdf_error_bytes(n: i32, filename: &[u8], error: &Error) -> Vec<u8> {
    let suffix: Option<Cow<str>> = match error {
        Error::Parse { message, .. } if n == 0 && message == "xref not found" => {
            Some(Cow::Borrowed(": can't find startxref"))
        }
        // Both of `reconstruct_xref`'s terminal errors throw via the same
        // `damagedPDF("", 0, message)` (`QPDF.cc:601-604,614`), which
        // `QPDFExc::createWhat` (`QPDFExc.cc:18-51`) formats identically as
        // `"<filename>: <message>"`; only flpdf's own "parse error at byte
        // N: " `Display` prefix -- which qpdf's real test-driver never
        // prints for either -- needs stripping here.
        Error::Parse { message, .. }
            if n != 0
                && (message == "unable to find trailer dictionary while recovering damaged file"
                    || message
                        == "error decoding candidate xref stream while recovering damaged file") =>
        {
            Some(Cow::Owned(format!(": {message}")))
        }
        _ => None,
    };
    if let Some(suffix) = suffix {
        let mut output = filename.to_vec();
        output.extend_from_slice(suffix.as_bytes());
        output
    } else {
        error.to_string().into_bytes()
    }
}

fn write_open_failure(
    n: i32,
    filename: &[u8],
    error: &Error,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let source = if let Some((source, diagnostics)) = error.open_failure() {
        for diagnostic in diagnostics.entries() {
            if write_warning(filename, diagnostic, stdout, stderr).is_err() {
                return 2;
            }
        }
        source
    } else {
        error
    };
    write_error_bytes(stdout, stderr, &open_pdf_error_bytes(n, filename, source))
}

pub(crate) fn open_error_bytes(
    filename: &[u8],
    crt_message: Option<&[u8]>,
    fallback: &io::Error,
) -> Vec<u8> {
    let message = crt_message
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| fallback.to_string().into_bytes());
    let mut output = b"open ".to_vec();
    output.extend_from_slice(filename);
    output.extend_from_slice(b": ");
    output.extend_from_slice(&message);
    output
}

fn strerror_bytes(error_code: libc::c_int) -> Option<Vec<u8>> {
    let message = unsafe { libc::strerror(error_code) };
    (!message.is_null()).then(|| unsafe { CStr::from_ptr(message) }.to_bytes().to_vec())
}

#[cfg(unix)]
pub(crate) fn crt_open_error_message(filename: &OsStr) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;

    let filename = CString::new(filename.as_bytes()).ok()?;
    let mode = CString::new("rb").expect("literal contains no NUL");
    let file = unsafe { libc::fopen(filename.as_ptr(), mode.as_ptr()) };
    if !file.is_null() {
        // Rust's initial read failed but this second, diagnostic CRT open won a race.
        // It provides no matching errno, so preserve the original Rust error as fallback.
        let _ = unsafe { libc::fclose(file) };
        return None;
    }
    let error_code = io::Error::last_os_error().raw_os_error()?;
    strerror_bytes(error_code)
}

#[cfg(windows)]
unsafe extern "C" {
    fn _wfopen_s(
        file: *mut *mut libc::FILE,
        filename: *const libc::wchar_t,
        mode: *const libc::wchar_t,
    ) -> libc::c_int;
}

#[cfg(all(unix, test))]
fn has_interior_nul(filename: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;

    filename.as_bytes().contains(&0)
}

#[cfg(windows)]
fn has_interior_nul(filename: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;

    filename.encode_wide().any(|unit| unit == 0)
}

#[cfg(windows)]
pub(crate) fn crt_open_error_message(filename: &OsStr) -> Option<Vec<u8>> {
    use std::os::windows::ffi::OsStrExt;

    if has_interior_nul(filename) {
        // `_wfopen_s` would stop at the NUL and probe a different path. With no
        // CRT evidence for Rust's failed path, preserve the original fallback.
        return None;
    }
    let filename: Vec<libc::wchar_t> = filename.encode_wide().chain(std::iter::once(0)).collect();
    let mode = [b'r' as libc::wchar_t, b'b' as libc::wchar_t, 0];
    let mut file = std::ptr::null_mut();
    let error_code = unsafe { _wfopen_s(&mut file, filename.as_ptr(), mode.as_ptr()) };
    if error_code == 0 {
        // Rust's initial read failed but this second, diagnostic CRT open won a race.
        // It provides no matching errno, so preserve the original Rust error as fallback.
        if !file.is_null() {
            let _ = unsafe { libc::fclose(file) };
        }
        return None;
    }
    strerror_bytes(error_code)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn crt_open_error_message(_filename: &OsStr) -> Option<Vec<u8>> {
    None
}

#[cfg(unix)]
pub(crate) fn os_str_diagnostic_bytes(value: &OsStr) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;

    Cow::Borrowed(value.as_bytes())
}

#[cfg(not(unix))]
pub(crate) fn os_str_diagnostic_bytes(value: &OsStr) -> Cow<'_, [u8]> {
    // This fallback is lossy only for unpaired wide values. Valid-Unicode Windows
    // diagnostics remain byte-identical to their prior UTF-8 output.
    Cow::Owned(value.to_string_lossy().into_owned().into_bytes())
}

fn decimal_error(prefix: &[u8], input: &[u8], suffix: &[u8]) -> Vec<u8> {
    let mut message = prefix.to_vec();
    message.extend_from_slice(input);
    message.extend_from_slice(suffix);
    message
}

fn parse_test_number(input: &[u8]) -> Result<i32, Vec<u8>> {
    let bytes = input;
    let mut index = 0;
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }

    let negative = match bytes.get(index) {
        Some(b'-') => {
            index += 1;
            true
        }
        Some(b'+') => {
            index += 1;
            false
        }
        _ => false,
    };

    let mut value = 0_u64;
    let mut consumed_digit = false;
    while let Some(digit) = bytes.get(index).and_then(|byte| byte.checked_sub(b'0')) {
        if digit > 9 {
            break;
        }
        consumed_digit = true;
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(digit)))
            .ok_or_else(|| {
                decimal_error(
                    b"overflow/underflow converting ",
                    input,
                    b" to 64-bit integer",
                )
            })?;
        index += 1;
    }

    if !consumed_digit {
        return Ok(0);
    }

    let i64_value = if negative {
        const I64_MIN_MAGNITUDE: u64 = 9_223_372_036_854_775_808;
        if value > I64_MIN_MAGNITUDE {
            return Err(decimal_error(
                b"overflow/underflow converting ",
                input,
                b" to 64-bit integer",
            ));
        }
        if value == I64_MIN_MAGNITUDE {
            i64::MIN
        } else {
            -(value as i64)
        }
    } else {
        if value > i64::MAX as u64 {
            return Err(decimal_error(
                b"overflow/underflow converting ",
                input,
                b" to 64-bit integer",
            ));
        }
        value as i64
    };

    i32::try_from(i64_value).map_err(|_| {
        format!(
            "integer out of range converting {i64_value} from a 8-byte signed type to a 4-byte signed type"
        )
        .into_bytes()
    })
}

pub(crate) fn emit_new_diagnostics<R: io::Read + io::Seek>(
    pdf: &Pdf<R>,
    diagnostics_written: &mut usize,
    filename: &[u8],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<()> {
    let diagnostics = pdf.repair_diagnostics();
    let entries = diagnostics.entries();
    for diagnostic in &entries[*diagnostics_written..] {
        write_warning(filename, diagnostic, stdout, stderr)?;
    }
    *diagnostics_written = entries.len();
    Ok(())
}

pub(crate) fn write_warning(
    filename: &[u8],
    diagnostic: &Diagnostic,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<()> {
    let message = diagnostic.message.as_str();
    if let Some(exception) = format_nntree_exception(filename, message) {
        let mut line = b"WARNING: ".to_vec();
        line.extend_from_slice(&exception);
        return write_stderr_bytes(stdout, stderr, &line);
    }
    if diagnostic.is_object_warning() {
        let mut line = b"WARNING: ".to_vec();
        line.extend_from_slice(message.as_bytes());
        return write_stderr_bytes(stdout, stderr, &line);
    }
    let offset = diagnostic.offset;
    let mut line = b"WARNING: ".to_vec();
    line.extend_from_slice(filename);
    if message.starts_with('(') {
        line.push(b' ');
    } else if let Some(offset) = offset {
        line.extend_from_slice(format!(" (offset {offset}): ").as_bytes());
    } else {
        line.extend_from_slice(b": ");
    }
    line.extend_from_slice(message.as_bytes());
    write_stderr_bytes(stdout, stderr, &line)
}

/// Reproduce qpdf's `QPDFExc::createWhat` for the structural messages emitted
/// by the canonical NNTree implementation.
///
/// flpdf stores the object description and detail together in a diagnostic
/// message so the core warning sink remains generic. qpdf keeps those fields
/// separate until `QPDF::warn` constructs the exception. The qtest driver is
/// the output boundary, so it restores that composition here, including the
/// filename repeated inside a nested "attempting to repair" detail.
pub(crate) fn format_nntree_exception(filename: &[u8], message: &str) -> Option<Vec<u8>> {
    const PREFIX: &str = "Name/Number tree node";
    if !message.starts_with(PREFIX) {
        return None;
    }
    let separator = message.find(": ")?;
    let object = &message[..separator];
    let detail = &message[separator + 2..];
    let mut result = Vec::new();
    if filename.is_empty() {
        result.extend_from_slice(object.as_bytes());
    } else {
        result.extend_from_slice(filename);
        result.extend_from_slice(b" (");
        result.extend_from_slice(object.as_bytes());
        result.extend_from_slice(b")");
    }
    result.extend_from_slice(b": ");

    if let Some(nested_start) = detail.find(PREFIX) {
        let before_nested = &detail[..nested_start];
        if before_nested.ends_with("error: ") {
            result.extend_from_slice(before_nested.as_bytes());
            if let Some(nested) = format_nntree_exception(filename, &detail[nested_start..]) {
                result.extend_from_slice(&nested);
            } else {
                result.extend_from_slice(&detail.as_bytes()[nested_start..]);
            }
            return Some(result);
        }
    }
    result.extend_from_slice(detail.as_bytes());
    Some(result)
}

fn write_error(stdout: &mut dyn Write, stderr: &mut dyn Write, message: &str) -> u8 {
    write_error_bytes(stdout, stderr, message.as_bytes())
}

fn write_error_bytes(stdout: &mut dyn Write, stderr: &mut dyn Write, message: &[u8]) -> u8 {
    let _ = write_stderr_bytes(stdout, stderr, message);
    2
}

fn write_stderr_bytes(
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    message: &[u8],
) -> io::Result<()> {
    stdout.flush()?;
    stderr.write_all(message)?;
    stderr.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::{
        crt_open_error_message, format_nntree_exception, has_interior_nul, open_error_bytes,
        open_pdf_error_bytes, run, write_error_bytes, write_warning,
    };
    use flpdf::Diagnostic;
    use std::{
        ffi::{OsStr, OsString},
        io::{self, Write},
    };

    #[cfg(unix)]
    #[test]
    fn usage_preserves_non_utf8_backslash_and_exe_suffix() {
        use std::os::unix::ffi::OsStringExt;

        let args = vec![OsString::from_vec(b"/tmp/test-\xff\\driver.exe".to_vec())];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        assert_eq!(run(&args, &mut stdout, &mut stderr), 2);
        assert!(stdout.is_empty());
        assert_eq!(stderr, b"Usage: test-\xff\\driver.exe n filename1 [arg2]\n");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_test_number_without_decimal_prefix_dispatches_zero() {
        use std::os::unix::ffi::OsStringExt;

        let args = vec![
            OsString::from("flpdf-test-driver"),
            OsString::from_vec(vec![0xff]),
            OsString::from(fixture("direct_null")),
        ];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        assert_eq!(run(&args, &mut stdout, &mut stderr), 0);
        assert!(stdout.ends_with(b"test 0 done\n"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn test_62_integer_accessors_match_qpdf_output() {
        let args = vec![
            OsString::from("flpdf-test-driver"),
            OsString::from("62"),
            OsString::from(format!(
                "{}/../../tests/fixtures/minimal.pdf",
                env!("CARGO_MANIFEST_DIR")
            )),
        ];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        assert_eq!(run(&args, &mut stdout, &mut stderr), 0);
        assert_eq!(stdout, b"test 62 done\n");
        assert_eq!(
            stderr,
            b"requested value of integer is too big; returning INT_MAX\n\
requested value of unsigned integer is too big; returning UINT_MAX\n\
unsigned value request for negative number; returning 0\n\
requested value of integer is too small; returning INT_MIN\n\
unsigned integer value request for negative number; returning 0\n\
requested value of integer is too big; returning INT_MAX\n"
        );
    }

    fn fixture(name: &str) -> String {
        format!(
            "{}/../../tests/fixtures/test_driver/{name}.pdf",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    #[test]
    fn open_error_bytes_preserve_non_utf8_crt_message_bytes() {
        let fallback = io::Error::other("fallback must not be used");
        assert_eq!(
            open_error_bytes(b"input.pdf", Some(&[0xff, b'!']), &fallback),
            b"open input.pdf: \xff!"
        );
    }

    #[test]
    fn interior_nul_guard_rejects_a_path_that_would_be_truncated_by_the_crt() {
        assert!(has_interior_nul(OsStr::new("before\0after")));
        assert!(!has_interior_nul(OsStr::new("ordinary.pdf")));
    }

    #[test]
    fn name_number_tree_warning_uses_qpdf_object_context() {
        let diagnostic = Diagnostic::warning(
            "Name/Number tree node (object 14): name/number tree node has neither non-empty /Nums nor /Kids",
            None,
        );
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        write_warning(b"number-tree.pdf", &diagnostic, &mut stdout, &mut stderr)
            .expect("warning output");

        assert!(stdout.is_empty());
        assert_eq!(
            stderr,
            b"WARNING: number-tree.pdf (Name/Number tree node (object 14)): name/number tree node has neither non-empty /Nums nor /Kids\n"
        );
    }

    #[test]
    fn name_number_tree_repair_warning_formats_nested_qpdf_contexts() {
        let diagnostic = Diagnostic::warning(
            "Name/Number tree node (object 24): attempting to repair after error: Name/Number tree node (object 25): node is missing /Limits",
            None,
        );
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        write_warning(b"number-tree.pdf", &diagnostic, &mut stdout, &mut stderr)
            .expect("warning output");

        assert_eq!(
            stderr,
            b"WARNING: number-tree.pdf (Name/Number tree node (object 24)): attempting to repair after error: number-tree.pdf (Name/Number tree node (object 25)): node is missing /Limits\n"
        );
    }

    #[test]
    fn name_number_tree_exception_handles_empty_filename_and_malformed_nested_detail() {
        assert_eq!(
            format_nntree_exception(b"", "Name/Number tree node: invalid tree")
                .expect("empty filename context"),
            b"Name/Number tree node: invalid tree"
        );
        assert_eq!(
            format_nntree_exception(
                b"number-tree.pdf",
                "Name/Number tree node (object 24): attempting to repair after error: Name/Number tree node without detail"
            )
            .expect("malformed nested context"),
            b"number-tree.pdf (Name/Number tree node (object 24)): attempting to repair after error: Name/Number tree node without detail"
        );
    }

    #[test]
    fn open_error_bytes_fall_back_only_without_a_crt_message() {
        let fallback = io::Error::other("fallback message");
        assert_eq!(
            open_error_bytes(b"input.pdf", None, &fallback),
            b"open input.pdf: fallback message"
        );
    }

    #[test]
    fn ordinary_pdf_open_error_uses_the_error_display() {
        let error = flpdf::Error::System("ordinary open failure".to_string());

        assert_eq!(
            open_pdf_error_bytes(1, b"input.pdf", &error),
            b"ordinary open failure"
        );
    }

    #[test]
    fn no_trailer_candidate_error_gets_the_qpdf_filename_prefix() {
        let error = flpdf::Error::parse(
            0,
            "unable to find trailer dictionary while recovering damaged file",
        );

        assert_eq!(
            open_pdf_error_bytes(1, b"input.pdf", &error),
            b"input.pdf: unable to find trailer dictionary while recovering damaged file"
        );
    }

    #[test]
    fn candidate_decode_failure_error_gets_the_qpdf_filename_prefix() {
        // `QPDFExc::createWhat` (`QPDFExc.cc:18-51`) wraps every
        // `damagedPDF("", 0, message)` throw -- both `reconstruct_xref`
        // terminal errors use it (`QPDF.cc:601-604,614`) -- as
        // `"<filename>: <message>"`. Only the "no candidate at all" branch
        // had this treatment; the newer "candidate found but undecodable"
        // message must get the identical prefix, not flpdf's own
        // "parse error at byte N: " `Display` wording.
        let error = flpdf::Error::parse(
            0,
            "error decoding candidate xref stream while recovering damaged file",
        );

        assert_eq!(
            open_pdf_error_bytes(1, b"input.pdf", &error),
            b"input.pdf: error decoding candidate xref stream while recovering damaged file"
        );
    }

    #[test]
    fn byte_error_writer_emits_raw_message_bytes_and_newline() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            write_error_bytes(&mut stdout, &mut stderr, &[0xff, b'!']),
            2
        );
        assert_eq!(stderr, b"\xff!\n");
    }

    #[cfg(unix)]
    #[test]
    fn unix_crt_open_failure_supplies_raw_strerror_bytes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let missing = directory.path().join("missing.pdf");
        let message = crt_open_error_message(missing.as_os_str())
            .expect("fopen failure must supply strerror bytes");
        assert!(!message.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn unix_crt_open_success_is_not_misreported_as_an_error() {
        assert!(crt_open_error_message(OsStr::new(&fixture("direct_null"))).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_crt_open_failure_supplies_raw_strerror_bytes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let missing = directory.path().join("missing.pdf");
        let message = crt_open_error_message(missing.as_os_str())
            .expect("_wfopen_s failure must supply strerror bytes");
        assert!(!message.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn windows_crt_probe_skips_an_interior_nul_path() {
        assert!(has_interior_nul(OsStr::new("before\0after")));
        assert!(crt_open_error_message(OsStr::new("before\0after")).is_none());
    }

    struct FlushFailure;

    impl Write for FlushFailure {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("flush failed"))
        }
    }

    struct WriteFailure;

    impl Write for WriteFailure {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FirstWriteFailure {
        attempted: Vec<u8>,
        writes: usize,
    }

    impl Write for FirstWriteFailure {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            self.attempted.extend_from_slice(buf);
            Err(io::Error::other("warning write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FooterFailure {
        bytes: Vec<u8>,
    }

    impl Write for FooterFailure {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if buf.windows(b"test ".len()).any(|window| window == b"test ") {
                return Err(io::Error::other("footer failed"));
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn repair_warning_flush_failure_exits_two() {
        let args = vec![
            OsString::from("flpdf-test-driver"),
            OsString::from("1"),
            OsString::from(fixture("repairable_input")),
        ];
        let mut stdout = FlushFailure;
        let mut stderr = Vec::new();
        assert_eq!(stdout.write(b"probe").expect("probe write"), 5);
        assert_eq!(run(&args, &mut stdout, &mut stderr), 2);
        assert!(stderr.is_empty());
    }

    #[test]
    fn failed_open_warning_write_failure_skips_terminal_error() {
        let args = vec![
            OsString::from("flpdf-test-driver"),
            OsString::from("1"),
            OsString::from(fixture("open_repair_failure")),
        ];
        let mut stdout = Vec::new();
        let mut stderr = FirstWriteFailure::default();

        assert_eq!(run(&args, &mut stdout, &mut stderr), 2);
        assert_eq!(stderr.writes, 1);
        assert!(stderr.attempted.starts_with(b"WARNING:"));
        assert!(!stderr
            .attempted
            .windows(b"unable to recover".len())
            .any(|window| window == b"unable to recover"));
        stderr.flush().expect("flush failure writer");
    }

    #[test]
    fn test_body_write_failure_is_reported_and_exits_two() {
        let args = vec![
            OsString::from("flpdf-test-driver"),
            OsString::from("1"),
            OsString::from(fixture("direct_null")),
        ];
        let mut stdout = WriteFailure;
        let mut stderr = Vec::new();
        assert_eq!(run(&args, &mut stdout, &mut stderr), 2);
        assert_eq!(stderr, b"I/O error: write failed\n");
    }

    #[test]
    fn footer_write_failure_exits_two() {
        let args = vec![
            OsString::from("flpdf-test-driver"),
            OsString::from("1"),
            OsString::from(fixture("direct_null")),
        ];
        let mut stdout = FooterFailure::default();
        let mut stderr = Vec::new();
        assert_eq!(run(&args, &mut stdout, &mut stderr), 2);
        assert!(stderr.is_empty());
        assert!(stdout.bytes.ends_with(b"unparseResolved: null\n"));
        stdout.flush().expect("flush footer writer");
    }
}
