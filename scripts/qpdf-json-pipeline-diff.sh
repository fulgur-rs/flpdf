#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
qpdf_source="$(
  cd "$("${repo_root}/scripts/fetch-qpdf-source.sh" --print-path)"
  pwd -P
)"
qpdf_commit="3b97c9bd266b7c32ea36d3536e22dab77412886d"
build_dir=
build_dir_fd=
build_dir_fd_path=
build_dir_inode=
root_anchor_device=
root_anchor_owner=
runner_pid="${BASHPID}"
temp_base=

path_is_within() {
  local child="$1"
  local parent="$2"
  [[ "${child}" == "${parent}" || "${child}" == "${parent}/"* ]]
}

check_source_state() {
  local dirty_message="${1:-probe modified pinned tracked source}"
  local actual_commit
  local source_status

  if ! actual_commit="$(git -C "${qpdf_source}" rev-parse --verify HEAD)"; then
    echo "qpdf-json-pipeline-diff.sh: unable to verify pinned source HEAD" >&2
    return 1
  fi
  if [[ "${actual_commit}" != "${qpdf_commit}" ]]; then
    echo "qpdf-json-pipeline-diff.sh: pinned source is not at ${qpdf_commit}" >&2
    return 1
  fi
  if ! source_status="$(
    git -C "${qpdf_source}" status --porcelain --untracked-files=no
  )"; then
    echo "qpdf-json-pipeline-diff.sh: unable to verify pinned source cleanliness" >&2
    return 1
  fi
  if [[ -n "${source_status}" ]]; then
    echo "qpdf-json-pipeline-diff.sh: ${dirty_message}" >&2
    return 1
  fi
}

unsafe_temp_artifact() {
  echo "qpdf-json-pipeline-diff.sh: unsafe temp artifact: $*" >&2
  return 1
}

verify_private_directory() {
  local directory="$1"
  local label="$2"
  local mode
  local owner

  if [[ -L "${directory}" || ! -d "${directory}" ]]; then
    unsafe_temp_artifact "${label} is a symlink or not a directory: ${directory}"
    return 1
  fi
  if ! read -r owner mode < <(stat -c '%u %a' -- "${directory}"); then
    unsafe_temp_artifact "cannot inspect ${label}: ${directory}"
    return 1
  fi
  if [[ "${owner}" != "${UID}" ]]; then
    unsafe_temp_artifact "${label} is not owned by uid ${UID}: ${directory}"
    return 1
  fi
  if [[ "${mode}" != 700 ]]; then
    unsafe_temp_artifact "${label} mode is ${mode}, expected 700: ${directory}"
    return 1
  fi
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
          "qpdf-json-pipeline-diff.sh: unsafe or unusable TMPDIR; using an external fallback" \
          >&2
      fi
      continue
    fi
    if path_is_within "${physical_root}" "${repo_root}" ||
      path_is_within "${physical_root}" "${qpdf_source}"; then
      if [[ "${root}" == "${requested_root}" ]]; then
        echo \
          "qpdf-json-pipeline-diff.sh: unsafe TMPDIR build path; using an external fallback" \
          >&2
      fi
      continue
    fi
    printf '%s\n' "${physical_root}"
    return 0
  done

  echo \
    "qpdf-json-pipeline-diff.sh: no usable temp base outside the repository and pinned source" \
    >&2
  return 1
}

verify_build_directory_path() {
  local logical_build
  local physical_build

  if [[ -z "${build_dir}" ||
    "$(dirname -- "${build_dir}")" != "${temp_base}" ||
    "$(basename -- "${build_dir}")" != flpdf-qpdf-json-pipeline.* ]]; then
    unsafe_temp_artifact "build directory escaped its lexical template"
    return 1
  fi
  if ! verify_private_directory "${build_dir}" "build directory"; then
    return 1
  fi
  if ! logical_build="$(realpath -m -s -- "${build_dir}")" ||
    ! physical_build="$(cd "${build_dir}" 2>/dev/null && pwd -P)" ||
    [[ "${logical_build}" != "${build_dir}" ]] ||
    [[ "${physical_build}" != "${build_dir}" ]] ||
    path_is_within "${build_dir}" "${repo_root}" ||
    path_is_within "${build_dir}" "${qpdf_source}"; then
    unsafe_temp_artifact "build directory is not a private external lexical path"
    return 1
  fi
}

