#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'Usage: %s [--check|--regenerate]\n' "$0" >&2
}

mode=${1:---check}
case "$mode" in
    --check | --regenerate) ;;
    *) usage; exit 2 ;;
esac

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
fixture_dir="${repo_root}/tests/fixtures/test_driver"
qpdf_source=$("${repo_root}/scripts/fetch-qpdf-source.sh" --print-path)
qpdf_source=$(CDPATH= cd -- "$qpdf_source" && pwd -P)
qpdf_commit=3b97c9bd266b7c32ea36d3536e22dab77412886d
build_dir=

fixture_names=(
    repairable_input
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
    stream_decode_parms_length_mismatch
    stream_offset_false_markers
    stream_unknown_decode_param
    stream_deep_invalid_filter
    stream_flate_error
    stream_filter_error_then_warning
    stream_asciihex_odd_nibble_recovery
    stream_asciihex_data_before_error
    stream_unfilterable
)

check_source() {
    local actual_commit status
    actual_commit=$(git -C "$qpdf_source" rev-parse --verify HEAD)
    if [[ "$actual_commit" != "$qpdf_commit" ]]; then
        printf 'qpdf-test-driver-diff.sh: expected qpdf commit %s, found %s\n' \
            "$qpdf_commit" "$actual_commit" >&2
        return 1
    fi
    status=$(git -C "$qpdf_source" status --porcelain --untracked-files=all)
    if [[ -n "$status" ]]; then
        printf 'qpdf-test-driver-diff.sh: pinned qpdf source is dirty\n' >&2
        return 1
    fi
}

cleanup() {
    local status=$?
    trap - EXIT
    if [[ -n "$build_dir" ]]; then
        case "$build_dir" in
            /tmp/flpdf-qpdf-test-driver.*)
                if [[ ! -L "$build_dir" && -d "$build_dir" &&
                    "$(stat -c '%u %a' -- "$build_dir")" == "${UID} 700" ]]; then
                    rm -rf -- "$build_dir" || status=1
                else
                    printf 'qpdf-test-driver-diff.sh: refusing unsafe cleanup: %s\n' \
                        "$build_dir" >&2
                    status=1
                fi
                ;;
            *)
                printf 'qpdf-test-driver-diff.sh: refusing escaped cleanup: %s\n' \
                    "$build_dir" >&2
                status=1
                ;;
        esac
    fi
    check_source || status=1
    exit "$status"
}

for command in cargo cmake cmp diff git mktemp stat; do
    command -v "$command" >/dev/null || {
        printf 'qpdf-test-driver-diff.sh: %s is required\n' "$command" >&2
        exit 1
    }
done

check_source
trap cleanup EXIT
build_dir=$(mktemp -d /tmp/flpdf-qpdf-test-driver.XXXXXXXX)
if [[ -L "$build_dir" || ! -d "$build_dir" ||
    "$(stat -c '%u %a' -- "$build_dir")" != "${UID} 700" ]]; then
    printf 'qpdf-test-driver-diff.sh: unsafe build directory: %s\n' "$build_dir" >&2
    exit 1
fi

cmake -S "$qpdf_source" -B "$build_dir" \
    -DCMAKE_BUILD_TYPE=Release \
    -DBUILD_SHARED_LIBS=OFF
cmake --build "$build_dir" --target test_driver --parallel

oracle="${build_dir}/qpdf/test_driver"
if [[ -L "$oracle" || ! -x "$oracle" ]]; then
    printf 'qpdf-test-driver-diff.sh: missing trusted test_driver build artifact\n' >&2
    exit 1
fi

CARGO_TARGET_DIR="${build_dir}/rust-target" cargo build \
    --manifest-path "${repo_root}/Cargo.toml" \
    --locked \
    -p flpdf-qtest-tools \
    --bin flpdf-test-driver
rust_driver="${build_dir}/rust-target/debug/flpdf-test-driver"
if [[ -L "$rust_driver" || ! -x "$rust_driver" ]]; then
    printf 'qpdf-test-driver-diff.sh: missing flpdf-test-driver build artifact\n' >&2
    exit 1
