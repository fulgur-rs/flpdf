#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'Usage: %s --check\n' "$0" >&2
}

if [[ "$#" -ne 1 || "$1" != --check ]]; then
    usage
    exit 2
fi

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
qpdf_source=$("${repo_root}/scripts/fetch-qpdf-source.sh" --print-path)
qpdf_source=$(CDPATH= cd -- "$qpdf_source" && pwd -P)
qpdf_commit=3b97c9bd266b7c32ea36d3536e22dab77412886d
build_dir=

check_source() {
    local actual_commit status
    actual_commit=$(git -C "$qpdf_source" rev-parse --verify HEAD)
    if [[ "$actual_commit" != "$qpdf_commit" ]]; then
        printf \
            'qpdf-character-encoding-diff.sh: expected qpdf commit %s, found %s\n' \
            "$qpdf_commit" "$actual_commit" >&2
        return 1
    fi
    status=$(git -C "$qpdf_source" status --porcelain --untracked-files=all)
    if [[ -n "$status" ]]; then
        printf 'qpdf-character-encoding-diff.sh: pinned qpdf source is dirty\n' >&2
        return 1
    fi
}

cleanup() {
    local status=$?
    trap - EXIT
    if [[ -n "$build_dir" ]]; then
        case "$build_dir" in
            /tmp/flpdf-qpdf-character-encoding.*)
                if [[ ! -L "$build_dir" && -d "$build_dir" &&
                    "$(stat -c '%u %a' -- "$build_dir")" == "${UID} 700" ]]; then
                    rm -rf -- "$build_dir" || status=1
                else
                    printf \
                        'qpdf-character-encoding-diff.sh: refusing unsafe cleanup: %s\n' \
                        "$build_dir" >&2
                    status=1
                fi
                ;;
            *)
                printf \
                    'qpdf-character-encoding-diff.sh: refusing escaped cleanup: %s\n' \
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
        printf 'qpdf-character-encoding-diff.sh: %s is required\n' "$command" >&2
        exit 1
    }
done

trap cleanup EXIT
check_source
build_dir=$(mktemp -d /tmp/flpdf-qpdf-character-encoding.XXXXXXXX)
if [[ "$build_dir" != /tmp/flpdf-qpdf-character-encoding.* ||
    -L "$build_dir" || ! -d "$build_dir" ||
    "$(stat -c '%u %a' -- "$build_dir")" != "${UID} 700" ]]; then
    printf \
        'qpdf-character-encoding-diff.sh: unsafe build directory: %s\n' \
        "$build_dir" >&2
    exit 1
fi

cmake -S "$qpdf_source" -B "$build_dir" \
    -DCMAKE_BUILD_TYPE=Release \
    -DBUILD_SHARED_LIBS=OFF
cmake --build "$build_dir" \
    --target test_pdf_doc_encoding test_pdf_unicode \
    --parallel

oracle_pdfdoc="${build_dir}/qpdf/test_pdf_doc_encoding"
oracle_unicode="${build_dir}/qpdf/test_pdf_unicode"
for artifact in "$oracle_pdfdoc" "$oracle_unicode"; do
    if [[ -L "$artifact" || ! -x "$artifact" ]]; then
        printf \
            'qpdf-character-encoding-diff.sh: missing trusted qpdf artifact: %s\n' \
            "$artifact" >&2
        exit 1
    fi
done

CARGO_TARGET_DIR="${build_dir}/rust-target" cargo build \
    --manifest-path "${repo_root}/Cargo.toml" \
    --locked \
    -p flpdf-qtest-tools \
    --bin flpdf-test-pdf-doc-encoding \
    --bin flpdf-test-pdf-unicode
rust_pdfdoc="${build_dir}/rust-target/debug/flpdf-test-pdf-doc-encoding"
rust_unicode="${build_dir}/rust-target/debug/flpdf-test-pdf-unicode"
for artifact in "$rust_pdfdoc" "$rust_unicode"; do
    if [[ -L "$artifact" || ! -x "$artifact" ]]; then
        printf \
            'qpdf-character-encoding-diff.sh: missing flpdf artifact: %s\n' \
            "$artifact" >&2
        exit 1
    fi
done

probe_dir="${build_dir}/probes"
oracle_output="${build_dir}/oracle"
rust_output="${build_dir}/rust"
mkdir -m 700 "$probe_dir" "$oracle_output" "$rust_output"
printf 'ASCII\nEuro: \342\202\254\nPotato: \360\237\245\224\nBad: \376after\n' \
    >"${probe_dir}/normal.txt"
printf 'first\r\nsecond\r\nthird\r' >"${probe_dir}/crlf.txt"
printf '\nlast-without-newline' >"${probe_dir}/blank-final.txt"
mkdir -m 700 "${probe_dir}/read-dir"

run_one() {
    local side=$1
    local probe=$2
    local argv0=$3
    local binary=$4
    local output=$5
    shift 5

    {
        FLPDF_DIFF_SIDE="$side" FLPDF_DIFF_PROBE="$probe" \
            bash -c 'exec -a "$1" "$2" "${@:3}"' _ \
            "$argv0" "$binary" "$@" >"$output" 2>&1
    } 2>/dev/null
}

run_pair() {
    local mode=$1
    local probe=$2
    local argv0=$3
    local oracle=$4
    local rust=$5
    shift 5
    local oracle_actual="${oracle_output}/${mode}-${probe}.out"
    local rust_actual="${rust_output}/${mode}-${probe}.out"
    local oracle_status rust_status

    set +e
    run_one oracle "$probe" "$argv0" "$oracle" "$oracle_actual" "$@"
    oracle_status=$?
    run_one rust "$probe" "$argv0" "$rust" "$rust_actual" "$@"
    rust_status=$?
    set -e

    if [[ "$rust_status" -ne "$oracle_status" ]]; then
        printf \
            'qpdf-character-encoding-diff.sh: %s/%s status mismatch (qpdf=%d flpdf=%d)\n' \
            "$mode" "$probe" "$oracle_status" "$rust_status" >&2
        exit 1
    fi
    if ! cmp -s -- "$oracle_actual" "$rust_actual"; then
        printf \
            'qpdf-character-encoding-diff.sh: %s/%s byte mismatch\n' \
            "$mode" "$probe" >&2
        diff -u -- "$oracle_actual" "$rust_actual" || true
        exit 1
    fi
}

for mode in pdfdoc unicode; do
    if [[ "$mode" == pdfdoc ]]; then
        argv0=test_pdf_doc_encoding
        oracle=$oracle_pdfdoc
        rust=$rust_pdfdoc
    else
        argv0=test_pdf_unicode
        oracle=$oracle_unicode
        rust=$rust_unicode
    fi

    run_pair "$mode" normal.txt "$argv0" "$oracle" "$rust" \
        "${probe_dir}/normal.txt"
    run_pair "$mode" crlf.txt "$argv0" "$oracle" "$rust" \
        "${probe_dir}/crlf.txt"
    run_pair "$mode" blank-final.txt "$argv0" "$oracle" "$rust" \
        "${probe_dir}/blank-final.txt"
    run_pair "$mode" usage "$argv0" "$oracle" "$rust"
    run_pair "$mode" missing.txt "$argv0" "$oracle" "$rust" \
        "${probe_dir}/missing.txt"
    run_pair "$mode" read-dir "$argv0" "$oracle" "$rust" \
        "${probe_dir}/read-dir"
done

printf 'qpdf character encoding differential: ok\n'
