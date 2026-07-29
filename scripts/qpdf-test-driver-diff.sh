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
    stream_flate
    stream_indirect_filter
    stream_chained_filter
    stream_indirect_filter_array
    stream_indirect_decode_parms
    stream_indirect_decode_parms_container
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

for command in cmake diff git mktemp stat; do
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

mkdir -m 700 "${build_dir}/oracle"
for name in "${fixture_names[@]}"; do
    pdf="${fixture_dir}/${name}.pdf"
    actual="${build_dir}/oracle/${name}.out"
    [[ -f "$pdf" ]] || {
        printf 'qpdf-test-driver-diff.sh: missing fixture: %s\n' "$pdf" >&2
        exit 1
    }
    (
        cd "$fixture_dir"
        "$oracle" 1 "${name}.pdf"
    ) >"$actual" 2>&1

    expected="${fixture_dir}/${name}.out"
    if [[ "$mode" == --regenerate ]]; then
        cp -f -- "$actual" "$expected"
    elif [[ ! -f "$expected" ]]; then
        printf 'qpdf-test-driver-diff.sh: missing oracle output: %s\n' "$expected" >&2
        exit 1
    elif ! cmp -s -- "$expected" "$actual"; then
        diff -u -- "$expected" "$actual" || true
        exit 1
    fi
done

if [[ "$mode" == --regenerate ]]; then
    printf 'regenerated %d qpdf test_driver outputs\n' "${#fixture_names[@]}"
else
    printf 'qpdf test_driver outputs match %d fixtures\n' "${#fixture_names[@]}"
fi
