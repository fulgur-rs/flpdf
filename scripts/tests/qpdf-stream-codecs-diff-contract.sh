#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd -P)"
fixture_root="$(mktemp -d)"
fixture_repo="${fixture_root}/repo"
fixture_source="${fixture_root}/qpdf-source"
fixture_home="${fixture_root}/home"
fake_bin="${fixture_root}/bin"
state="${fixture_root}/state"
log="${fixture_root}/contract.log"
build_path_file="${state}/build-path"
status_count_file="${state}/status-count"
victim="${fixture_root}/victim"

cleanup() { rm -rf -- "${fixture_root}"; }
trap cleanup EXIT

mkdir -p "${fixture_repo}/scripts" "${fixture_repo}/tests/oracle" \
  "${fixture_source}/include/qpdf" "${fixture_source}/libqpdf" \
  "${fixture_home}/.cache" "${fake_bin}" "${state}" "${victim}"
chmod 700 "${fixture_home}" "${fixture_home}/.cache"
printf 'do not touch\n' >"${victim}/sentinel"

# These copies intentionally make the first invocation RED until the runner and
# probe exist. The fixture exercises only the published runner boundary.
cp "${repo_root}/scripts/qpdf-stream-codecs-diff.sh" \
  "${fixture_repo}/scripts/qpdf-stream-codecs-diff.sh"
cp "${repo_root}/tests/oracle/qpdf_stream_codecs_probe.cc" \
  "${fixture_repo}/tests/oracle/qpdf_stream_codecs_probe.cc"

real_git="$(command -v git)"
real_mktemp="$(command -v mktemp)"
"${real_git}" -C "${fixture_source}" init -q
"${real_git}" -C "${fixture_source}" config user.email contract@example.invalid
"${real_git}" -C "${fixture_source}" config user.name contract
printf 'clean\n' >"${fixture_source}/sentinel"
"${real_git}" -C "${fixture_source}" add sentinel
"${real_git}" -C "${fixture_source}" commit -qm fixture
fixture_commit="$("${real_git}" -C "${fixture_source}" rev-parse HEAD)"
sed -i "s/qpdf_commit=\"[0-9a-f]*\"/qpdf_commit=\"${fixture_commit}\"/" \
  "${fixture_repo}/scripts/qpdf-stream-codecs-diff.sh"
cp "${fixture_repo}/scripts/qpdf-stream-codecs-diff.sh" "${state}/runner"

cat >"${fixture_repo}/scripts/fetch-qpdf-source.sh" <<EOF
#!/usr/bin/env bash
printf '%s\\n' '${fixture_source}'
EOF
chmod +x "${fixture_repo}/scripts/fetch-qpdf-source.sh" \
  "${fixture_repo}/scripts/qpdf-stream-codecs-diff.sh"

cat >"${fake_bin}/git" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == -C && "${3:-}" == status ]]; then
  count=0
  [[ -f "${STATUS_COUNT_FILE}" ]] && count="$(<"${STATUS_COUNT_FILE}")"
  count=$((count + 1))
  printf '%s\n' "${count}" >"${STATUS_COUNT_FILE}"
  [[ "${GIT_STATUS_FAIL_CALL:-}" == "${count}" ]] && exit 86
fi
exec "${REAL_GIT}" "$@"
EOF

cat >"${fake_bin}/mktemp" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
created="$("${REAL_MKTEMP}" "$@")"
printf '%s\n' "${created}" >"${BUILD_PATH_FILE}"
if [[ "${SWAP_MKTEMP_LEAF:-0}" == 1 ]]; then
  rmdir -- "${created}"
  ln -s -- "${FIXTURE_VICTIM}" "${created}"
fi
printf '%s\n' "${created}"
EOF

