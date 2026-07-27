#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd -P)"
fixture_root="$(mktemp -d)"

fixture_repo="${fixture_root}/repo"
fixture_source="${fixture_root}/qpdf-source"
fixture_home="${fixture_root}/home"
fake_bin="${fixture_root}/bin"
contract_log="${fixture_root}/contract.log"
contract_state="${fixture_root}/state"
fixture_slug="$(basename "${fixture_root}")"
fixture_legacy_build_leaf="flpdf-qpdf-tokenizer-probe-contract-${fixture_slug}"
fixture_private_parent_leaf="flpdf-qpdf-tokenizer-probe-cache-contract-${fixture_slug}"
fixture_private_build_leaf="qpdf-contract-${fixture_slug}"

cleanup() {
  rm -rf \
    "${fixture_root}" \
    "/tmp/${fixture_private_parent_leaf}" \
    "/var/tmp/${fixture_private_parent_leaf}"
}
trap cleanup EXIT
mkdir -p \
  "${fixture_repo}/scripts" \
  "${fixture_repo}/tests/oracle" \
  "${fixture_source}" \
  "${fixture_home}/.cache" \
  "${fake_bin}" \
  "${contract_state}"
chmod 700 "${fixture_home}" "${fixture_home}/.cache"

cp "${repo_root}/scripts/qpdf-tokenizer-diff.sh" \
  "${fixture_repo}/scripts/qpdf-tokenizer-diff.sh"
cp "${repo_root}/tests/oracle/qpdf_tokenizer_probe.cc" \
  "${fixture_repo}/tests/oracle/qpdf_tokenizer_probe.cc"
sed -i \
  "s/flpdf-qpdf-tokenizer-probe-11\\.9\\.0/${fixture_legacy_build_leaf}/g" \
  "${fixture_repo}/scripts/qpdf-tokenizer-diff.sh"
sed -i \
  "s/flpdf-qpdf-tokenizer-probe-cache-\\\${UID}/${fixture_private_parent_leaf}/g" \
  "${fixture_repo}/scripts/qpdf-tokenizer-diff.sh"
sed -i \
  "s/build_leaf=\"qpdf-11\\.9\\.0\"/build_leaf=\"${fixture_private_build_leaf}\"/" \
  "${fixture_repo}/scripts/qpdf-tokenizer-diff.sh"

git -C "${fixture_source}" init -q
git -C "${fixture_source}" config user.email contract@example.invalid
git -C "${fixture_source}" config user.name contract
printf 'clean\n' >"${fixture_source}/sentinel"
git -C "${fixture_source}" add sentinel
git -C "${fixture_source}" commit -qm fixture
fixture_commit="$(git -C "${fixture_source}" rev-parse HEAD)"
sed -i \
  "s/qpdf_commit=\"[0-9a-f]*\"/qpdf_commit=\"${fixture_commit}\"/" \
  "${fixture_repo}/scripts/qpdf-tokenizer-diff.sh"
root_fixture_driver="${fixture_repo}/scripts/qpdf-tokenizer-diff-root.sh"
cp "${fixture_repo}/scripts/qpdf-tokenizer-diff.sh" "${root_fixture_driver}"
sed -i "s/\"\\\${UID}\"/\"0\"/g" "${root_fixture_driver}"

cat >"${fixture_repo}/scripts/fetch-qpdf-source.sh" <<EOF
#!/usr/bin/env bash
printf '%s\\n' '${fixture_source}'
EOF
chmod +x \
  "${fixture_repo}/scripts/fetch-qpdf-source.sh" \
  "${fixture_repo}/scripts/qpdf-tokenizer-diff.sh" \
  "${root_fixture_driver}"

