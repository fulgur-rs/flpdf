#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd -P)"
fixture_root="$(mktemp -d)"
fixture_repo="${fixture_root}/repo"
fixture_source="${fixture_root}/qpdf-source"
fixture_home="${fixture_root}/home"
fake_bin="${fixture_root}/bin"
contract_state="${fixture_root}/state"
contract_log="${fixture_root}/contract.log"
build_path_file="${contract_state}/build-path"
git_status_count_file="${contract_state}/git-status-count"

cleanup() {
  rm -rf -- "${fixture_root}"
}
trap cleanup EXIT

mkdir -p \
  "${fixture_repo}/scripts" \
  "${fixture_repo}/tests/oracle/qpdf_pl_rc4_shim/qpdf" \
  "${fixture_source}/include/qpdf" \
  "${fixture_source}/libqpdf" \
  "${fixture_home}/.cache" \
  "${fake_bin}" \
  "${contract_state}"
chmod 700 "${fixture_home}" "${fixture_home}/.cache"

cp "${repo_root}/scripts/qpdf-rc4-diff.sh" \
  "${fixture_repo}/scripts/qpdf-rc4-diff.sh"
cp "${repo_root}/tests/oracle/qpdf_rc4_probe.cc" \
  "${fixture_repo}/tests/oracle/qpdf_rc4_probe.cc"
cp "${repo_root}/tests/oracle/qpdf_pl_rc4_shim/qpdf/RC4.hh" \
  "${fixture_repo}/tests/oracle/qpdf_pl_rc4_shim/qpdf/RC4.hh"

real_git="$(command -v git)"
real_mktemp="$(command -v mktemp)"

"${real_git}" -C "${fixture_source}" init -q
"${real_git}" -C "${fixture_source}" config user.email contract@example.invalid
"${real_git}" -C "${fixture_source}" config user.name contract
printf 'clean\n' >"${fixture_source}/sentinel"
"${real_git}" -C "${fixture_source}" add sentinel
"${real_git}" -C "${fixture_source}" commit -qm fixture
fixture_commit="$("${real_git}" -C "${fixture_source}" rev-parse HEAD)"
sed -i \
  "s/qpdf_commit=\"[0-9a-f]*\"/qpdf_commit=\"${fixture_commit}\"/" \
  "${fixture_repo}/scripts/qpdf-rc4-diff.sh"

cat >"${fixture_repo}/scripts/fetch-qpdf-source.sh" <<EOF
#!/usr/bin/env bash
printf '%s\n' '${fixture_source}'
EOF
chmod +x \
  "${fixture_repo}/scripts/fetch-qpdf-source.sh" \
  "${fixture_repo}/scripts/qpdf-rc4-diff.sh"

cat >"${fake_bin}/git" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == -C && "${3:-}" == status ]]; then
  count=0
  if [[ -f "${GIT_STATUS_COUNT_FILE}" ]]; then
    count="$(cat "${GIT_STATUS_COUNT_FILE}")"
  fi
  count=$((count + 1))
  printf '%s\n' "${count}" >"${GIT_STATUS_COUNT_FILE}"
  if [[ -n "${GIT_STATUS_FAIL_CALL:-}" &&
    "${count}" == "${GIT_STATUS_FAIL_CALL}" ]]; then
    exit 86
  fi
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

printf 'c++' >>"${CONTRACT_LOG}"
printf ' <%s>' "$@" >>"${CONTRACT_LOG}"
printf '\n' >>"${CONTRACT_LOG}"

if [[ "${FAIL_CXX:-0}" == 1 ]]; then
  exit 97
fi

if [[ "${MUTATE_HEAD_STAGE:-}" == cxx ]]; then
  printf 'cxx mutation\n' >>"${FIXTURE_SOURCE}/sentinel"
  "${REAL_GIT}" -C "${FIXTURE_SOURCE}" add sentinel
  "${REAL_GIT}" -C "${FIXTURE_SOURCE}" commit -qm cxx-mutation
fi