open_build_directory() {
  local path_inode

  if ! exec {build_dir_fd}<"${build_dir}"; then
    unsafe_temp_artifact "cannot open build directory: ${build_dir}"
    return 1
  fi
  build_dir_fd_path="/proc/${runner_pid}/fd/${build_dir_fd}"
  if ! build_dir_inode="$(stat -Lc '%d:%i' -- "${build_dir_fd_path}")" ||
    ! path_inode="$(stat -c '%d:%i' -- "${build_dir}")" ||
    [[ "${build_dir_inode}" != "${path_inode}" ]]; then
    unsafe_temp_artifact "build directory changed while opening it: ${build_dir}"
    return 1
  fi
}

verify_build_directory_identity() {
  local fd_inode
  local path_inode

  if [[ -z "${build_dir_inode}" || -z "${build_dir_fd_path}" ]] ||
    ! verify_build_directory_path ||
    ! fd_inode="$(stat -Lc '%d:%i' -- "${build_dir_fd_path}")" ||
    ! path_inode="$(stat -c '%d:%i' -- "${build_dir}")" ||
    [[ "${fd_inode}" != "${build_dir_inode}" ]] ||
    [[ "${path_inode}" != "${build_dir_inode}" ]]; then
    unsafe_temp_artifact "build directory identity changed: ${build_dir}"
    return 1
  fi
}

cleanup() {
  local status=$?
  trap - EXIT

  if [[ -n "${build_dir}" ]]; then
    if [[ -n "${build_dir_inode}" ]] &&
      verify_build_directory_identity; then
      if ! rm -rf -- "${build_dir}"; then
        echo \
          "qpdf-json-pipeline-diff.sh: unable to remove validated build directory" \
          >&2
        status=1
      fi
    else
      echo \
        "qpdf-json-pipeline-diff.sh: refusing to clean an unvalidated lexical build path" \
        >&2
      status=1
    fi
  fi
  if ! check_source_state; then
    status=1
  fi
  exit "${status}"
}

for required_command in git mktemp realpath stat; do
  if ! command -v "${required_command}" >/dev/null; then
    echo "qpdf-json-pipeline-diff.sh: ${required_command} is required" >&2
    exit 1
  fi
done
if ! read -r root_anchor_owner root_anchor_device < <(
  stat -c '%u %d' -- /
); then
  echo "qpdf-json-pipeline-diff.sh: unable to identify the filesystem root anchor" >&2
  exit 1
fi
trap cleanup EXIT

temp_base="$(select_temp_base)"
build_dir="$(mktemp -d "${temp_base}/flpdf-qpdf-json-pipeline.XXXXXXXX")"
verify_build_directory_path
open_build_directory
verify_build_directory_identity

if ! check_source_state "pinned source has tracked-file changes"; then
  exit 1
fi

probe="${build_dir_fd_path}/qpdf_json_pipeline_probe"
c++ -std=c++17 \
  -I"${qpdf_source}/libqpdf" \
  -I"${qpdf_source}/include" \
  "${repo_root}/tests/oracle/qpdf_json_pipeline_probe.cc" \
  "${qpdf_source}/libqpdf/Pipeline.cc" \
  "${qpdf_source}/libqpdf/Pl_String.cc" \
  "${qpdf_source}/libqpdf/Pl_Concatenate.cc" \
  "${qpdf_source}/libqpdf/Pl_Base64.cc" \
  "${qpdf_source}/libqpdf/Pl_OStream.cc" \
  -o "${probe}"

if ! check_source_state ||
  ! verify_build_directory_identity ||
  [[ -L "${probe}" || ! -f "${probe}" || ! -x "${probe}" ]] ||
  [[ "$(stat -c '%u' -- "${probe}")" != "${UID}" ]]; then
  echo \
    "qpdf-json-pipeline-diff.sh: compiled probe is not a trusted build artifact" \
    >&2
  exit 1
fi

"${probe}" core >/dev/null

cd "${repo_root}"
QPDF_JSON_PIPELINE_PROBE="${probe}" \
  cargo test -p flpdf --test pipeline_public_api \
  live_qpdf_core_records_match_rust -- --ignored --exact