real_stat="$(command -v stat)"
cat >"${fake_bin}/stat" <<EOF
#!/usr/bin/env bash
set -euo pipefail
real_stat='${real_stat}'
format=
target=
args=("\$@")
for ((i = 0; i < \${#args[@]}; ++i)); do
  case "\${args[i]}" in
    -c)
      format="\${args[i + 1]}"
      ;;
    --)
      target="\${args[i + 1]}"
      ;;
  esac
done

owner=
device=
if [[ -n "\${FAKE_STAT_ALL_OWNER:-}" ]]; then
  owner="\${FAKE_STAT_ALL_OWNER}"
elif [[ "\${target}" == / && -n "\${FAKE_STAT_ROOT_OWNER:-}" ]]; then
  owner="\${FAKE_STAT_ROOT_OWNER}"
elif [[ -n "\${FAKE_STAT_OWNER_PATH:-}" &&
  "\${target}" == "\${FAKE_STAT_OWNER_PATH}" ]]; then
  owner="\${FAKE_STAT_OWNER_UID:?}"
fi
if [[ "\${target}" == / && -n "\${FAKE_STAT_ROOT_DEVICE:-}" ]]; then
  device="\${FAKE_STAT_ROOT_DEVICE}"
elif [[ -n "\${FAKE_STAT_DEVICE_PATH:-}" &&
  "\${target}" == "\${FAKE_STAT_DEVICE_PATH}" ]]; then
  device="\${FAKE_STAT_DEVICE:?}"
fi

case "\${format}" in
  %u)
    if [[ -n "\${owner}" ]]; then
      printf '%s\\n' "\${owner}"
      exit 0
    fi
    ;;
  "%u %d")
    if [[ -n "\${owner}" || -n "\${device}" ]]; then
      owner="\${owner:-\$("\${real_stat}" -c '%u' -- "\${target}")}"
      device="\${device:-\$("\${real_stat}" -c '%d' -- "\${target}")}"
      printf '%s %s\\n' "\${owner}" "\${device}"
      exit 0
    fi
    ;;
  "%u %a")
    if [[ -n "\${owner}" ]]; then
      mode="\$("\${real_stat}" -c '%a' -- "\${target}")"
      printf '%s %s\\n' "\${owner}" "\${mode}"
      exit 0
    fi
    ;;
  "%u %a %d")
    if [[ -n "\${owner}" || -n "\${device}" ]]; then
      owner="\${owner:-\$("\${real_stat}" -c '%u' -- "\${target}")}"
      mode="\$("\${real_stat}" -c '%a' -- "\${target}")"
      device="\${device:-\$("\${real_stat}" -c '%d' -- "\${target}")}"
      printf '%s %s %s\\n' "\${owner}" "\${mode}" "\${device}"
      exit 0
    fi
    ;;
esac
exec "\${real_stat}" "\$@"
EOF

cat >"${fake_bin}/cmake" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'cmake' >>"${CONTRACT_LOG}"
printf ' <%s>' "$@" >>"${CONTRACT_LOG}"
printf '\n' >>"${CONTRACT_LOG}"

if [[ "${1:-}" == "--build" ]]; then
  exit 0
fi

build_dir=
while (($#)); do
  if [[ "$1" == "-B" ]]; then
    build_dir="$2"
    break
  fi
  shift
done
[[ -n "${build_dir}" ]]

if [[ "${MUTATE_SOURCE:-0}" == 1 ]]; then
  printf 'mutated\n' >>"${FIXTURE_SOURCE}/sentinel"
  exit 91
fi

if ! mkdir "${CONTRACT_STATE}/configuring" 2>/dev/null; then
  echo "configure race detected" >&2
  exit 92
fi
sleep 0.25
rmdir "${CONTRACT_STATE}/configuring"
mkdir -p "${build_dir}/libqpdf"
printf 'fixture\n' >"${build_dir}/libqpdf/libqpdf.so.29"
EOF

cat >"${fake_bin}/c++" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'c++' >>"${CONTRACT_LOG}"
printf ' <%s>' "$@" >>"${CONTRACT_LOG}"
printf '\n' >>"${CONTRACT_LOG}"

output=
saw_old_dtags=0
saw_normalizer_include=0
saw_normalizer_source=0
while (($#)); do
  case "$1" in
    -o)
      output="$2"
      shift 2
      ;;
    "-I${FIXTURE_SOURCE}/libqpdf")
      saw_normalizer_include=1
      shift
      ;;
    "${FIXTURE_SOURCE}/libqpdf/ContentNormalizer.cc")
      saw_normalizer_source=1
      shift
      ;;
    -Wl,--disable-new-dtags)
      saw_old_dtags=1
      shift
      ;;
    *)
      shift
      ;;
  esac
