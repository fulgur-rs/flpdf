#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'Usage: %s [--generate|--check]\n' "$0" >&2
}

mode=${1:---generate}
case "$mode" in
    --generate | --check) ;;
    *) usage; exit 2 ;;
esac

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
cd "$script_dir"

fixture_names=(
    repairable_input
    open_repair_failure
    implicit_null
    direct_null
    dangling_ref
    indirect_null
    indirect_bool_true
    indirect_bool_false
    chained_reference
    integer
    real
    string_hex_literal
    name_escape
    array_indirect
    dict_keys
    dict_dangling_value
    dict_escaped_key
    stream_flate
    stream_indirect_filter
    stream_chained_filter
    stream_indirect_filter_array
    stream_indirect_decode_parms
    stream_indirect_decode_parms_container
    stream_decode_parms_direct_null
    stream_decode_parms_indirect_null
    stream_decode_parms_length_mismatch
    stream_offset_false_markers
    stream_unknown_decode_param
    stream_deep_invalid_filter
    stream_flate_error
    stream_filter_error_then_warning
    stream_asciihex_odd_nibble_recovery
    stream_asciihex_data_before_error
    stream_asciihex_downstream_cleanup_after_error
    stream_unfilterable
    stream_unsupported_filter_skips_decode_parms
)

generate_all() {
    command -v python3 >/dev/null || {
        printf 'python3 is required to generate test_driver fixtures\n' >&2
        exit 1
    }
    python3 - "$script_dir" <<'PYEOF'
import os
import sys
import zlib

out_dir = sys.argv[1]


def build_pdf(
    qtest: bytes | None,
    extras: dict[int, bytes],
    bad_startxref: bool = False,
    object_leader: bytes = b"",
) -> bytes:
    objects: dict[int, bytes] = {
        1: b"<< /Type /Catalog /Pages 2 0 R >>",
        2: b"<< /Type /Pages /Count 0 /Kids [ ] >>",
    }
    objects.update(extras)
    max_object = max(objects)
    data = bytearray(b"%PDF-1.7\n")
    offsets: dict[int, int] = {}
    for number in sorted(objects):
        data += object_leader
        offsets[number] = len(data)
        data += f"{number} 0 obj\n".encode("ascii")
        data += objects[number]
        data += b"\nendobj\n"
    xref_offset = len(data)
    data += f"xref\n0 {max_object + 1}\n".encode("ascii")
    data += b"0000000000 65535 f \n"
    for number in range(1, max_object + 1):
        if number in offsets:
            data += f"{offsets[number]:010d} 00000 n \n".encode("ascii")
        else:
            data += b"0000000000 00000 f \n"
    data += f"trailer\n<< /Size {max_object + 1} /Root 1 0 R".encode("ascii")
    if qtest is not None:
        data += b" /QTest " + qtest
    data += b" >>\nstartxref\n"
    data += (b"0" if bad_startxref else str(xref_offset).encode("ascii"))
    data += b"\n%%EOF\n"
    return bytes(data)


def stream(dictionary: bytes, payload: bytes) -> bytes:
    return (
        b"<< "
        + dictionary
        + b" /Length "
        + str(len(payload)).encode("ascii")
        + b" >>\nstream\n"
        + payload
        + b"\nendstream"
    )


def write(name: str, data: bytes) -> None:
    with open(os.path.join(out_dir, name + ".pdf"), "wb") as output:
        output.write(data)


write("repairable_input", build_pdf(b"true", {}, bad_startxref=True))
write("open_repair_failure", b"%PDF-1.7\nstartxref\n0\n%%EOF\n")
write("implicit_null", build_pdf(None, {}))
write("direct_null", build_pdf(b"null", {}))
write("dangling_ref", build_pdf(b"99 0 R", {}))
write("indirect_null", build_pdf(b"6 0 R", {6: b"null"}))
write("indirect_bool_true", build_pdf(b"6 0 R", {6: b"true"}))
write("indirect_bool_false", build_pdf(b"6 0 R", {6: b"false"}))
write("chained_reference", build_pdf(b"6 0 R", {6: b"7 0 R", 7: b"true"}))
write("integer", build_pdf(b"6 0 R", {6: b"42"}))
write("real", build_pdf(b"6 0 R", {6: b"1.50"}))
write("string_hex_literal", build_pdf(b"6 0 R", {6: b"(a\\nb)"}))
write("name_escape", build_pdf(b"6 0 R", {6: b"/hex#20strings"}))
write(
    "array_indirect",
    build_pdf(
        b"6 0 R",
        {
            6: b"[ /literal null /indirect 7 0 R /undefined 99 0 R 0.0 -0.0 0. -0. ]",
            7: b"true",
        },
    ),
)
write(
    "dict_keys",
    build_pdf(b"6 0 R", {6: b"<< /b false /a 7 0 R >>", 7: b"true"}),
)
write(
    "dict_dangling_value",
    build_pdf(b"6 0 R", {6: b"<< /a (a) /gone 99 0 R >>"}),
)
write(
    "dict_escaped_key",
    build_pdf(b"6 0 R", {6: b"<< /hex#20strings true >>"}),
)

flate_abc = zlib.compress(b"abc")
write(
    "stream_flate",
    build_pdf(b"6 0 R", {6: stream(b"/Filter /FlateDecode", flate_abc)}),
)
write(
    "stream_indirect_filter",
    build_pdf(
        b"6 0 R",
        {6: stream(b"/Filter 7 0 R", flate_abc), 7: b"/FlateDecode"},
    ),
)
write(
    "stream_chained_filter",
    build_pdf(
        b"6 0 R",
        {
            6: stream(b"/Filter 7 0 R", flate_abc),
            7: b"8 0 R",
            8: b"/FlateDecode",
        },
    ),
)
write(
    "stream_indirect_filter_array",
    build_pdf(
        b"6 0 R",
        {6: stream(b"/Filter [ 7 0 R ]", flate_abc), 7: b"/FlateDecode"},
    ),
)

predictor_payload = zlib.compress(b"\x00abc")
write(
    "stream_indirect_decode_parms",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Filter /FlateDecode /DecodeParms << /Predictor 7 0 R /Columns 8 0 R >>",
                predictor_payload,
            ),
            7: b"12",
            8: b"3",
        },
    ),
)
write(
    "stream_indirect_decode_parms_container",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Filter [ /FlateDecode ] /DecodeParms [ 7 0 R ]",
                predictor_payload,
            ),
            7: b"<< /Predictor 12 /Columns 3 >>",
        },
    ),
)
write(
    "stream_decode_parms_direct_null",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Filter /FlateDecode /DecodeParms << /Predictor null >>",
                flate_abc,
            ),
        },
    ),
)
write(
    "stream_decode_parms_indirect_null",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Filter /FlateDecode /DecodeParms << /Predictor 7 0 R >>",
                flate_abc,
            ),
            7: b"null",
        },
    ),
)
write(
    "stream_decode_parms_length_mismatch",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Filter [ /FlateDecode /FlateDecode ] /DecodeParms [ null ]",
                b"abc",
            ),
        },
    ),
)
write(
    "stream_offset_false_markers",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Note (first\nstream\nsecond) "
                b"/Filter [ /FlateDecode /FlateDecode ] /DecodeParms [ null ]",
                b"abc",
            ),
        },
        object_leader=b" ",
    ),
)
write(
    "stream_unfilterable",
    build_pdf(b"6 0 R", {6: stream(b"/Filter /BogusDecode", b"abc")}),
)
write(
    "stream_unsupported_filter_skips_decode_parms",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Filter [ /FlateDecode /BogusDecode ] "
                b"/DecodeParms [ 7 0 R null null ]",
                flate_abc,
            ),
            7: b"<< /Predictor 99 0 R >>",
        },
    ),
)
write(
    "stream_flate_error",
    build_pdf(b"6 0 R", {6: stream(b"/Filter /FlateDecode", b"abc")}),
)
write(
    "stream_filter_error_then_warning",
    build_pdf(
        b"6 0 R",
        {6: stream(b"/Filter [ /ASCIIHexDecode /FlateDecode ]", b"78G")},
    ),
)
write(
    "stream_asciihex_odd_nibble_recovery",
    build_pdf(b"6 0 R", {6: stream(b"/Filter /AHx", b"4G ")}),
)
write(
    "stream_asciihex_data_before_error",
    build_pdf(
        b"6 0 R",
        {6: stream(b"/Filter [ /AHx /AHx ]", b"3431G")},
    ),
)
write(
    "stream_asciihex_downstream_cleanup_after_error",
    build_pdf(
        b"6 0 R",
        {6: stream(b"/Filter [ /AHx /AHx ]", b"343G")},
    ),
)

