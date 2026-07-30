#!/usr/bin/env bash
set -euo pipefail

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
script_source="${repo_root}/scripts/qpdf-character-encoding-diff.sh"
fixture_root=$(mktemp -d)
fixture_repo="${fixture_root}/repo"
fixture_source="${fixture_root}/qpdf-source"
fake_bin="${fixture_root}/bin"
contract_log="${fixture_root}/contract.log"

cleanup() {
    rm -rf -- "${fixture_root}"
}
trap cleanup EXIT

mkdir -p "${fixture_repo}/scripts" "${fixture_source}" "${fake_bin}"
cp -f -- "$script_source" "${fixture_repo}/scripts/"

git -C "$fixture_source" init -q
git -C "$fixture_source" config user.email contract@example.invalid
git -C "$fixture_source" config user.name contract
printf 'clean\n' >"${fixture_source}/sentinel"
git -C "$fixture_source" add sentinel
git -C "$fixture_source" commit -qm fixture
fixture_commit=$(git -C "$fixture_source" rev-parse HEAD)
sed -i \
    "s/qpdf_commit=[0-9a-f]*/qpdf_commit=${fixture_commit}/" \
    "${fixture_repo}/scripts/qpdf-character-encoding-diff.sh"

cat >"${fixture_repo}/scripts/fetch-qpdf-source.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'fetch' >>"${CONTRACT_LOG}"
printf ' <%s>' "$@" >>"${CONTRACT_LOG}"
printf '\n' >>"${CONTRACT_LOG}"
[[ "$#" == 1 && "$1" == --print-path ]]
printf '%s\n' "${FIXTURE_SOURCE}"
EOF

real_mktemp=$(command -v mktemp)
cat >"${fake_bin}/mktemp" <<EOF
#!/usr/bin/env bash
set -euo pipefail
if [[ -n "\${FAKE_MKTEMP_RESULT:-}" ]]; then
    printf '%s\n' "\${FAKE_MKTEMP_RESULT}"
else
    exec "${real_mktemp}" "\$@"
fi
EOF

cat >"${fake_bin}/fixture-helper" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'run <%s>' "$(basename "$0")" >>"${CONTRACT_LOG}"
printf ' <%s>' "${FLPDF_DIFF_PROBE:-unknown}" >>"${CONTRACT_LOG}"
printf ' <%s>' "$@" >>"${CONTRACT_LOG}"
printf '\n' >>"${CONTRACT_LOG}"
printf 'stdout:%s\n' "${1:-usage}"
printf 'stderr:%s\n' "${1:-usage}" >&2
status=0
case "${1:-usage}" in
    *missing* | *read-dir*) status=134 ;;
    usage) status=2 ;;
esac
if [[ "${FLPDF_DIFF_SIDE:-}" == rust && "${RUST_STATUS_MISMATCH:-0}" == 1 ]]; then
    status=93
fi
if [[ "${FLPDF_DIFF_SIDE:-}" == rust && "${RUST_OUTPUT_MISMATCH:-0}" == 1 ]]; then
    printf 'rust-only mismatch\n'
fi
exit "$status"
EOF