done
[[ "${saw_old_dtags}" == 1 ]] || {
  echo "missing -Wl,--disable-new-dtags" >&2
  exit 93
}
[[ "${saw_normalizer_include}" == 1 ]] || {
  echo "missing private ContentNormalizer include path" >&2
  exit 95
}
[[ "${saw_normalizer_source}" == 1 ]] || {
  echo "missing private ContentNormalizer source" >&2
  exit 96
}
printf '#!/usr/bin/env bash\nexit 0\n' >"${output}"
chmod +x "${output}"
EOF

cat >"${fake_bin}/ldd" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'ldd' >>"${CONTRACT_LOG}"
printf ' <%s>' "$@" >>"${CONTRACT_LOG}"
printf '\n' >>"${CONTRACT_LOG}"
if [[ "${LDD_MODE:-pinned}" == hostile ]]; then
  printf '\tlibqpdf.so.29 => /lib/x86_64-linux-gnu/libqpdf.so.29 (0x1)\n'
else
  lib_dir="${LD_LIBRARY_PATH%%:*}"
  printf '\tlibqpdf.so.29 => %s/libqpdf.so.29 (0x1)\n' "${lib_dir}"
fi
EOF

cat >"${fake_bin}/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'cargo' >>"${CONTRACT_LOG}"
printf ' <%s>' "$@" >>"${CONTRACT_LOG}"
printf '\n' >>"${CONTRACT_LOG}"
if (($# != 8)) ||
  [[ "$1" != test || "$2" != -p || "$3" != flpdf || "$4" != --lib ||
    "$6" != -- || "$7" != --ignored || "$8" != --exact ]]; then
  echo "cargo did not select one exact ignored library test" >&2
  exit 94
fi
case "$5" in
  tokenizer::tests::qpdf_tokenizer_differential_all_modes | \
    content_normalizer::tests::qpdf_content_normalizer_differential)
    ;;
  *)
    echo "cargo selected an unexpected ignored library test" >&2
    exit 94
    ;;
esac
EOF
chmod +x \
  "${fake_bin}/stat" \
  "${fake_bin}/cmake" \
  "${fake_bin}/c++" \
  "${fake_bin}/ldd" \
  "${fake_bin}/cargo"

run_fixture() {
  env \
    PATH="${fake_bin}:${PATH}" \
    HOME="${fixture_home}" \
    XDG_CACHE_HOME="${fixture_home}/.cache" \
    CONTRACT_LOG="${contract_log}" \
    CONTRACT_STATE="${contract_state}" \
    FIXTURE_SOURCE="${fixture_source}" \
    FAKE_STAT_ALL_OWNER="${FAKE_STAT_ALL_OWNER:-}" \
    FAKE_STAT_ROOT_OWNER="${FAKE_STAT_ROOT_OWNER:-}" \
    FAKE_STAT_ROOT_DEVICE="${FAKE_STAT_ROOT_DEVICE:-}" \
    FAKE_STAT_OWNER_PATH="${FAKE_STAT_OWNER_PATH:-}" \
    FAKE_STAT_OWNER_UID="${FAKE_STAT_OWNER_UID:-}" \
    FAKE_STAT_DEVICE_PATH="${FAKE_STAT_DEVICE_PATH:-}" \
    FAKE_STAT_DEVICE="${FAKE_STAT_DEVICE:-}" \
    "${CONTRACT_DRIVER:-${fixture_repo}/scripts/qpdf-tokenizer-diff.sh}"
}

assert_driver_rejects_before_tools() {
  local output="$1"
  local status="$2"
  if [[ "${status}" == 0 ]]; then
    echo "unsafe cache artifact unexpectedly succeeded" >&2
    exit 1
  fi
  if [[ -s "${contract_log}" ]]; then
    echo "external build tool ran before unsafe cache rejection" >&2
    exit 1
  fi
  grep -F "unsafe cache artifact" "${output}"
}

snapshot_directory() {
  local directory="$1"
  local entry

  stat -c 'directory %d:%i:%u:%a' -- "${directory}"
  while IFS= read -r entry; do
    stat -c 'entry %n %F %d:%i:%u:%a:%s' -- "${entry}"
    if [[ -f "${entry}" && ! -L "${entry}" ]]; then
      cksum -- "${entry}"
    fi
  done < <(
    find "${directory}" -mindepth 1 -maxdepth 1 -print |
      LC_ALL=C sort
  )
}

