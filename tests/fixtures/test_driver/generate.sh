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

manifest="${script_dir}/fixture-names.txt"
fixture_names=()
while IFS= read -r name || [[ -n "$name" ]]; do
    if [[ -z "$name" || ! "$name" =~ ^[a-z0-9_]+$ ]]; then
        printf 'invalid fixture manifest entry: %s\n' "$name" >&2
        exit 1
    fi
    for existing in "${fixture_names[@]}"; do
        if [[ "$existing" == "$name" ]]; then
            printf 'duplicate fixture manifest entry: %s\n' "$name" >&2
            exit 1
        fi
    done
    fixture_names+=("$name")
done <"$manifest"

generate_all() {
    local out_dir=$1
    command -v python3 >/dev/null || {
        printf 'python3 is required to generate test_driver fixtures\n' >&2
        exit 1
    }
    python3 - "$out_dir" <<'PYEOF'
import os
import sys
import base64
import zlib

out_dir = sys.argv[1]


def build_pdf(
    qtest: bytes | None,
    extras: dict[int, bytes],
    bad_startxref: bool = False,
    omit_startxref: bool = False,
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
    data += b" >>\n"
    if not omit_startxref:
        data += b"startxref\n"
        data += (b"0" if bad_startxref else str(xref_offset).encode("ascii"))
        data += b"\n"
    data += b"%%EOF\n"
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
write(
    "empty_reconstructed_xref",
    b"%PDF-1.7\ntrailer\n<< /Size 1 /Root 1 0 R >>\nstartxref\n0\n%%EOF\n",
)
write(
    "missing_pdf_header",
    build_pdf(b"true", {}).replace(b"%PDF-1.7\n", b"notpdf!!\n", 1),
)
write("leading_material_pdf_header", b"leading material\n" + build_pdf(b"true", {}))
write("missing_startxref", build_pdf(b"true", {}, omit_startxref=True))
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
write(
    "dict_indirect_value_warning",
    build_pdf(
        b"6 0 R",
        {
            6: b"<< /a 7 0 R >>",
            7: b"true\nnot-endobj",
        },
    ),
)
write(
    "dict_duplicate_key",
    build_pdf(b"6 0 R", {6: b"<< /a 1 /a 2 >>"}),
)
write(
    "dict_duplicate_key_after_reference_lookahead",
    build_pdf(b"6 0 R", {6: b"[ 1 0 << /x 1 /x 2 >> ]"}),
)

flate_abc = zlib.compress(b"abc")
write(
    "stream_flate",
    build_pdf(b"6 0 R", {6: stream(b"/Filter /FlateDecode", flate_abc)}),
)

dct_jpeg = base64.b64decode(
    b"/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/2wBDAQkJCQwLDBgNDRgyIRwhMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjL/wAARCAACAAIDAREAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwDTvLW3gvriGGCKOJJGVERAAoBwAAOgr5DG43ExxNSMakklJ9X3Pq8Hg8PLD05SpxbcV0XY/9k="
)
write(
    "stream_dct",
    build_pdf(b"6 0 R", {6: stream(b"/Filter /DCTDecode", dct_jpeg)}),
)
write(
    "stream_dct_alias",
    build_pdf(b"6 0 R", {6: stream(b"/Filter /DCT", dct_jpeg)}),
)
write(
    "stream_dct_decode_parms",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Filter /DCTDecode /DecodeParms << /ColorTransform 0 >>",
                dct_jpeg,
            )
        },
    ),
)
write(
    "stream_dct_malformed",
    build_pdf(b"6 0 R", {6: stream(b"/Filter /DCTDecode", b"abc")}),
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
    "stream_flate_nondict_decode_parms",
    build_pdf(
        b"6 0 R",
        {6: stream(b"/Filter /FlateDecode /DecodeParms 42", flate_abc)},
    ),
)
write(
    "stream_lzw_nondict_decode_parms_array",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Filter [ /LZWDecode ] /DecodeParms [ 42 ]",
                bytes.fromhex("80106020"),
            ),
        },
    ),
)
write(
    "stream_decode_parms_indirect_nondict",
    build_pdf(
        b"6 0 R",
        {
            6: stream(b"/Filter /FlateDecode /DecodeParms 7 0 R", flate_abc),
            7: b"42",
        },
    ),
)
write(
    "stream_decode_parms_indirect_nondict_array",
    build_pdf(
        b"6 0 R",
        {
            6: stream(b"/Filter [ /FlateDecode ] /DecodeParms [ 7 0 R ]", flate_abc),
            7: b"42",
        },
    ),
)
write(
    "stream_decode_parms_indirect_container_nondict",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Filter [ /FlateDecode /FlateDecode ] /DecodeParms 7 0 R",
                zlib.compress(flate_abc),
            ),
            7: b"[ 9 9 ]",
        },
    ),
)