cat >"${fake_bin}/c++" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'c++' >>"${CONTRACT_LOG}"; printf ' <%s>' "$@" >>"${CONTRACT_LOG}"; printf '\n' >>"${CONTRACT_LOG}"
required=(
  "-std=c++17"
  "-DQPDF_DISABLE_QTC"
  "-I${FIXTURE_SOURCE}/include"
  "-I${FIXTURE_SOURCE}/libqpdf"
  "${FIXTURE_REPO}/tests/oracle/qpdf_stream_codecs_probe.cc"
  "${FIXTURE_SOURCE}/libqpdf/Pipeline.cc"
  "${FIXTURE_SOURCE}/libqpdf/Pl_ASCII85Decoder.cc"
  "${FIXTURE_SOURCE}/libqpdf/Pl_ASCIIHexDecoder.cc"
  "${FIXTURE_SOURCE}/libqpdf/Pl_RunLength.cc"
)
for expected in "${required[@]}"; do
  found=0
  for actual in "$@"; do [[ "${actual}" == "${expected}" ]] && found=1; done
  [[ "${found}" == 1 ]] || { echo "fake c++: missing ${expected}" >&2; exit 98; }
done
if [[ "${FAIL_CXX:-0}" == 1 ]]; then exit 97; fi
if [[ "${DIRTY_STAGE:-}" == cxx ]]; then printf 'dirty\n' >>"${FIXTURE_SOURCE}/sentinel"; fi
output=
while (($#)); do
  [[ "$1" == -o ]] && { output="$2"; break; }
  shift
done
[[ -n "${output}" ]]
printf '#!/usr/bin/env bash\nexit 0\n' >"${output}"
chmod +x "${output}"
EOF

cat >"${fake_bin}/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'cargo' >>"${CONTRACT_LOG}"; printf ' <%s>' "$@" >>"${CONTRACT_LOG}"; printf '\n' >>"${CONTRACT_LOG}"
if [[ ! -x "${QPDF_STREAM_CODECS_PROBE:-}" ]]; then
  echo "fake cargo: QPDF_STREAM_CODECS_PROBE is missing or not executable: ${QPDF_STREAM_CODECS_PROBE:-}" >&2
  exit 98
fi
expected=(test -p flpdf --lib pipeline::stream_codecs_oracle::qpdf_stream_codecs_differential -- --ignored --exact)
[[ "$#" == "${#expected[@]}" ]] || { echo "fake cargo: wrong count $#" >&2; exit 98; }
[[ "$*" == 'test -p flpdf --lib pipeline::stream_codecs_oracle::qpdf_stream_codecs_differential -- --ignored --exact' ]] || { printf 'fake cargo: unexpected args <%s>\n' "$*" >&2; exit 98; }
if [[ "${DIRTY_STAGE:-}" == cargo ]]; then printf 'dirty\n' >>"${FIXTURE_SOURCE}/sentinel"; fi
EOF
chmod +x "${fake_bin}/git" "${fake_bin}/mktemp" "${fake_bin}/c++" "${fake_bin}/cargo"

reset_fixture() {
  "${real_git}" -C "${fixture_source}" reset --hard -q "${fixture_commit}"
  "${real_git}" -C "${fixture_source}" clean -fdq
  cp "${state}/runner" "${fixture_repo}/scripts/qpdf-stream-codecs-diff.sh"
  chmod +x "${fixture_repo}/scripts/qpdf-stream-codecs-diff.sh"
  : >"${log}"
  rm -f -- "${build_path_file}" "${status_count_file}"
}

run_fixture() {
  local temp_root="$1"
  env PATH="${fake_bin}:${PATH}" HOME="${fixture_home}" XDG_CACHE_HOME="${fixture_home}/.cache" \
    TMPDIR="${temp_root}" CONTRACT_LOG="${log}" BUILD_PATH_FILE="${build_path_file}" \
    STATUS_COUNT_FILE="${status_count_file}" REAL_GIT="${real_git}" REAL_MKTEMP="${real_mktemp}" \
    FIXTURE_REPO="${fixture_repo}" FIXTURE_SOURCE="${fixture_source}" FIXTURE_VICTIM="${victim}" \
    SWAP_MKTEMP_LEAF="${SWAP_MKTEMP_LEAF:-0}" FAIL_CXX="${FAIL_CXX:-0}" \
    DIRTY_STAGE="${DIRTY_STAGE:-}" GIT_STATUS_FAIL_CALL="${GIT_STATUS_FAIL_CALL:-}" \
    "${fixture_repo}/scripts/qpdf-stream-codecs-diff.sh"
}

assert_victim_untouched() {
  [[ "$(<"${victim}/sentinel")" == 'do not touch' ]] || { echo 'victim changed' >&2; exit 1; }
}
assert_cleaned_build() {
  [[ -f "${build_path_file}" ]] || return 0
  local path
  path="$(<"${build_path_file}")"
  [[ ! -e "${path}" && ! -L "${path}" ]] || { echo "build directory remained: ${path}" >&2; exit 1; }
}
assert_fails_and_cleans() {
  local label="$1" temp_root="$2"; shift 2
  local status=0
  "$@" "${temp_root}" >"${fixture_root}/${label}.out" 2>&1 || status=$?
  [[ "${status}" != 0 ]] || { echo "${label} unexpectedly succeeded" >&2; exit 1; }
  assert_cleaned_build
  assert_victim_untouched
}

safe_tmp="${fixture_root}/safe-tmp"
mkdir -m 700 "${safe_tmp}"
reset_fixture
if ! run_fixture "${safe_tmp}"; then
  cat "${log}" >&2
  exit 1
fi
assert_cleaned_build
assert_victim_untouched
grep -Fx 'cargo <test> <-p> <flpdf> <--lib> <pipeline::stream_codecs_oracle::qpdf_stream_codecs_differential> <--> <--ignored> <--exact>' "${log}"

# Every failure case below starts from a clean pinned fixture, preserves the
# victim, and proves that a validated temporary build directory is removed.
reset_fixture
"${real_git}" -C "${fixture_source}" commit --allow-empty -qm wrong-head
assert_fails_and_cleans wrong-head "${safe_tmp}" run_fixture

reset_fixture
printf 'dirty\n' >>"${fixture_source}/sentinel"
assert_fails_and_cleans dirty-before "${safe_tmp}" run_fixture

reset_fixture
FAIL_CXX=1 assert_fails_and_cleans compiler-failure "${safe_tmp}" run_fixture

reset_fixture
DIRTY_STAGE=cxx assert_fails_and_cleans dirty-after-compile "${safe_tmp}" run_fixture
grep -q '^c++' "${log}" && ! grep -q '^cargo' "${log}"

reset_fixture
DIRTY_STAGE=cargo assert_fails_and_cleans dirty-after-cargo "${safe_tmp}" run_fixture
grep -q '^cargo' "${log}"

reset_fixture
GIT_STATUS_FAIL_CALL=1 assert_fails_and_cleans status-before-compile "${safe_tmp}" run_fixture
[[ ! -s "${log}" ]]

reset_fixture
GIT_STATUS_FAIL_CALL=3 assert_fails_and_cleans status-after-cargo "${safe_tmp}" run_fixture
grep -q '^cargo' "${log}"

# A repository-local TMPDIR must not be selected; the private external cache
# fallback is both used and cleaned instead.
inside_repo="${fixture_repo}/inside-repo-tmp"
mkdir -m 700 "${inside_repo}"
reset_fixture
inside_repo_output="${fixture_root}/inside-repo.out"
if ! run_fixture "${inside_repo}" >"${inside_repo_output}" 2>&1; then
  echo 'repository-local TMPDIR fallback unexpectedly failed' >&2
  exit 1
fi
grep -F 'qpdf-stream-codecs-diff.sh: unsafe' "${inside_repo_output}"
grep -F 'using an external fallback' "${inside_repo_output}"
build_path="$(<"${build_path_file}")"
case "${build_path}" in "${fixture_repo}"/*) echo 'used repository TMPDIR' >&2; exit 1;; esac
assert_cleaned_build
assert_victim_untouched

reset_fixture
swap_status=0
SWAP_MKTEMP_LEAF=1 run_fixture "${safe_tmp}" >"${fixture_root}/swapped-mktemp-leaf.out" 2>&1 || swap_status=$?
[[ "${swap_status}" != 0 ]] || { echo 'swapped mktemp leaf unexpectedly succeeded' >&2; exit 1; }
assert_victim_untouched
[[ -L "$(<"${build_path_file}")" ]] || { echo 'swapped mktemp leaf was not preserved for refusal' >&2; exit 1; }
[[ ! -s "${log}" ]]

printf 'qpdf-stream-codecs-diff contract: PASS\n'
