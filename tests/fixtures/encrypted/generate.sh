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

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../../.." && pwd)
src="$repo_root/tests/fixtures/minimal.pdf"

need_qpdf() {
    if ! command -v qpdf >/dev/null 2>&1; then
        printf 'qpdf is required to generate/check encrypted fixtures\n' >&2
        exit 1
    fi
}

fixture_rows() {
    cat <<'ROWS'
v1-rc4-40-r2.pdf|user-v1|owner-v1|40||a3668e477aa1c1e138f9ffa965db3d81cb6abaceeb8b7269d8e8622d29df753d|R = 2|
v2-rc4-128-r3.pdf|user-v2|owner-v2|128|--use-aes=n|a3668e477aa1c1e138f9ffa965db3d81cb6abaceeb8b7269d8e8622d29df753d|R = 3|
v4-rc4-128-r4.pdf|user-v4-rc4|owner-v4-rc4|128|--use-aes=n --force-V4|a3668e477aa1c1e138f9ffa965db3d81cb6abaceeb8b7269d8e8622d29df753d|R = 4|file encryption method: RC4
v4-aes-128-r4.pdf|user-v4-aes|owner-v4-aes|128|--use-aes=y|a3668e477aa1c1e138f9ffa965db3d81cb6abaceeb8b7269d8e8622d29df753d|R = 4|file encryption method: AESv2
v5-aes-256-r5.pdf|user-v5-r5|owner-v5-r5|256|--force-R5|ee894a875f95c6e53451fa3bb84683af3adbb329dba4b58cef502054a3a5d518|R = 5|file encryption method: AESv3
v5-aes-256-r6.pdf|user-v5-r6|owner-v5-r6|256||f54d7aa9e6150ce3dc675c615ca571f2c8a924e0853e5ad215674951655ed42a|R = 6|file encryption method: AESv3
v5-aes-256-r6-utf8.pdf|café|résumé|256||f54d7aa9e6150ce3dc675c615ca571f2c8a924e0853e5ad215674951655ed42a|R = 6|file encryption method: AESv3
ROWS
}

generate_fixture() {
    local file=$1 user=$2 owner=$3 bits=$4 options=$5 out=$6
    local weak=()

    if [[ $bits != 256 ]]; then
        weak=(--allow-weak-crypto)
    fi

    # shellcheck disable=SC2206
    local option_args=($options)
    qpdf --static-id "${weak[@]}" \
        --encrypt "$user" "$owner" "$bits" "${option_args[@]}" -- \
        "$src" "$out"
}

sha256_file() {
    sha256sum "$1" | cut -d ' ' -f 1
}

check_fixture() {
    local file=$1 user=$2 expected_sha=$3 expected_r=$4 expected_method=$5
    local path="$script_dir/$file"
    local tmpdir plain actual show

    if [[ ! -f $path ]]; then
        printf 'missing fixture: %s\n' "$path" >&2
        return 1
    fi

    qpdf --password="$user" --check "$path" >/dev/null

    tmpdir=$(mktemp -d)
    trap 'rm -rf "$tmpdir"' RETURN
    plain="$tmpdir/plain.pdf"
    qpdf --password="$user" --decrypt --static-id "$path" "$plain"
    actual=$(sha256_file "$plain")
    if [[ $actual != "$expected_sha" ]]; then
        printf '%s: plaintext sha256 mismatch: got %s expected %s\n' \
            "$file" "$actual" "$expected_sha" >&2
        return 1
    fi

    show=$(qpdf --show-encryption --password="$user" "$path")
    if [[ $show != *"$expected_r"* ]]; then
        printf '%s: missing encryption revision marker %s\n' "$file" "$expected_r" >&2
        return 1
    fi
    if [[ -n $expected_method && $show != *"$expected_method"* ]]; then
        printf '%s: missing encryption method marker %s\n' "$file" "$expected_method" >&2
        return 1
    fi
}

need_qpdf

while IFS='|' read -r file user owner bits options expected_sha expected_r expected_method; do
    case "$mode" in
        --generate)
            generate_fixture "$file" "$user" "$owner" "$bits" "$options" "$script_dir/$file"
            ;;
        --check)
            check_fixture "$file" "$user" "$expected_sha" "$expected_r" "$expected_method"
            ;;
    esac
done < <(fixture_rows)

case "$mode" in
    --generate) printf 'generated encrypted fixtures in %s\n' "$script_dir" ;;
    --check) printf 'encrypted fixture checks passed\n' ;;
esac
