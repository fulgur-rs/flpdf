#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if ! git check-ignore -q fuzz/corpus; then
    printf '%s\n' 'error: fuzz/corpus must remain gitignored' >&2
    exit 1
fi

if ! command -v sha256sum >/dev/null 2>&1; then
    printf '%s\n' 'error: sha256sum is required to name corpus inputs' >&2
    exit 1
fi

targets=(roundtrip xref filter_pipeline primitive_parser objstm)
for target in "${targets[@]}"; do
    mkdir -p "fuzz/corpus/$target"
done

# Hash-named fixture inputs are idempotent and do not overwrite libFuzzer's
# generated corpus entries. Never clear active corpora: re-seeding preserves
# discoveries from earlier runs.
copy_seed() {
    local target="$1"
    local source_path="$2"
    local source_hash
    local destination

    source_hash="$(sha256sum -- "$source_path")"
    source_hash="${source_hash%% *}"
    destination="fuzz/corpus/$target/${source_hash}-${source_path##*/}"
    if [[ ! -e "$destination" ]]; then
        cp -f -- "$source_path" "$destination"
    fi
}

copy_tree() {
    local target="$1"
    local source_dir="$2"
    local source_path

    while IFS= read -r -d '' source_path; do
        copy_seed "$target" "$source_path"
    done < <(find "$source_dir" -type f -print0)
}

copy_pdfs() {
    local target="$1"
    local source_dir="$2"
    local source_path

    while IFS= read -r -d '' source_path; do
        copy_seed "$target" "$source_path"
    done < <(find "$source_dir" -type f -name '*.pdf' -print0)
}

# Whole-document inputs cover both ordinary parsing/writing and strict/repair
# xref paths. Keep the focused /Prev cases in the xref corpus as well.
copy_pdfs roundtrip "$repo_root/tests/fixtures"
copy_tree roundtrip "$repo_root/fuzz/seeds/roundtrip"
copy_pdfs xref "$repo_root/tests/fixtures"
copy_tree xref "$repo_root/fuzz/seeds/roundtrip"
copy_tree xref "$repo_root/fuzz/seeds/prev_chain"

# The filter target uses a compact binary input format; primitive_parser and
# objstm target specific fixture collections from the reader tests.
copy_tree filter_pipeline "$repo_root/fuzz/seeds/filter_pipeline"
copy_pdfs primitive_parser "$repo_root/tests/fixtures/test_driver"
copy_tree primitive_parser "$repo_root/fuzz/seeds/primitive_parser"
while IFS= read -r -d '' source_path; do
    copy_seed objstm "$source_path"
done < <(find "$repo_root/tests/fixtures" -type f -iname '*objstm*.pdf' -size -4k -print0)
copy_tree objstm "$repo_root/fuzz/seeds/objstm"

for target in "${targets[@]}"; do
    count="$(find "fuzz/corpus/$target" -type f -print | wc -l | tr -d '[:space:]')"
    if [[ "$count" == 0 ]]; then
        printf 'error: generated corpus for %s is empty\n' "$target" >&2
        exit 1
    fi
    printf '%-16s %s inputs\n' "$target" "$count"
done