lock_attack_tmp="${fixture_root}/lock-attack-tmp"
mkdir -m 700 "${lock_attack_tmp}"
printf 'protected victim\n' >"${fixture_root}/lock-victim.expected"
cp "${fixture_root}/lock-victim.expected" "${fixture_root}/lock-victim"
ln -s \
  "${fixture_root}/lock-victim" \
  "${lock_attack_tmp}/${fixture_legacy_build_leaf}.lock"
: >"${contract_log}"
lock_status=0
TMPDIR="${lock_attack_tmp}" \
  run_fixture >"${fixture_root}/lock-attack.out" 2>&1 ||
  lock_status=$?
if ! cmp -s "${fixture_root}/lock-victim.expected" "${fixture_root}/lock-victim"; then
  echo "lock symlink victim was modified" >&2
  exit 1
fi
assert_driver_rejects_before_tools "${fixture_root}/lock-attack.out" "${lock_status}"

legacy_leaf_tmp="${fixture_root}/legacy-leaf-tmp"
legacy_leaf_target="${fixture_root}/legacy-leaf-target"
mkdir -m 700 "${legacy_leaf_tmp}" "${legacy_leaf_target}"
printf 'unchanged target\n' >"${legacy_leaf_target}/sentinel"
ln -s \
  "${legacy_leaf_target}" \
  "${legacy_leaf_tmp}/${fixture_legacy_build_leaf}"
: >"${contract_log}"
legacy_leaf_status=0
TMPDIR="${legacy_leaf_tmp}" \
  run_fixture >"${fixture_root}/legacy-leaf.out" 2>&1 ||
  legacy_leaf_status=$?
if [[ -e "${legacy_leaf_target}/libqpdf" ]] ||
  [[ "$(find "${legacy_leaf_target}" -mindepth 1 -maxdepth 1 | wc -l)" != 1 ]] ||
  [[ "$(cat "${legacy_leaf_target}/sentinel")" != "unchanged target" ]]; then
  echo "build-leaf symlink target was modified" >&2
  exit 1
fi
assert_driver_rejects_before_tools "${fixture_root}/legacy-leaf.out" "${legacy_leaf_status}"

private_leaf_tmp="${fixture_root}/private-leaf-tmp"
private_leaf_target="${fixture_root}/private-leaf-target"
private_parent="${private_leaf_tmp}/${fixture_private_parent_leaf}"
mkdir -m 700 "${private_leaf_tmp}" "${private_leaf_target}"
mkdir -m 700 "${private_parent}"
printf 'unchanged private target\n' >"${private_leaf_target}/sentinel"
ln -s \
  "${private_leaf_target}" \
  "${private_parent}/${fixture_private_build_leaf}"
: >"${contract_log}"
private_leaf_status=0
TMPDIR="${private_leaf_tmp}" \
  run_fixture >"${fixture_root}/private-leaf.out" 2>&1 ||
  private_leaf_status=$?
if [[ -e "${private_leaf_target}/libqpdf" ]] ||
  [[ "$(find "${private_leaf_target}" -mindepth 1 -maxdepth 1 | wc -l)" != 1 ]] ||
  [[ "$(cat "${private_leaf_target}/sentinel")" != "unchanged private target" ]]; then
  echo "private build-leaf symlink target was modified" >&2
  exit 1
fi
assert_driver_rejects_before_tools "${fixture_root}/private-leaf.out" "${private_leaf_status}"

private_mode_tmp="${fixture_root}/private-mode-tmp"
mkdir -m 700 "${private_mode_tmp}"
mkdir -m 755 "${private_mode_tmp}/${fixture_private_parent_leaf}"
printf 'unchanged private mode\n' \
  >"${private_mode_tmp}/${fixture_private_parent_leaf}/sentinel"
snapshot_directory "${private_mode_tmp}/${fixture_private_parent_leaf}" \
  >"${fixture_root}/private-mode.before"
: >"${contract_log}"
private_mode_status=0
TMPDIR="${private_mode_tmp}" \
  run_fixture >"${fixture_root}/private-mode.out" 2>&1 ||
  private_mode_status=$?
assert_driver_rejects_before_tools "${fixture_root}/private-mode.out" "${private_mode_status}"
snapshot_directory "${private_mode_tmp}/${fixture_private_parent_leaf}" \
  >"${fixture_root}/private-mode.after"