def build_false_next_offset_pdf() -> bytes:
    data = bytearray(b"%PDF-1.7\n")
    offsets: dict[int, int] = {}
    offsets[1] = len(data)
    data += b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"
    offsets[2] = len(data)
    data += b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [ ] >>\nendobj\n"
    offsets[6] = len(data)
    data += (
        b"6 0 obj\n<< /Filter [ /FlateDecode /FlateDecode ] "
        b"/DecodeParms [ null 42 ] /Length 3 >>\nstream\nabc\nendstream\nendobj\n"
    )
    # A dangling in-use xref entry for object 7 whose recorded offset lands
    # inside object 6's own dictionary, before /DecodeParms. Nothing ever
    # resolves object 7; it exists only so flpdf's bounded-read heuristic
    # (which trusts the next recorded offset to bound how far it reads for
    # object 6) sees a false, too-early boundary and must retry unbounded
    # instead of truncating before /DecodeParms.
    false_next = offsets[6] + len(b"6 0 obj\n<< /Filter")
    xref_offset = len(data)
    data += b"xref\n0 8\n"
    data += b"0000000000 65535 f \n"
    data += f"{offsets[1]:010d} 00000 n \n".encode("ascii")
    data += f"{offsets[2]:010d} 00000 n \n".encode("ascii")
    data += b"0000000000 00000 f \n"
    data += b"0000000000 00000 f \n"
    data += b"0000000000 00000 f \n"
    data += f"{offsets[6]:010d} 00000 n \n".encode("ascii")
    data += f"{false_next:010d} 00000 n \n".encode("ascii")
    data += b"trailer\n<< /Size 8 /Root 1 0 R /QTest 6 0 R >>\n"
    data += f"startxref\n{xref_offset}\n".encode("ascii")
    data += b"%%EOF\n"
    return bytes(data)


write("stream_decode_parms_false_next_offset", build_false_next_offset_pdf())
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
    "stream_flate_png_finish_warning_order",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 2 >>",
                zlib.compress(b"\x00A")[:-4],
            ),
        },
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
write(
    "stream_filter_chain_17",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Filter [ "
                + b" ".join([b"/ASCIIHexDecode"] * 17)
                + b" ]",
                b">",
            ),
        },
    ),
)
write(
    "stream_crypt_identity",
    build_pdf(b"6 0 R", {6: stream(b"/Filter /Crypt", b"abc")}),
)
write(
    "stream_crypt_identity_decode_parms_array",
    build_pdf(
        b"6 0 R",
        {
            6: stream(
                b"/Filter [ /Crypt /FlateDecode ] "
                b"/DecodeParms [ << /Type /CryptFilterDecodeParms /Name 42 >> null ]",
                flate_abc,
            ),
        },
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

check_inventory() {
    local directory=$1
    local extension=$2
    local files=()
    local name
    shopt -s nullglob
    files=("$directory"/*."$extension")
    shopt -u nullglob
    if [[ "${#files[@]}" -ne "${#fixture_names[@]}" ]]; then
        printf '%s inventory count differs from manifest: expected %d, found %d\n' \
            "$extension" "${#fixture_names[@]}" "${#files[@]}" >&2
        exit 1
    fi
    for name in "${fixture_names[@]}"; do
        test -f "${directory}/${name}.${extension}" || {
            printf 'missing manifest fixture: %s.%s\n' "$name" "$extension" >&2
            exit 1
        }
    done
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
        if [[ "$name" == open_repair_failure ||
            "$name" == empty_reconstructed_xref ]]; then
            expected_status=2
        else
            expected_status='0 or 3'
        fi
        if [[ ("$name" == open_repair_failure ||
                "$name" == empty_reconstructed_xref) && "$status" -ne 2 ]] ||
            [[ "$name" != open_repair_failure &&
                "$name" != empty_reconstructed_xref &&
                "$status" -ne 0 && "$status" -ne 3 ]]; then
            printf 'qpdf rejected %s.pdf with exit %d\n' "$name" "$status" >&2
            printf 'expected exit %s\n' "$expected_status" >&2
            exit 1
        fi
    done
}

if [[ "$mode" == --generate ]]; then
    generate_all "$script_dir"
    check_inventory "$script_dir" pdf
else
    generated_dir=$(mktemp -d /tmp/flpdf-test-driver-fixtures.XXXXXXXX)
    cleanup_generated() {
        local status=$?
        trap - EXIT
        case "$generated_dir" in
            /tmp/flpdf-test-driver-fixtures.*)
                rm -rf -- "$generated_dir" || status=1
                ;;
            *)
                printf 'refusing unsafe fixture cleanup: %s\n' "$generated_dir" >&2
                status=1
                ;;
        esac
        exit "$status"
    }
    trap cleanup_generated EXIT
    generate_all "$generated_dir"
    check_inventory "$generated_dir" pdf
    check_inventory "$script_dir" pdf
    check_inventory "$script_dir" out
    for name in "${fixture_names[@]}"; do
        cmp -- "${generated_dir}/${name}.pdf" "${script_dir}/${name}.pdf"
    done
fi
check_all