output=
while (($#)); do
  if [[ "$1" == -o ]]; then
    output="$2"
    break
  fi
  shift
done
[[ -n "${output}" ]]

cat >"${output}" <<'PROBE'
#!/usr/bin/env bash
set -euo pipefail

case "${1:-}|${2:-}|${3:-}|${4:-}|${5:-}|${6:-}" in
  "explicit|||0||")
    echo "qpdf_rc4_probe: empty explicit key" >&2
    exit 2
    ;;
  "cstr|||0||" | "cstr|00||0||")
    echo "qpdf_rc4_probe: empty C-string key" >&2
    exit 2
    ;;
  "explicit|0g||0||")
    echo "qpdf_rc4_probe: invalid hex" >&2
    exit 2
    ;;
  "explicit|00||0junk||")
    echo "qpdf_rc4_probe: invalid split" >&2
    exit 2
    ;;
  "pipeline|explicit||0|0|65536")
    echo "qpdf_rc4_probe: empty explicit key" >&2
    exit 2
    ;;
  "pipeline|cstr|00|0|0|65536")
    echo "qpdf_rc4_probe: empty C-string key" >&2
    exit 2
    ;;
  "pipeline|explicit|00|0junk|0|65536")
    echo "qpdf_rc4_probe: invalid input length" >&2
    exit 2
    ;;
  "pipeline|explicit|00|0|0junk|65536")
    echo "qpdf_rc4_probe: invalid write split" >&2
    exit 2
    ;;
  "pipeline|explicit|00|0|0|1junk")
    echo "qpdf_rc4_probe: invalid output buffer size" >&2
    exit 2
    ;;
  "pipeline|explicit|00|0|1|65536")
    echo "qpdf_rc4_probe: write split exceeds input" >&2
    exit 2
    ;;
  "pipeline|explicit|00|0|0|0")
    echo "qpdf_rc4_probe: zero output buffer size" >&2
    exit 2
    ;;
esac

printf 'one\t\nsplit\t\nin-place\t\n'
PROBE
chmod +x "${output}"
EOF

cat >"${fake_bin}/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf 'cargo' >>"${CONTRACT_LOG}"
printf ' <%s>' "$@" >>"${CONTRACT_LOG}"
printf '\n' >>"${CONTRACT_LOG}"

if [[ "${MUTATE_HEAD_STAGE:-}" == cargo ]]; then
  printf 'cargo mutation\n' >>"${FIXTURE_SOURCE}/sentinel"
  "${REAL_GIT}" -C "${FIXTURE_SOURCE}" add sentinel
  "${REAL_GIT}" -C "${FIXTURE_SOURCE}" commit -qm cargo-mutation
fi
EOF

chmod +x \
  "${fake_bin}/git" \
  "${fake_bin}/mktemp" \
  "${fake_bin}/c++" \
  "${fake_bin}/cargo"

reset_fixture() {
  "${real_git}" -C "${fixture_source}" checkout -q "${fixture_commit}"
  : >"${contract_log}"
  rm -f -- "${build_path_file}" "${git_status_count_file}"
}

run_fixture() {
  local temp_root="$1"

  env \
    PATH="${fake_bin}:${PATH}" \
    HOME="${fixture_home}" \
    XDG_CACHE_HOME="${fixture_home}/.cache" \
    TMPDIR="${temp_root}" \
    CONTRACT_LOG="${contract_log}" \
    BUILD_PATH_FILE="${build_path_file}" \
    GIT_STATUS_COUNT_FILE="${git_status_count_file}" \
    REAL_GIT="${real_git}" \
    REAL_MKTEMP="${real_mktemp}" \
    FIXTURE_SOURCE="${fixture_source}" \
    FIXTURE_VICTIM="${FIXTURE_VICTIM:-}" \
    SWAP_MKTEMP_LEAF="${SWAP_MKTEMP_LEAF:-0}" \
    FAIL_CXX="${FAIL_CXX:-0}" \
    GIT_STATUS_FAIL_CALL="${GIT_STATUS_FAIL_CALL:-}" \
    MUTATE_HEAD_STAGE="${MUTATE_HEAD_STAGE:-}" \
    "${fixture_repo}/scripts/qpdf-rc4-diff.sh"
}

assert_failed() {
  local status="$1"
  local scenario="$2"

  if [[ "${status}" == 0 ]]; then
    echo "qpdf-rc4-diff contract: ${scenario} unexpectedly succeeded" >&2
    exit 1
  fi
}

swap_tmp="${fixture_root}/swap-tmp"
swap_victim="${fixture_root}/swap-victim"
mkdir -m 700 "${swap_tmp}" "${swap_victim}"
printf 'protected fixture victim\n' >"${swap_victim}/sentinel"
reset_fixture
swap_status=0
FIXTURE_VICTIM="${swap_victim}" \
  SWAP_MKTEMP_LEAF=1 \
  FAIL_CXX=1 \
  run_fixture "${swap_tmp}" >"${fixture_root}/swap.out" 2>&1 ||
  swap_status=$?