if ! cmp -s \
  "${fixture_root}/private-mode.before" \
  "${fixture_root}/private-mode.after"; then
  echo "unsafe private parent metadata or contents changed" >&2
  exit 1
fi

build_mode_tmp="${fixture_root}/build-mode-tmp"
build_mode_parent="${build_mode_tmp}/${fixture_private_parent_leaf}"
mkdir -m 700 "${build_mode_tmp}" "${build_mode_parent}"
mkdir -m 755 "${build_mode_parent}/${fixture_private_build_leaf}"
printf 'unchanged build mode\n' \
  >"${build_mode_parent}/${fixture_private_build_leaf}/sentinel"
snapshot_directory "${build_mode_parent}" \
  >"${fixture_root}/build-mode-parent.before"
snapshot_directory "${build_mode_parent}/${fixture_private_build_leaf}" \
  >"${fixture_root}/build-mode.before"
: >"${contract_log}"
build_mode_status=0
TMPDIR="${build_mode_tmp}" \
  run_fixture >"${fixture_root}/build-mode.out" 2>&1 ||
  build_mode_status=$?
assert_driver_rejects_before_tools "${fixture_root}/build-mode.out" "${build_mode_status}"
snapshot_directory "${build_mode_parent}" \
  >"${fixture_root}/build-mode-parent.after"
snapshot_directory "${build_mode_parent}/${fixture_private_build_leaf}" \
  >"${fixture_root}/build-mode.after"
if ! cmp -s \
  "${fixture_root}/build-mode-parent.before" \
  "${fixture_root}/build-mode-parent.after" ||
  ! cmp -s \
    "${fixture_root}/build-mode.before" \
    "${fixture_root}/build-mode.after"; then
  echo "unsafe build directory metadata or contents changed" >&2
  exit 1
fi

overflow_sticky_tmp="${fixture_root}/overflow-sticky-tmp"
mkdir -m 1777 "${overflow_sticky_tmp}"
snapshot_directory "${overflow_sticky_tmp}" \
  >"${fixture_root}/overflow-sticky.before"
: >"${contract_log}"
FAKE_STAT_ROOT_OWNER=65534 \
  FAKE_STAT_OWNER_PATH="${overflow_sticky_tmp}" \
  FAKE_STAT_OWNER_UID=65534 \
  FAKE_STAT_DEVICE_PATH="${overflow_sticky_tmp}" \
  FAKE_STAT_DEVICE=999999 \
  TMPDIR="${overflow_sticky_tmp}" \
  run_fixture >"${fixture_root}/overflow-sticky.out" 2>&1
configured_build="$(
  sed -n 's/^cmake .* <-B> <\([^>]*\)>.*/\1/p' "${contract_log}" |
    head -n 1
)"
case "${configured_build}/" in
  "${overflow_sticky_tmp}/"*)
    echo "overflow-uid sticky TMPDIR was used for the build" >&2
    exit 1
    ;;
  "${fixture_home}/.cache/"*)
    ;;
  *)
    echo "safe current-owned fallback was not used: ${configured_build}" >&2
    exit 1
    ;;
esac
snapshot_directory "${overflow_sticky_tmp}" \
  >"${fixture_root}/overflow-sticky.after"
if ! cmp -s \
  "${fixture_root}/overflow-sticky.before" \
  "${fixture_root}/overflow-sticky.after"; then
  echo "overflow-uid sticky TMPDIR metadata or contents changed" >&2
  exit 1
fi

root_sticky_tmp="${fixture_root}/root-sticky-tmp"
mkdir -m 1777 "${root_sticky_tmp}"
: >"${contract_log}"
CONTRACT_DRIVER="${root_fixture_driver}" \
  FAKE_STAT_ALL_OWNER=0 \
  TMPDIR="${root_sticky_tmp}" \
  run_fixture >"${fixture_root}/root-sticky.out" 2>&1
configured_build="$(
  sed -n 's/^cmake .* <-B> <\([^>]*\)>.*/\1/p' "${contract_log}" |
    head -n 1
)"
case "${configured_build}/" in
  "${root_sticky_tmp}/"*)
    ;;
  *)
    echo "root-owned sticky TMPDIR was not used: ${configured_build}" >&2
    exit 1
    ;;
esac

