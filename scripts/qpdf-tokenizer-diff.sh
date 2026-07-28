#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
qpdf_source="$(
  cd "$("${repo_root}/scripts/fetch-qpdf-source.sh" --print-path)"
  pwd -P
)"
qpdf_commit="3b97c9bd266b7c32ea36d3536e22dab77412886d"
legacy_build_leaf="flpdf-qpdf-tokenizer-probe-11.9.0"
cache_parent_leaf="flpdf-qpdf-tokenizer-probe-cache-${UID}"
build_leaf="qpdf-11.9.0"
root_anchor_device=
root_anchor_owner=
source_was_clean=0

path_is_within() {
  local child="$1"
  local parent="$2"
  [[ "${child}" == "${parent}" || "${child}" == "${parent}/"* ]]
}

check_source_clean() {
  local dirty_message="${1:-probe build modified pinned source files}"
  local source_status
  if ! source_status="$(
    git -C "${qpdf_source}" status --porcelain --untracked-files=no
  )"; then
    echo "qpdf-tokenizer-diff.sh: unable to verify pinned source cleanliness" >&2
    return 1
  fi
  if [[ -n "${source_status}" ]]; then
    echo "qpdf-tokenizer-diff.sh: ${dirty_message}" >&2
    return 1
  fi
}

check_source_on_exit() {
  local status=$?
  trap - EXIT
  if [[ "${source_was_clean}" == 1 ]] && ! check_source_clean; then
    status=1
  fi
  exit "${status}"
}

unsafe_cache_artifact() {
  echo "qpdf-tokenizer-diff.sh: unsafe cache artifact: $*" >&2
  return 1
}

verify_private_directory() {
  local directory="$1"
  local label="$2"
  local owner
  local mode

  if [[ -L "${directory}" || ! -d "${directory}" ]]; then
    unsafe_cache_artifact "${label} is a symlink or not a directory: ${directory}"
    return 1
  fi
  if ! owner="$(stat -c '%u' -- "${directory}")" ||
    ! mode="$(stat -c '%a' -- "${directory}")"; then
    unsafe_cache_artifact "cannot inspect ${label}: ${directory}"
    return 1
  fi
  if [[ "${owner}" != "${UID}" ]]; then
    unsafe_cache_artifact "${label} is not owned by uid ${UID}: ${directory}"
    return 1
  fi
  if [[ "${mode}" != 700 ]]; then
    unsafe_cache_artifact "${label} mode is ${mode}, expected 700: ${directory}"
    return 1
  fi
}

create_private_directory() {
  local directory="$1"
  local label="$2"

  if [[ ! -e "${directory}" && ! -L "${directory}" ]]; then
    if ! (umask 077 && mkdir -- "${directory}"); then
      if [[ ! -e "${directory}" && ! -L "${directory}" ]]; then
        unsafe_cache_artifact "cannot create ${label}: ${directory}"
        return 1
      fi
    fi
  fi
  verify_private_directory "${directory}" "${label}"
}

mode_grants_child_replacement() {
  local mode_value="$1"

  (( (mode_value & 0030) == 0030 || (mode_value & 0003) == 0003 ))
}

owner_is_trusted() {
  local owner="$1"
  local device="$2"

  [[ "${owner}" == 0 || "${owner}" == "${UID}" ||
    "${owner}" == "${root_anchor_owner}" &&
      "${device}" == "${root_anchor_device}" ]]
}