fi

mkdir -m 700 "${build_dir}/oracle" "${build_dir}/rust"

run_cli_probe() {
    local name=$1
    shift
    local oracle_actual="${build_dir}/oracle/cli-${name}.out"
    local rust_actual="${build_dir}/rust/cli-${name}.out"
    local oracle_status rust_status

    set +e
    (
        cd "$fixture_dir"
        "$oracle" "$@"
    ) >"$oracle_actual" 2>&1
    oracle_status=$?
    (
        cd "$fixture_dir"
        "$rust_driver" "$@"
    ) >"$rust_actual" 2>&1
    rust_status=$?
    set -e

    if [[ "$rust_status" -ne "$oracle_status" ]]; then
        printf \
            'qpdf-test-driver-diff.sh: CLI %s status mismatch (qpdf=%d flpdf=%d)\n' \
            "$name" "$oracle_status" "$rust_status" >&2
        exit 1
    fi
    if ! cmp -s -- "$oracle_actual" "$rust_actual"; then
        diff -u -- "$oracle_actual" "$rust_actual" || true
        exit 1
    fi
}

for name in "${fixture_names[@]}"; do
    pdf="${fixture_dir}/${name}.pdf"
    oracle_actual="${build_dir}/oracle/${name}.out"
    rust_actual="${build_dir}/rust/${name}.out"
    [[ -f "$pdf" ]] || {
        printf 'qpdf-test-driver-diff.sh: missing fixture: %s\n' "$pdf" >&2
        exit 1
    }
    set +e
    (
        cd "$fixture_dir"
        "$oracle" 1 "${name}.pdf"
    ) >"$oracle_actual" 2>&1
    oracle_status=$?
    (
        cd "$fixture_dir"
        "$rust_driver" 1 "${name}.pdf"
    ) >"$rust_actual" 2>&1
    rust_status=$?
    set -e

    if [[ "$oracle_status" -ne 0 || "$rust_status" -ne "$oracle_status" ]]; then
        printf \
            'qpdf-test-driver-diff.sh: %s status mismatch (qpdf=%d flpdf=%d expected=0)\n' \
            "$name" "$oracle_status" "$rust_status" >&2
        exit 1
    fi

    if ! cmp -s -- "$oracle_actual" "$rust_actual"; then
        diff -u -- "$oracle_actual" "$rust_actual" || true
        exit 1
    fi

    expected="${fixture_dir}/${name}.out"
    if [[ "$mode" == --regenerate ]]; then
        cp -f -- "$oracle_actual" "$expected"
    elif [[ ! -f "$expected" ]]; then
        printf 'qpdf-test-driver-diff.sh: missing oracle output: %s\n' "$expected" >&2
        exit 1
    elif ! cmp -s -- "$expected" "$oracle_actual"; then
        diff -u -- "$expected" "$oracle_actual" || true
        exit 1
    fi
done

run_cli_probe decimal_prefix 1x direct_null.pdf
run_cli_probe signed_prefix $' \t+1trailing' direct_null.pdf
run_cli_probe no_digits not-a-number direct_null.pdf
run_cli_probe i32_overflow 2147483648 direct_null.pdf
run_cli_probe i64_overflow 9223372036854775808 direct_null.pdf
run_cli_probe i64_underflow -9223372036854775809 direct_null.pdf
run_cli_probe i64_minimum -9223372036854775808 direct_null.pdf
run_cli_probe unsupported_test 99 direct_null.pdf
run_cli_probe missing_path 1 missing-cli-probe.pdf
run_cli_probe no_repair_test_zero 0 repairable_input.pdf

if [[ "$mode" == --regenerate ]]; then
    printf 'regenerated and matched %d qpdf test_driver outputs and 10 CLI probes\n' "${#fixture_names[@]}"
else
    printf 'qpdf and flpdf test_driver outputs match %d fixtures and 10 CLI probes\n' "${#fixture_names[@]}"
fi
