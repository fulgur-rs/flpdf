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
probe_path_file="${contract_state}/probe-path"
git_status_count_file="${contract_state}/git-status-count"
runner_template="${contract_state}/qpdf-json-pipeline-diff.sh"

cleanup() {
  rm -rf -- "${fixture_root}"
}
trap cleanup EXIT

mkdir -p \
  "${fixture_repo}/scripts" \
  "${fixture_repo}/tests/oracle" \
  "${fixture_source}/include/qpdf" \
  "${fixture_source}/libqpdf" \
  "${fixture_home}/.cache" \
  "${fake_bin}" \
  "${contract_state}"
chmod 700 "${fixture_home}" "${fixture_home}/.cache"

cp "${repo_root}/scripts/qpdf-json-pipeline-diff.sh" \
  "${fixture_repo}/scripts/qpdf-json-pipeline-diff.sh"
cp "${repo_root}/tests/oracle/qpdf_json_pipeline_probe.cc" \
  "${fixture_repo}/tests/oracle/qpdf_json_pipeline_probe.cc"

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
  "${fixture_repo}/scripts/qpdf-json-pipeline-diff.sh"
cp "${fixture_repo}/scripts/qpdf-json-pipeline-diff.sh" "${runner_template}"

cat >"${fixture_repo}/scripts/fetch-qpdf-source.sh" <<EOF
#!/usr/bin/env bash
printf '%s\n' '${fixture_source}'
EOF
chmod +x \
  "${fixture_repo}/scripts/fetch-qpdf-source.sh" \
  "${fixture_repo}/scripts/qpdf-json-pipeline-diff.sh"

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
if [[ -n "${MKTEMP_MODE:-}" ]]; then
  chmod "${MKTEMP_MODE}" -- "${created}"
fi
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

require_argument() {
  local expected="$1"
  shift
  local argument
  for argument in "$@"; do
    if [[ "${argument}" == "${expected}" ]]; then
      return 0
    fi
  done
  echo "fake c++: missing required argument: ${expected}" >&2
  exit 98
}

require_argument "-I${FIXTURE_SOURCE}/libqpdf" "$@"
require_argument "-I${FIXTURE_SOURCE}/include" "$@"
require_argument \
  "${FIXTURE_REPO}/tests/oracle/qpdf_json_pipeline_probe.cc" \
  "$@"
require_argument "${FIXTURE_SOURCE}/libqpdf/Pipeline.cc" "$@"
require_argument "${FIXTURE_SOURCE}/libqpdf/Pl_String.cc" "$@"
require_argument "${FIXTURE_SOURCE}/libqpdf/Pl_Concatenate.cc" "$@"
require_argument "${FIXTURE_SOURCE}/libqpdf/Pl_Base64.cc" "$@"
require_argument "${FIXTURE_SOURCE}/libqpdf/Pl_OStream.cc" "$@"

if [[ "${FAIL_CXX:-0}" == 1 ]]; then
  exit 97
fi

if [[ "${MUTATE_HEAD_STAGE:-}" == cxx ]]; then
  printf 'cxx head mutation\n' >>"${FIXTURE_SOURCE}/sentinel"
  "${REAL_GIT}" -C "${FIXTURE_SOURCE}" add sentinel
  "${REAL_GIT}" -C "${FIXTURE_SOURCE}" commit -qm cxx-head-mutation
fi
if [[ "${DIRTY_SOURCE_STAGE:-}" == cxx ]]; then
  printf 'cxx dirty mutation\n' >>"${FIXTURE_SOURCE}/sentinel"
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
printf '%s\n' "${output}" >"${PROBE_PATH_FILE}"

cat >"${output}" <<'PROBE'
#!/usr/bin/env bash
set -euo pipefail

printf 'probe' >>"${CONTRACT_LOG}"
printf ' <%s>' "$@" >>"${CONTRACT_LOG}"
printf '\n' >>"${CONTRACT_LOG}"

if [[ "${FAIL_PROBE:-0}" == 1 ]]; then
  exit 96