metadata = b"1"
for _ in range(64):
    metadata = b"[ " + metadata + b" ]"
write(
    "stream_unknown_decode_param",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Filter /FlateDecode /DecodeParms << /Metadata "
                + metadata
                + b" >>",
                flate_abc,
            ),
        },
    ),
)

deep_invalid_filter = b"/FlateDecode"
for _ in range(64):
    deep_invalid_filter = b"[ " + deep_invalid_filter + b" ]"
write(
    "stream_deep_invalid_filter",
    build_pdf(b"6 0 R", {6: stream(b"/Filter " + deep_invalid_filter, b"abc")}),
)
PYEOF
}

check_all() {
    command -v qpdf >/dev/null || {
        printf 'qpdf is required to check test_driver fixtures\n' >&2
        exit 1
    }
    for name in "${fixture_names[@]}"; do
        test -f "${name}.pdf" || {
            printf 'missing fixture: %s.pdf\n' "$name" >&2
            exit 1
        }
        set +e
        qpdf --check "${name}.pdf" >/dev/null 2>&1
        status=$?
        set -e
        if [[ "$name" == open_repair_failure ]]; then
            expected_status=2
        else
            expected_status='0 or 3'
        fi
        if [[ "$name" == open_repair_failure && "$status" -ne 2 ]] ||
            [[ "$name" != open_repair_failure && "$status" -ne 0 && "$status" -ne 3 ]]; then
            printf 'qpdf rejected %s.pdf with exit %d\n' "$name" "$status" >&2
            printf 'expected exit %s\n' "$expected_status" >&2
            exit 1
        fi
    done
}

if [[ "$mode" == --generate ]]; then
    generate_all
fi
check_all