assert_failed "${swap_status}" "symlink-swapped mktemp leaf"
swap_sentinel="$(cat "${swap_victim}/sentinel" 2>/dev/null || true)"
if [[ "${swap_sentinel}" != "protected fixture victim" ]]; then
  echo "qpdf-rc4-diff contract: fixture victim was deleted or modified" >&2
  exit 1
fi
if [[ -s "${contract_log}" ]]; then
  echo "qpdf-rc4-diff contract: compiler ran after swapped-leaf rejection" >&2
  exit 1
fi

nonsticky_tmp="${fixture_root}/nonsticky-shared-tmp"
mkdir "${nonsticky_tmp}"
chmod 0777 "${nonsticky_tmp}"
reset_fixture
run_fixture "${nonsticky_tmp}" >"${fixture_root}/nonsticky.out" 2>&1
configured_build="$(cat "${build_path_file}")"
case "${configured_build}/" in
  "${fixture_home}/.cache/"*)
    ;;
  *)
    echo \
      "qpdf-rc4-diff contract: unsafe shared TMPDIR was used: ${configured_build}" \
      >&2
    exit 1
    ;;
esac

source_fail_tmp="${fixture_root}/source-fail-tmp"
mkdir -m 700 "${source_fail_tmp}"
reset_fixture
source_fail_status=0
GIT_STATUS_FAIL_CALL=1 \
  run_fixture "${source_fail_tmp}" >"${fixture_root}/source-fail.out" 2>&1 ||
  source_fail_status=$?
assert_failed "${source_fail_status}" "initial source status failure"
if [[ -s "${contract_log}" ]]; then
  echo "qpdf-rc4-diff contract: tools ran after initial source check failed" >&2
  exit 1
fi

post_compile_tmp="${fixture_root}/post-compile-tmp"
mkdir -m 700 "${post_compile_tmp}"
reset_fixture
post_compile_status=0
GIT_STATUS_FAIL_CALL=2 \
  run_fixture "${post_compile_tmp}" >"${fixture_root}/post-compile.out" 2>&1 ||
  post_compile_status=$?
assert_failed "${post_compile_status}" "post-compile source status failure"
if ! grep -q '^c++' "${contract_log}" ||
  grep -q '^cargo' "${contract_log}"; then
  echo "qpdf-rc4-diff contract: post-compile failure crossed a tool boundary" >&2
  exit 1
fi

head_after_compile_tmp="${fixture_root}/head-after-compile-tmp"
mkdir -m 700 "${head_after_compile_tmp}"
reset_fixture
head_after_compile_status=0
MUTATE_HEAD_STAGE=cxx \
  run_fixture "${head_after_compile_tmp}" \
  >"${fixture_root}/head-after-compile.out" 2>&1 ||
  head_after_compile_status=$?
assert_failed "${head_after_compile_status}" "post-compile HEAD mutation"
if grep -q '^cargo' "${contract_log}"; then
  echo "qpdf-rc4-diff contract: cargo ran after post-compile HEAD mutation" >&2
  exit 1
fi

head_at_exit_tmp="${fixture_root}/head-at-exit-tmp"
mkdir -m 700 "${head_at_exit_tmp}"
reset_fixture
head_at_exit_status=0
MUTATE_HEAD_STAGE=cargo \
  run_fixture "${head_at_exit_tmp}" >"${fixture_root}/head-at-exit.out" 2>&1 ||
  head_at_exit_status=$?
assert_failed "${head_at_exit_status}" "EXIT-time HEAD mutation"
if ! grep -q '^cargo' "${contract_log}"; then
  echo "qpdf-rc4-diff contract: EXIT scenario did not reach cargo" >&2
  exit 1
fi

status_at_exit_tmp="${fixture_root}/status-at-exit-tmp"
mkdir -m 700 "${status_at_exit_tmp}"
reset_fixture
status_at_exit_status=0
GIT_STATUS_FAIL_CALL=3 \
  run_fixture "${status_at_exit_tmp}" >"${fixture_root}/status-at-exit.out" 2>&1 ||
  status_at_exit_status=$?
assert_failed "${status_at_exit_status}" "EXIT-time source status failure"
if ! grep -q '^cargo' "${contract_log}"; then
  echo "qpdf-rc4-diff contract: EXIT status scenario did not reach cargo" >&2
  exit 1
fi

printf 'qpdf-rc4-diff contract: PASS\n'