verify_temp_base_ancestry() {
  local child="$1"
  local child_device
  local child_owner
  local parent
  local parent_device
  local parent_mode
  local parent_mode_value
  local parent_owner

  read -r child_owner child_device < <(
    stat -c '%u %d' -- "${child}"
  ) || return 1
  while [[ "${child}" != / ]]; do
    parent="$(dirname -- "${child}")"
    read -r parent_owner parent_mode parent_device < <(
      stat -c '%u %a %d' -- "${parent}"
    ) || return 1
    parent_mode_value=$((8#${parent_mode}))

    # An untrusted owner can relax its directory later. A sticky directory
    # protects this path component only when both it and the child are owned
    # by numeric root, this uid, or the same-device owner of the / anchor.
    if ! owner_is_trusted "${parent_owner}" "${parent_device}"; then
      return 1
    fi
    if mode_grants_child_replacement "${parent_mode_value}" &&
      ! {
        (( (parent_mode_value & 01000) != 0 )) &&
          owner_is_trusted "${child_owner}" "${child_device}"
      }; then
      return 1
    fi

    child="${parent}"
    child_owner="${parent_owner}"
    child_device="${parent_device}"
  done
}

safe_temp_base_path() {
  local requested_root="$1"
  local device
  local logical_root
  local mode
  local mode_value
  local owner
  local physical_root

  if [[ -L "${requested_root}" || ! -d "${requested_root}" ]] ||
    ! logical_root="$(realpath -m -s -- "${requested_root}")" ||
    ! physical_root="$(cd "${requested_root}" 2>/dev/null && pwd -P)" ||
    [[ "${logical_root}" != "${physical_root}" ]] ||
    [[ ! -w "${physical_root}" || ! -x "${physical_root}" ]] ||
    ! read -r owner mode device < <(
      stat -c '%u %a %d' -- "${physical_root}"
    ); then
    return 1
  fi
  mode_value=$((8#${mode}))

  # Numeric uid 0 is root on every filesystem. A nonzero owner of / is trusted
  # only on that same device: this supports a user-namespace root anchor
  # without treating an unrelated root-squash/nfsnobody uid as root.
  # Processes running under this same uid are inside the script's trust
  # boundary and can intentionally replace its cache.
  if {
    [[ "${owner}" == 0 ||
      "${owner}" == "${root_anchor_owner}" &&
        "${device}" == "${root_anchor_device}" ]]
  } && (( (mode_value & 01000) != 0 )); then
    :
  elif [[ "${owner}" == "${UID}" ]]; then
    if mode_grants_child_replacement "${mode_value}"; then
      return 1
    fi
  else
    return 1
  fi

  verify_temp_base_ancestry "${physical_root}" || return 1
  printf '%s\n' "${physical_root}"
}

select_temp_base() {
  local requested_root="${TMPDIR:-/tmp}"
  local qpdf_cache_base
  local root
  local physical_root
  local -a roots

  qpdf_cache_base="$(dirname -- "$(dirname -- "${qpdf_source}")")"
  roots=("${requested_root}")
  if [[ -n "${XDG_CACHE_HOME:-}" ]]; then
    roots+=("${XDG_CACHE_HOME}")
  fi
  if [[ -n "${HOME:-}" ]]; then
    roots+=("${HOME}/.cache")
  fi
  roots+=("${qpdf_cache_base}" /tmp /var/tmp)

  for root in "${roots[@]}"; do
    if ! physical_root="$(safe_temp_base_path "${root}")"; then
      if [[ "${root}" == "${requested_root}" ]]; then
        echo \
          "qpdf-tokenizer-diff.sh: unsafe or unusable TMPDIR; using an external fallback" \
          >&2
      fi
      continue
    fi
    if path_is_within "${physical_root}" "${repo_root}" ||
      path_is_within "${physical_root}" "${qpdf_source}"; then
      if [[ "${root}" == "${requested_root}" ]]; then
        echo \
          "qpdf-tokenizer-diff.sh: unsafe TMPDIR build path; using an external fallback" \
          >&2
      fi
      continue
    fi
    printf '%s\n' "${physical_root}"
    return 0
  done

  echo \
    "qpdf-tokenizer-diff.sh: no usable temp base outside the repository and pinned source" \
    >&2
  return 1
}

for required_command in realpath flock ldd stat; do
  if ! command -v "${required_command}" >/dev/null; then
    echo "qpdf-tokenizer-diff.sh: ${required_command} is required" >&2
    exit 1
  fi
done
if ! read -r root_anchor_owner root_anchor_device < <(
  stat -c '%u %d' -- /
); then
  echo "qpdf-tokenizer-diff.sh: unable to identify the filesystem root anchor" >&2
  exit 1
fi

if [[ "$(git -C "${qpdf_source}" rev-parse HEAD)" != "${qpdf_commit}" ]]; then
  echo "qpdf-tokenizer-diff.sh: pinned source is not at ${qpdf_commit}" >&2
  exit 1
fi
if ! check_source_clean "pinned source has tracked-file changes"; then
  exit 1
fi
source_was_clean=1
trap check_source_on_exit EXIT

temp_base="$(select_temp_base)"
legacy_build_dir="${temp_base}/${legacy_build_leaf}"
legacy_lock_file="${legacy_build_dir}.lock"
if [[ -e "${legacy_build_dir}" || -L "${legacy_build_dir}" ||
  -e "${legacy_lock_file}" || -L "${legacy_lock_file}" ]]; then
  unsafe_cache_artifact \
    "legacy top-level cache requires explicit removal or migration: ${legacy_build_dir}"
  exit 1
fi

cache_parent="${temp_base}/${cache_parent_leaf}"
if ! create_private_directory "${cache_parent}" "cache parent"; then
  exit 1
fi
cache_parent="$(cd "${cache_parent}" && pwd -P)"
if path_is_within "${cache_parent}" "${repo_root}" ||
  path_is_within "${cache_parent}" "${qpdf_source}" ||
  [[ "${cache_parent}" != "${temp_base}/${cache_parent_leaf}" ]]; then
  unsafe_cache_artifact "cache parent escaped the selected temp base: ${cache_parent}"
  exit 1
fi

build_dir="${cache_parent}/${build_leaf}"
if ! create_private_directory "${build_dir}" "build directory"; then
  exit 1
fi
build_dir="$(cd "${build_dir}" && pwd -P)"
if ! path_is_within "${build_dir}" "${cache_parent}" ||
  path_is_within "${build_dir}" "${repo_root}" ||
  path_is_within "${build_dir}" "${qpdf_source}" ||
  [[ "${build_dir}" != "${cache_parent}/${build_leaf}" ]]; then
  unsafe_cache_artifact "build directory escaped the private cache parent: ${build_dir}"
  exit 1
fi

if ! exec {build_lock_fd}<"${build_dir}"; then
  unsafe_cache_artifact "cannot open build directory for locking: ${build_dir}"
  exit 1
fi
flock "${build_lock_fd}"
if ! verify_private_directory "${cache_parent}" "cache parent" ||
  ! verify_private_directory "${build_dir}" "build directory"; then
  exit 1
fi
locked_cache_parent="$(cd "${cache_parent}" && pwd -P)"
locked_build_dir="$(cd "${build_dir}" && pwd -P)"
if [[ "${locked_cache_parent}" != "${cache_parent}" ]] ||
  [[ "${locked_build_dir}" != "${build_dir}" ]] ||
  ! path_is_within "${locked_build_dir}" "${locked_cache_parent}" ||
  path_is_within "${locked_cache_parent}" "${repo_root}" ||
  path_is_within "${locked_cache_parent}" "${qpdf_source}"; then
  unsafe_cache_artifact "cache containment changed while acquiring the lock"
  exit 1
fi
locked_inode="$(stat -Lc '%d:%i' -- "/proc/${BASHPID}/fd/${build_lock_fd}")"
path_inode="$(stat -c '%d:%i' -- "${build_dir}")"
if [[ "${locked_inode}" != "${path_inode}" ]]; then
  unsafe_cache_artifact "build directory changed while acquiring its lock: ${build_dir}"
  exit 1
fi

probe_binary="${build_dir}/flpdf-qpdf-tokenizer-probe"

cmake -S "${qpdf_source}" -B "${build_dir}" \
  -DBUILD_STATIC_LIBS=OFF \
  -DBUILD_SHARED_LIBS=ON \
  -DREQUIRE_CRYPTO_NATIVE=OFF \
  -DCMAKE_BUILD_TYPE=Release
cmake --build "${build_dir}" --target libqpdf --parallel

c++ -std=c++17 \
  -DPOINTERHOLDER_TRANSITION=4 \
  -I"${qpdf_source}/include" \
  -I"${qpdf_source}/libqpdf" \
  "${repo_root}/tests/oracle/qpdf_tokenizer_probe.cc" \
  "${qpdf_source}/libqpdf/ContentNormalizer.cc" \
  "${qpdf_source}/libqpdf/ResourceFinder.cc" \
  -L"${build_dir}/libqpdf" \
  -Wl,--disable-new-dtags \
  "-Wl,-rpath,${build_dir}/libqpdf" \
  -lqpdf \
  -o "${probe_binary}"

check_source_clean

probe_lib_dir="$(cd "${build_dir}/libqpdf" && pwd -P)"
probe_library_path="${probe_lib_dir}${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
loader_output="$(LD_LIBRARY_PATH="${probe_library_path}" ldd "${probe_binary}")"
resolved_libqpdf="$(
  awk '$1 ~ /^libqpdf\.so/ && $2 == "=>" { print $3; exit }' <<<"${loader_output}"
)"
if [[ -z "${resolved_libqpdf}" || ! -e "${resolved_libqpdf}" ]]; then
  echo "qpdf-tokenizer-diff.sh: unable to resolve the probe libqpdf" >&2
  exit 1
fi
resolved_libqpdf="$(realpath -e -- "${resolved_libqpdf}")"
if ! path_is_within "${resolved_libqpdf}" "${probe_lib_dir}"; then
  echo \
    "qpdf-tokenizer-diff.sh: resolved libqpdf is outside the pinned build: ${resolved_libqpdf}" \
    >&2
  exit 1
fi

cd "${repo_root}"
LD_LIBRARY_PATH="${probe_library_path}" \
  QPDF_TOKENIZER_PROBE="${probe_binary}" \
  cargo test -p flpdf --lib \
  tokenizer::tests::qpdf_tokenizer_differential_all_modes \
  -- --ignored --exact
LD_LIBRARY_PATH="${probe_library_path}" \
  QPDF_TOKENIZER_PROBE="${probe_binary}" \
  cargo test -p flpdf --lib \
  pipeline::qpdf_tokenizer::tests::qpdf_token_filter_differential \
  -- --ignored --exact
LD_LIBRARY_PATH="${probe_library_path}" \
  QPDF_TOKENIZER_PROBE="${probe_binary}" \
  cargo test -p flpdf --lib \
  pipeline::qpdf_tokenizer::tests::qpdf_token_filter_lifecycle_differential \
  -- --ignored --exact
LD_LIBRARY_PATH="${probe_library_path}" \
  QPDF_TOKENIZER_PROBE="${probe_binary}" \
  cargo test -p flpdf --lib \
  content_normalizer::tests::qpdf_content_normalizer_differential \
  -- --ignored --exact
LD_LIBRARY_PATH="${probe_library_path}" \
  QPDF_TOKENIZER_PROBE="${probe_binary}" \
  cargo test -p flpdf --lib \
  resource_finder::tests::qpdf_resource_finder_differential \
  -- --ignored --exact