assert_unsafe_tmp_falls_back() {
  local unsafe_tmp="$1"
  local configured_build

  : >"${contract_log}"
  TMPDIR="${unsafe_tmp}" run_fixture
  configured_build="$(
    sed -n 's/^cmake .* <-B> <\([^>]*\)>.*/\1/p' "${contract_log}" |
      head -n 1
  )"
  [[ -n "${configured_build}" ]]
  case "${configured_build}/" in
    "${fixture_repo}/"* | "${fixture_source}/"*)
      echo "unsafe TMPDIR was used for the build: ${configured_build}" >&2
      exit 1
      ;;
    "${fixture_home}/.cache/"*)
      ;;
    *)
      echo "safe fallback escaped the fixture cache: ${configured_build}" >&2
      exit 1
      ;;
  esac
  [[ ! -e "${unsafe_tmp}" ]]
}

assert_unsafe_tmp_falls_back "${fixture_repo}/nested-tmp"
assert_unsafe_tmp_falls_back "${fixture_source}/nested-tmp"

nonsticky_shared_tmp="${fixture_root}/nonsticky-shared-tmp"
mkdir -m 0777 "${nonsticky_shared_tmp}"
snapshot_directory "${nonsticky_shared_tmp}" \
  >"${fixture_root}/nonsticky-shared.before"
: >"${contract_log}"
TMPDIR="${nonsticky_shared_tmp}" \
  run_fixture >"${fixture_root}/nonsticky-shared.out" 2>&1
configured_build="$(
  sed -n 's/^cmake .* <-B> <\([^>]*\)>.*/\1/p' "${contract_log}" |
    head -n 1
)"
case "${configured_build}/" in
  "${nonsticky_shared_tmp}/"*)
    echo "nonsticky shared TMPDIR was used for the build" >&2
    exit 1
    ;;
  "${fixture_home}/.cache/"*)
    ;;
  *)
    echo "nonsticky TMPDIR fallback escaped the fixture cache: ${configured_build}" >&2
    exit 1
    ;;
esac
snapshot_directory "${nonsticky_shared_tmp}" \
  >"${fixture_root}/nonsticky-shared.after"
if ! cmp -s \
  "${fixture_root}/nonsticky-shared.before" \
  "${fixture_root}/nonsticky-shared.after"; then
  echo "nonsticky shared TMPDIR metadata or contents changed" >&2
  exit 1
fi

: >"${contract_log}"
if LDD_MODE=hostile run_fixture >"${fixture_root}/hostile.out" 2>&1; then
  echo "system libqpdf resolution unexpectedly succeeded" >&2
  exit 1
fi
grep -F "resolved libqpdf is outside the pinned build" "${fixture_root}/hostile.out"
if grep -q '^cargo' "${contract_log}"; then
  echo "cargo ran after hostile libqpdf resolution" >&2
  exit 1
fi

: >"${contract_log}"
parallel_tmp="${fixture_root}/parallel-tmp"
TMPDIR="${parallel_tmp}" run_fixture >"${fixture_root}/parallel-1.out" 2>&1 &
first_pid=$!
TMPDIR="${parallel_tmp}" run_fixture >"${fixture_root}/parallel-2.out" 2>&1 &
second_pid=$!
wait "${first_pid}"
wait "${second_pid}"
[[ "$(grep -c '^cargo' "${contract_log}")" == 4 ]]
[[ "$(
  grep -Fxc \
    'cargo <test> <-p> <flpdf> <--lib> <tokenizer::tests::qpdf_tokenizer_differential_all_modes> <--> <--ignored> <--exact>' \
    "${contract_log}"
)" == 2 ]]
[[ "$(
  grep -Fxc \
    'cargo <test> <-p> <flpdf> <--lib> <content_normalizer::tests::qpdf_content_normalizer_differential> <--> <--ignored> <--exact>' \
    "${contract_log}"
)" == 2 ]]

git -C "${fixture_source}" checkout -q -- sentinel
: >"${contract_log}"
if MUTATE_SOURCE=1 run_fixture >"${fixture_root}/mutation.out" 2>&1; then
  echo "source mutation during a failed build unexpectedly succeeded" >&2
  exit 1
fi
grep -F "probe build modified pinned source files" "${fixture_root}/mutation.out"

printf 'qpdf-tokenizer-diff contract: PASS\n'
