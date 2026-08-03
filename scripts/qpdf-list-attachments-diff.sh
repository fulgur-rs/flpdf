#!/usr/bin/env bash
set -euo pipefail

# qpdf-list-attachments-diff.sh — compare `flpdf --list-attachments` against the
# recorded qpdf 11.9.0 output for the pinned qtest inputs.
#
# Unlike the other qpdf-*-diff.sh drivers this one needs no qpdf build: qtest
# checks the expected output into the source tree, so the pinned commit is
# itself the oracle. The commit and cleanliness guards below are what make that
# comparison meaningful.

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
work_dir=

check_source() {
    local actual_commit status
    actual_commit=$(git -C "$qpdf_source" rev-parse --verify HEAD)
    if [[ "$actual_commit" != "$qpdf_commit" ]]; then
        printf \
            'qpdf-list-attachments-diff.sh: expected qpdf commit %s, found %s\n' \
            "$qpdf_commit" "$actual_commit" >&2
        return 1
    fi
    status=$(git -C "$qpdf_source" status --porcelain --untracked-files=all)
    if [[ -n "$status" ]]; then
        printf 'qpdf-list-attachments-diff.sh: pinned qpdf source is dirty\n' >&2
        return 1
    fi
}

cleanup() {
    local status=$?
    trap - EXIT
    if [[ -n "$work_dir" ]]; then
        case "$work_dir" in
            /tmp/flpdf-qpdf-list-attachments.*)
                if [[ ! -L "$work_dir" && -d "$work_dir" &&
                    "$(stat -c '%u %a' -- "$work_dir")" == "${UID} 700" ]]; then
                    rm -rf -- "$work_dir" || status=1
                else
                    printf \
                        'qpdf-list-attachments-diff.sh: refusing unsafe cleanup: %s\n' \
                        "$work_dir" >&2
                    status=1
                fi
                ;;
            *)
                printf \
                    'qpdf-list-attachments-diff.sh: refusing escaped cleanup: %s\n' \
                    "$work_dir" >&2
                status=1
                ;;
        esac
    fi
    check_source || status=1
    exit "$status"
}

for command in cargo cmp diff git mktemp stat; do
    command -v "$command" >/dev/null || {
        printf 'qpdf-list-attachments-diff.sh: %s is required\n' "$command" >&2
        exit 1
    }
done

trap cleanup EXIT
check_source
work_dir=$(mktemp -d /tmp/flpdf-qpdf-list-attachments.XXXXXXXX)
if [[ "$work_dir" != /tmp/flpdf-qpdf-list-attachments.* ||
    -L "$work_dir" || ! -d "$work_dir" ||
    "$(stat -c '%u %a' -- "$work_dir")" != "${UID} 700" ]]; then
    printf \
        'qpdf-list-attachments-diff.sh: unsafe work directory: %s\n' \
        "$work_dir" >&2
    exit 1
fi

CARGO_TARGET_DIR="${work_dir}/rust-target" cargo build \
    --manifest-path "${repo_root}/Cargo.toml" \
    --locked \
    -p flpdf-cli \
    --bin flpdf
flpdf="${work_dir}/rust-target/debug/flpdf"
if [[ -L "$flpdf" || ! -x "$flpdf" ]]; then
    printf 'qpdf-list-attachments-diff.sh: missing flpdf artifact: %s\n' "$flpdf" >&2
    exit 1
fi

qtest_dir="${qpdf_source}/qpdf/qtest/qpdf"

# Each case is: input PDF, expected output recorded by qtest, expected status,
# and the qpdf argv that produced it.
run_case() {
    local input=$1
    local expected=$2
    local expected_status=$3
    shift 3
    local actual="${work_dir}/$(basename "$expected").actual"
    local status

    for artifact in "${qtest_dir}/${input}" "${qtest_dir}/${expected}"; do
        if [[ ! -f "$artifact" ]]; then
            printf \
                'qpdf-list-attachments-diff.sh: missing pinned qtest artifact: %s\n' \
                "$artifact" >&2
            exit 1
        fi
    done

    set +e
    "$flpdf" "$@" "${qtest_dir}/${input}" >"$actual" 2>&1
    status=$?
    set -e

    if [[ "$status" -ne "$expected_status" ]]; then
        printf \
            'qpdf-list-attachments-diff.sh: %s status mismatch (qpdf=%d flpdf=%d)\n' \
            "$input" "$expected_status" "$status" >&2
        exit 1
    fi
    if ! cmp -s -- "${qtest_dir}/${expected}" "$actual"; then
        printf 'qpdf-list-attachments-diff.sh: %s byte mismatch\n' "$input" >&2
        diff -u -- "${qtest_dir}/${expected}" "$actual" || true
        exit 1
    fi
}

# qtest character-encoding, 4th invocation: UTF-16LE strings.
run_case utf16le.pdf utf16le-attachments.out 0 --list-attachments --verbose

printf 'qpdf list-attachments differential: ok\n'