cat >"${fake_bin}/cmake" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'cmake' >>"${CONTRACT_LOG}"
printf ' <%s>' "$@" >>"${CONTRACT_LOG}"
printf '\n' >>"${CONTRACT_LOG}"
if [[ "${1:-}" != --build ]]; then
    build_dir=
    while (($#)); do
        if [[ "$1" == -B ]]; then
            build_dir=$2
            break
        fi
        shift
    done
    [[ -n "$build_dir" ]]
    printf 'build-mode <%s>\n' "$(stat -c %a -- "$build_dir")" >>"${CONTRACT_LOG}"
    exit 0
fi
build_dir=$2
mkdir -p "${build_dir}/qpdf"
for helper in test_pdf_doc_encoding test_pdf_unicode; do
    cp -f -- "${FAKE_BIN}/fixture-helper" "${build_dir}/qpdf/${helper}"
    chmod +x "${build_dir}/qpdf/${helper}"
done
EOF

cat >"${fake_bin}/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'cargo' >>"${CONTRACT_LOG}"
printf ' <%s>' "$@" >>"${CONTRACT_LOG}"
printf '\n' >>"${CONTRACT_LOG}"
mkdir -p "${CARGO_TARGET_DIR}/debug"
for helper in flpdf-test-pdf-doc-encoding flpdf-test-pdf-unicode; do
    cp -f -- "${FAKE_BIN}/fixture-helper" "${CARGO_TARGET_DIR}/debug/${helper}"
    chmod +x "${CARGO_TARGET_DIR}/debug/${helper}"
done
EOF

chmod +x \
    "${fixture_repo}/scripts/fetch-qpdf-source.sh" \
    "${fixture_repo}/scripts/qpdf-character-encoding-diff.sh" \
    "${fake_bin}/mktemp" \
    "${fake_bin}/fixture-helper" \
    "${fake_bin}/cmake" \
    "${fake_bin}/cargo"

run_fixture() {
    env \
        PATH="${fake_bin}:${PATH}" \
        CONTRACT_LOG="${contract_log}" \
        FIXTURE_SOURCE="${fixture_source}" \
        FAKE_BIN="${fake_bin}" \
        FAKE_MKTEMP_RESULT="${FAKE_MKTEMP_RESULT:-}" \
        RUST_STATUS_MISMATCH="${RUST_STATUS_MISMATCH:-0}" \
        RUST_OUTPUT_MISMATCH="${RUST_OUTPUT_MISMATCH:-0}" \
        "${fixture_repo}/scripts/qpdf-character-encoding-diff.sh" --check
}

: >"$contract_log"
run_fixture
grep -Fx 'fetch <--print-path>' "$contract_log"
grep -Fx 'build-mode <700>' "$contract_log"
grep -F 'cmake <--build> ' "$contract_log" |
    grep -F '<--target> <test_pdf_doc_encoding> <test_pdf_unicode>'
grep -F 'cargo <build>' "$contract_log" |
    grep -F '<--bin> <flpdf-test-pdf-doc-encoding>' |
    grep -F '<--bin> <flpdf-test-pdf-unicode>'
for probe in normal.txt crlf.txt blank-final.txt usage missing.txt read-dir; do
    grep -F "<${probe}>" "$contract_log" >/dev/null
done

set +e
RUST_STATUS_MISMATCH=1 run_fixture >"${fixture_root}/status.out" 2>&1
status=$?
set -e
[[ "$status" -ne 0 ]]
grep -F 'status mismatch' "${fixture_root}/status.out"

set +e
RUST_OUTPUT_MISMATCH=1 run_fixture >"${fixture_root}/output.out" 2>&1
status=$?
set -e
[[ "$status" -ne 0 ]]
grep -F 'byte mismatch' "${fixture_root}/output.out"

printf 'dirty\n' >>"${fixture_source}/sentinel"
set +e
run_fixture >"${fixture_root}/dirty.out" 2>&1
status=$?
set -e
[[ "$status" -ne 0 ]]
grep -F 'pinned qpdf source is dirty' "${fixture_root}/dirty.out"
git -C "$fixture_source" checkout -q -- sentinel

unsafe_target="${fixture_root}/unsafe-target"
mkdir -m 700 "$unsafe_target"
printf 'preserve\n' >"${unsafe_target}/sentinel"
set +e
FAKE_MKTEMP_RESULT="$unsafe_target" run_fixture >"${fixture_root}/escaped.out" 2>&1
status=$?
set -e
[[ "$status" -ne 0 ]]
grep -F 'unsafe build directory' "${fixture_root}/escaped.out"
grep -Fx 'preserve' "${unsafe_target}/sentinel"

symlink_target="${fixture_root}/symlink-target"
mkdir -m 700 "$symlink_target"
symlink_build="/tmp/flpdf-qpdf-character-encoding.contract-$$"
ln -s "$symlink_target" "$symlink_build"
set +e
FAKE_MKTEMP_RESULT="$symlink_build" run_fixture >"${fixture_root}/symlink.out" 2>&1
status=$?
set -e
[[ "$status" -ne 0 ]]
grep -F 'unsafe build directory' "${fixture_root}/symlink.out"
[[ -L "$symlink_build" ]]
rm -f -- "$symlink_build"

echo "qpdf-character-encoding differential contract: ok"