fi
if (($# != 1)) || [[ "$1" != core ]]; then
  echo "fake probe: expected core selector" >&2
  exit 98
fi
printf 'string-null\tok\t6162\t1\t0\n'
PROBE
chmod +x "${output}"

if [[ "${SWAP_BUILD_STAGE:-}" == cxx ]]; then
  build_path="$(cat "${BUILD_PATH_FILE}")"
  mv -- "${build_path}" "${build_path}.moved"
  mkdir -m 700 -- "${build_path}"
  printf 'protected runtime replacement\n' >"${build_path}/sentinel"
fi
EOF

cat >"${fake_bin}/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf 'cargo' >>"${CONTRACT_LOG}"
printf ' <%s>' "$@" >>"${CONTRACT_LOG}"
printf '\n' >>"${CONTRACT_LOG}"

expected_probe="$(cat "${PROBE_PATH_FILE}")"
if [[ "${QPDF_JSON_PIPELINE_PROBE:-}" != "${expected_probe}" ||
  ! -x "${QPDF_JSON_PIPELINE_PROBE:-}" ]]; then
  echo "fake cargo: QPDF_JSON_PIPELINE_PROBE must name the executable probe" >&2
  exit 98
fi
if ! grep -qx 'probe <core>' "${CONTRACT_LOG}"; then
  echo "fake cargo: probe core validation must run before cargo" >&2
  exit 98
fi
if (($# != 9)) ||
  [[ "$1" != test ||
    "$2" != -p ||
    "$3" != flpdf ||
    "$4" != --test ||
    "$5" != pipeline_public_api ||
    "$6" != live_qpdf_core_records_match_rust ||
    "$7" != -- ||
    "$8" != --ignored ||
    "$9" != --exact ]]; then
  echo "fake cargo: unexpected differential selector arguments" >&2
  exit 98
fi

if [[ "${FAIL_CARGO:-0}" == 1 ]]; then
  exit 95
fi
if [[ "${MUTATE_HEAD_STAGE:-}" == cargo ]]; then
  printf 'cargo head mutation\n' >>"${FIXTURE_SOURCE}/sentinel"
  "${REAL_GIT}" -C "${FIXTURE_SOURCE}" add sentinel
  "${REAL_GIT}" -C "${FIXTURE_SOURCE}" commit -qm cargo-head-mutation
fi
if [[ "${DIRTY_SOURCE_STAGE:-}" == cargo ]]; then
  printf 'cargo dirty mutation\n' >>"${FIXTURE_SOURCE}/sentinel"
fi
EOF

chmod +x \
  "${fake_bin}/git" \
  "${fake_bin}/mktemp" \
  "${fake_bin}/c++" \
  "${fake_bin}/cargo"

reset_fixture() {
  "${real_git}" -C "${fixture_source}" checkout -q "${fixture_commit}"
  "${real_git}" -C "${fixture_source}" restore \
    --source "${fixture_commit}" --staged --worktree -- sentinel
  cp "${runner_template}" \
    "${fixture_repo}/scripts/qpdf-json-pipeline-diff.sh"
  : >"${contract_log}"
  rm -f -- \
    "${build_path_file}" \
    "${probe_path_file}" \
    "${git_status_count_file}"
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
    PROBE_PATH_FILE="${probe_path_file}" \
    GIT_STATUS_COUNT_FILE="${git_status_count_file}" \
    REAL_GIT="${real_git}" \
    REAL_MKTEMP="${real_mktemp}" \
    FIXTURE_REPO="${fixture_repo}" \
    FIXTURE_SOURCE="${fixture_source}" \
    FIXTURE_VICTIM="${FIXTURE_VICTIM:-}" \
    MKTEMP_MODE="${MKTEMP_MODE:-}" \
    SWAP_MKTEMP_LEAF="${SWAP_MKTEMP_LEAF:-0}" \
    SWAP_BUILD_STAGE="${SWAP_BUILD_STAGE:-}" \
    FAIL_CXX="${FAIL_CXX:-0}" \
    FAIL_PROBE="${FAIL_PROBE:-0}" \
    FAIL_CARGO="${FAIL_CARGO:-0}" \
    GIT_STATUS_FAIL_CALL="${GIT_STATUS_FAIL_CALL:-}" \
    MUTATE_HEAD_STAGE="${MUTATE_HEAD_STAGE:-}" \
    DIRTY_SOURCE_STAGE="${DIRTY_SOURCE_STAGE:-}" \
    "${fixture_repo}/scripts/qpdf-json-pipeline-diff.sh"
}

assert_failed() {
  local status="$1"
  local scenario="$2"

  if [[ "${status}" == 0 ]]; then
    echo \
      "qpdf-json-pipeline-diff contract: ${scenario} unexpectedly succeeded" \
      >&2
    exit 1
  fi
}

assert_build_removed() {
  local scenario="$1"
  local build_path

  build_path="$(cat "${build_path_file}")"
  if [[ -e "${build_path}" || -L "${build_path}" ]]; then
    echo \
      "qpdf-json-pipeline-diff contract: ${scenario} build output was not removed" \
      >&2
    exit 1
  fi
}

assert_runner_mutation_rejected() {
  local scenario="$1"
  local expression="$2"
  local mutation_tmp="${fixture_root}/mutation-${scenario}"
  local mutation_status=0

  mkdir -m 700 "${mutation_tmp}"
  reset_fixture
  sed -i "${expression}" \
    "${fixture_repo}/scripts/qpdf-json-pipeline-diff.sh"
  run_fixture "${mutation_tmp}" >"${mutation_tmp}/output" 2>&1 ||
    mutation_status=$?
  assert_failed "${mutation_status}" "runner mutation ${scenario}"
}

success_tmp="${fixture_root}/success-tmp"
mkdir -m 700 "${success_tmp}"
reset_fixture
run_fixture "${success_tmp}" >"${fixture_root}/success.out" 2>&1
if [[ "$(cut -d' ' -f1 "${contract_log}")" != $'c++\nprobe\ncargo' ]]; then
  echo "qpdf-json-pipeline-diff contract: success tool order is wrong" >&2
  exit 1
fi
assert_build_removed "successful"

wrong_commit_tmp="${fixture_root}/wrong-commit-tmp"
mkdir -m 700 "${wrong_commit_tmp}"
reset_fixture
sed -i \
  "s/qpdf_commit=\"[0-9a-f]*\"/qpdf_commit=\"0000000000000000000000000000000000000000\"/" \
  "${fixture_repo}/scripts/qpdf-json-pipeline-diff.sh"
wrong_commit_status=0
run_fixture "${wrong_commit_tmp}" >"${fixture_root}/wrong-commit.out" 2>&1 ||
  wrong_commit_status=$?
assert_failed "${wrong_commit_status}" "wrong qpdf commit"
if [[ -s "${contract_log}" ]]; then
  echo "qpdf-json-pipeline-diff contract: tools ran for wrong qpdf commit" >&2
  exit 1
fi
assert_build_removed "wrong-commit"

dirty_before_tmp="${fixture_root}/dirty-before-tmp"
mkdir -m 700 "${dirty_before_tmp}"
reset_fixture
printf 'dirty before\n' >>"${fixture_source}/sentinel"
dirty_before_status=0
run_fixture "${dirty_before_tmp}" >"${fixture_root}/dirty-before.out" 2>&1 ||
  dirty_before_status=$?
assert_failed "${dirty_before_status}" "dirty source before compilation"
if [[ -s "${contract_log}" ]]; then
  echo "qpdf-json-pipeline-diff contract: tools ran for dirty source" >&2
  exit 1
fi
assert_build_removed "dirty-before"

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
  echo \
    "qpdf-json-pipeline-diff contract: fixture victim was deleted or modified" \
    >&2
  exit 1
fi
if [[ -s "${contract_log}" ]]; then
  echo \
    "qpdf-json-pipeline-diff contract: compiler ran after swapped-leaf rejection" \
    >&2
  exit 1
fi

for invalid_mode in 755 770; do
  mode_tmp="${fixture_root}/mode-${invalid_mode}-tmp"
  mkdir -m 700 "${mode_tmp}"
  reset_fixture
  mode_status=0
  MKTEMP_MODE="${invalid_mode}" \
    run_fixture "${mode_tmp}" >"${fixture_root}/mode-${invalid_mode}.out" 2>&1 ||
    mode_status=$?
  assert_failed "${mode_status}" "mktemp leaf mode ${invalid_mode}"
  if [[ -s "${contract_log}" ]]; then
    echo \
      "qpdf-json-pipeline-diff contract: compiler ran for mode ${invalid_mode} leaf" \
      >&2
    exit 1
  fi
done

runtime_swap_tmp="${fixture_root}/runtime-swap-tmp"
mkdir -m 700 "${runtime_swap_tmp}"
reset_fixture
runtime_swap_status=0
SWAP_BUILD_STAGE=cxx \
  run_fixture "${runtime_swap_tmp}" >"${fixture_root}/runtime-swap.out" 2>&1 ||
  runtime_swap_status=$?
assert_failed "${runtime_swap_status}" "post-compile build leaf swap"
runtime_swap_build="$(cat "${build_path_file}")"
runtime_swap_sentinel="$(cat "${runtime_swap_build}/sentinel" 2>/dev/null || true)"
if [[ ! -d "${runtime_swap_build}" || -L "${runtime_swap_build}" ||
  "${runtime_swap_sentinel}" != "protected runtime replacement" ]]; then
  echo \
    "qpdf-json-pipeline-diff contract: runtime replacement was deleted or modified" \
    >&2
  exit 1
fi
if ! grep -q '^c++' "${contract_log}" ||
  grep -Eq '^(probe|cargo)' "${contract_log}"; then
  echo \
    "qpdf-json-pipeline-diff contract: runtime leaf swap crossed a tool boundary" \
    >&2
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
      "qpdf-json-pipeline-diff contract: unsafe shared TMPDIR was used: ${configured_build}" \
      >&2
    exit 1
    ;;
esac
assert_build_removed "fallback"

assert_runner_mutation_rejected \
  "missing-libqpdf-include" \
  '\|-I"${qpdf_source}/libqpdf"|d'
assert_runner_mutation_rejected \
  "missing-public-include" \
  '\|-I"${qpdf_source}/include"|d'
assert_runner_mutation_rejected \
  "missing-probe-source" \
  '\|qpdf_json_pipeline_probe\.cc|d'
assert_runner_mutation_rejected \
  "missing-pipeline-source" \
  '\|libqpdf/Pipeline\.cc|d'
assert_runner_mutation_rejected \
  "missing-string-source" \
  '\|libqpdf/Pl_String\.cc|d'
assert_runner_mutation_rejected \
  "missing-concatenate-source" \
  '\|libqpdf/Pl_Concatenate\.cc|d'
assert_runner_mutation_rejected \
  "missing-base64-source" \
  '\|libqpdf/Pl_Base64\.cc|d'
assert_runner_mutation_rejected \
  "missing-ostream-source" \
  '\|libqpdf/Pl_OStream\.cc|d'
assert_runner_mutation_rejected \
  "missing-probe-execution" \
  '\|core >/dev/null|d'
assert_runner_mutation_rejected \
  "missing-probe-env" \
  '\|QPDF_JSON_PIPELINE_PROBE=|d'
assert_runner_mutation_rejected \
  "missing-exact-selector" \
  's|-- --ignored --exact|-- --ignored|'
assert_runner_mutation_rejected \
  "wrong-selector-scope" \
  's|live_qpdf_core_records_match_rust|pipeline_public_api::live_qpdf_core_records_match_rust|'

source_fail_tmp="${fixture_root}/source-fail-tmp"
mkdir -m 700 "${source_fail_tmp}"
reset_fixture
source_fail_status=0
GIT_STATUS_FAIL_CALL=1 \
  run_fixture "${source_fail_tmp}" >"${fixture_root}/source-fail.out" 2>&1 ||
  source_fail_status=$?
assert_failed "${source_fail_status}" "initial source status failure"
if [[ -s "${contract_log}" ]]; then
  echo \
    "qpdf-json-pipeline-diff contract: tools ran after initial source check failed" \
    >&2
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
  grep -Eq '^(probe|cargo)' "${contract_log}"; then
  echo \
    "qpdf-json-pipeline-diff contract: post-compile failure crossed a tool boundary" \
    >&2
  exit 1
fi

dirty_after_compile_tmp="${fixture_root}/dirty-after-compile-tmp"
mkdir -m 700 "${dirty_after_compile_tmp}"
reset_fixture
dirty_after_compile_status=0
DIRTY_SOURCE_STAGE=cxx \
  run_fixture "${dirty_after_compile_tmp}" \
  >"${fixture_root}/dirty-after-compile.out" 2>&1 ||
  dirty_after_compile_status=$?
assert_failed "${dirty_after_compile_status}" "dirty source after compilation"
if grep -Eq '^(probe|cargo)' "${contract_log}"; then
  echo \
    "qpdf-json-pipeline-diff contract: dirty post-compile source crossed a tool boundary" \
    >&2
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
if grep -Eq '^(probe|cargo)' "${contract_log}"; then
  echo \
    "qpdf-json-pipeline-diff contract: post-compile HEAD mutation crossed a tool boundary" \
    >&2
  exit 1
fi

compiler_fail_tmp="${fixture_root}/compiler-fail-tmp"
mkdir -m 700 "${compiler_fail_tmp}"
reset_fixture
compiler_fail_status=0
FAIL_CXX=1 \
  run_fixture "${compiler_fail_tmp}" >"${fixture_root}/compiler-fail.out" 2>&1 ||
  compiler_fail_status=$?
assert_failed "${compiler_fail_status}" "compiler failure"
assert_build_removed "compiler-failure"

probe_fail_tmp="${fixture_root}/probe-fail-tmp"
mkdir -m 700 "${probe_fail_tmp}"
reset_fixture
probe_fail_status=0
FAIL_PROBE=1 \
  run_fixture "${probe_fail_tmp}" >"${fixture_root}/probe-fail.out" 2>&1 ||
  probe_fail_status=$?
assert_failed "${probe_fail_status}" "probe failure"
if grep -q '^cargo' "${contract_log}"; then
  echo "qpdf-json-pipeline-diff contract: cargo ran after probe failure" >&2
  exit 1
fi
assert_build_removed "probe-failure"

cargo_fail_tmp="${fixture_root}/cargo-fail-tmp"
mkdir -m 700 "${cargo_fail_tmp}"
reset_fixture
cargo_fail_status=0
FAIL_CARGO=1 \
  run_fixture "${cargo_fail_tmp}" >"${fixture_root}/cargo-fail.out" 2>&1 ||
  cargo_fail_status=$?
assert_failed "${cargo_fail_status}" "cargo failure"
assert_build_removed "cargo-failure"

head_at_exit_tmp="${fixture_root}/head-at-exit-tmp"
mkdir -m 700 "${head_at_exit_tmp}"
reset_fixture
head_at_exit_status=0
MUTATE_HEAD_STAGE=cargo \
  run_fixture "${head_at_exit_tmp}" >"${fixture_root}/head-at-exit.out" 2>&1 ||
  head_at_exit_status=$?
assert_failed "${head_at_exit_status}" "EXIT-time HEAD mutation"
if ! grep -q '^cargo' "${contract_log}"; then
  echo "qpdf-json-pipeline-diff contract: EXIT scenario did not reach cargo" >&2
  exit 1
fi

dirty_at_exit_tmp="${fixture_root}/dirty-at-exit-tmp"
mkdir -m 700 "${dirty_at_exit_tmp}"
reset_fixture
dirty_at_exit_status=0
DIRTY_SOURCE_STAGE=cargo \
  run_fixture "${dirty_at_exit_tmp}" >"${fixture_root}/dirty-at-exit.out" 2>&1 ||
  dirty_at_exit_status=$?
assert_failed "${dirty_at_exit_status}" "EXIT-time dirty source"
if ! grep -q '^cargo' "${contract_log}"; then
  echo \
    "qpdf-json-pipeline-diff contract: dirty EXIT scenario did not reach cargo" \
    >&2
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
  echo \
    "qpdf-json-pipeline-diff contract: EXIT status scenario did not reach cargo" \
    >&2
  exit 1
fi

printf 'qpdf-json-pipeline-diff contract: PASS\n'
