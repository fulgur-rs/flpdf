#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
qpdf_source="$(cd "$("${repo_root}/scripts/fetch-qpdf-source.sh" --print-path)" && pwd -P)"
qpdf_commit="3b97c9bd266b7c32ea36d3536e22dab77412886d"
build_dir=
build_dir_fd=
build_dir_fd_path=
build_dir_inode=
root_anchor_owner=
root_anchor_device=
runner_pid="${BASHPID}"
temp_base=

path_is_within() {
  local child="$1" parent="$2"
  [[ "${child}" == "${parent}" || "${child}" == "${parent}/"* ]]
}

check_source_state() {
  local dirty_message="${1:-probe modified pinned tracked source}"
  local actual_commit source_status
  if ! actual_commit="$(git -C "${qpdf_source}" rev-parse --verify HEAD)"; then
    echo "qpdf-stream-codecs-diff.sh: unable to verify pinned source HEAD" >&2
    return 1
  fi
  if [[ "${actual_commit}" != "${qpdf_commit}" ]]; then
    echo "qpdf-stream-codecs-diff.sh: pinned source is not at ${qpdf_commit}" >&2
    return 1
  fi
  if ! source_status="$(
    git -C "${qpdf_source}" status --porcelain --untracked-files=all --ignored
  )"; then
    echo "qpdf-stream-codecs-diff.sh: unable to verify pinned source cleanliness" >&2
    return 1
  fi
  if [[ -n "${source_status}" ]]; then
    echo "qpdf-stream-codecs-diff.sh: ${dirty_message}" >&2
    return 1
  fi
}

unsafe_temp_artifact() {
  echo "qpdf-stream-codecs-diff.sh: unsafe temp artifact: $*" >&2
  return 1
}

verify_private_directory() {
  local directory="$1" label="$2" owner mode
  if [[ -L "${directory}" || ! -d "${directory}" ]]; then
    unsafe_temp_artifact "${label} is a symlink or not a directory: ${directory}"
    return 1
  fi
  if ! read -r owner mode < <(stat -c '%u %a' -- "${directory}"); then
    unsafe_temp_artifact "cannot inspect ${label}: ${directory}"
    return 1
  fi
  if [[ "${owner}" != "${UID}" || "${mode}" != 700 ]]; then
    unsafe_temp_artifact "${label} must be owned by uid ${UID} with mode 700: ${directory}"
    return 1
  fi
}

mode_grants_child_replacement() {
  local mode_value="$1"
  (( (mode_value & 0030) == 0030 || (mode_value & 0003) == 0003 ))
}

owner_is_trusted() {
  local owner="$1" device="$2"
  [[ "${owner}" == 0 || "${owner}" == "${UID}" ||
    "${owner}" == "${root_anchor_owner}" && "${device}" == "${root_anchor_device}" ]]
}

verify_temp_base_ancestry() {
  local child="$1" child_owner child_device parent parent_owner parent_mode parent_device parent_mode_value
  read -r child_owner child_device < <(stat -c '%u %d' -- "${child}") || return 1
  while [[ "${child}" != / ]]; do
    parent="$(dirname -- "${child}")"
    read -r parent_owner parent_mode parent_device < <(stat -c '%u %a %d' -- "${parent}") || return 1
    parent_mode_value=$((8#${parent_mode}))
    if ! owner_is_trusted "${parent_owner}" "${parent_device}"; then
      return 1
    fi
    if mode_grants_child_replacement "${parent_mode_value}" && ! {
      (( (parent_mode_value & 01000) != 0 )) && owner_is_trusted "${child_owner}" "${child_device}"
    }; then
      return 1
    fi
    child="${parent}"; child_owner="${parent_owner}"; child_device="${parent_device}"
  done
}

safe_temp_base_path() {
  local requested_root="$1" logical_root physical_root owner mode device mode_value
  if [[ -L "${requested_root}" || ! -d "${requested_root}" ]] ||
    ! logical_root="$(realpath -m -s -- "${requested_root}")" ||
    ! physical_root="$(cd "${requested_root}" 2>/dev/null && pwd -P)" ||
    [[ "${logical_root}" != "${physical_root}" ]] ||
    [[ ! -w "${physical_root}" || ! -x "${physical_root}" ]] ||
    ! read -r owner mode device < <(stat -c '%u %a %d' -- "${physical_root}"); then
    return 1
  fi
  mode_value=$((8#${mode}))
  if {
    [[ "${owner}" == 0 || "${owner}" == "${root_anchor_owner}" && "${device}" == "${root_anchor_device}" ]]
  } && (( (mode_value & 01000) != 0 )); then
    :
  elif [[ "${owner}" == "${UID}" ]] && ! mode_grants_child_replacement "${mode_value}"; then
    :
  else
    return 1
  fi
  verify_temp_base_ancestry "${physical_root}" || return 1
  printf '%s\n' "${physical_root}"
}

select_temp_base() {
  local requested_root="${TMPDIR:-/tmp}" root physical_root qpdf_cache_base
  local -a roots
  qpdf_cache_base="$(dirname -- "$(dirname -- "${qpdf_source}")")"
  roots=("${requested_root}")
  [[ -n "${XDG_CACHE_HOME:-}" ]] && roots+=("${XDG_CACHE_HOME}")
  [[ -n "${HOME:-}" ]] && roots+=("${HOME}/.cache")
  roots+=("${qpdf_cache_base}" /tmp /var/tmp)
  for root in "${roots[@]}"; do
    if ! physical_root="$(safe_temp_base_path "${root}")"; then
      if [[ "${root}" == "${requested_root}" ]]; then
        echo "qpdf-stream-codecs-diff.sh: unsafe or unusable TMPDIR; using an external fallback" >&2
      fi
      continue
    fi
    if path_is_within "${physical_root}" "${repo_root}" || path_is_within "${physical_root}" "${qpdf_source}"; then
      if [[ "${root}" == "${requested_root}" ]]; then
        echo "qpdf-stream-codecs-diff.sh: unsafe TMPDIR build path; using an external fallback" >&2
      fi
      continue
    fi
    printf '%s\n' "${physical_root}"
    return 0
  done
  echo "qpdf-stream-codecs-diff.sh: no usable temp base outside the repository and pinned source" >&2
  return 1
}

verify_build_directory_path() {
  local logical_build physical_build
  if [[ -z "${build_dir}" || "$(dirname -- "${build_dir}")" != "${temp_base}" ||
    "$(basename -- "${build_dir}")" != flpdf-qpdf-stream-codecs.* ]]; then
    unsafe_temp_artifact "build directory escaped its lexical template"
    return 1
  fi
  verify_private_directory "${build_dir}" "build directory" || return 1
  if ! logical_build="$(realpath -m -s -- "${build_dir}")" ||
    ! physical_build="$(cd "${build_dir}" 2>/dev/null && pwd -P)" ||
    [[ "${logical_build}" != "${build_dir}" || "${physical_build}" != "${build_dir}" ]] ||
    path_is_within "${build_dir}" "${repo_root}" || path_is_within "${build_dir}" "${qpdf_source}"; then
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
  local fd_inode path_inode
  if [[ -z "${build_dir_inode}" || -z "${build_dir_fd_path}" ]] ||
    ! verify_build_directory_path ||
    ! fd_inode="$(stat -Lc '%d:%i' -- "${build_dir_fd_path}")" ||
    ! path_inode="$(stat -c '%d:%i' -- "${build_dir}")" ||
    [[ "${fd_inode}" != "${build_dir_inode}" || "${path_inode}" != "${build_dir_inode}" ]]; then
    unsafe_temp_artifact "build directory identity changed: ${build_dir}"
    return 1
  fi
}

cleanup() {
  local status=$?
  trap - EXIT
  if [[ -n "${build_dir}" ]]; then
    if [[ -n "${build_dir_inode}" ]] && verify_build_directory_identity; then
      rm -rf -- "${build_dir}" || status=1
    else
      echo "qpdf-stream-codecs-diff.sh: refusing to clean an unvalidated lexical build path" >&2
      status=1
    fi
  fi
  check_source_state || status=1
  exit "${status}"
}

for required_command in git mktemp realpath stat c++ cargo; do
  command -v "${required_command}" >/dev/null || {
    echo "qpdf-stream-codecs-diff.sh: ${required_command} is required" >&2; exit 1;
  }
done
if ! read -r root_anchor_owner root_anchor_device < <(stat -c '%u %d' -- /); then
  echo "qpdf-stream-codecs-diff.sh: unable to identify filesystem root anchor" >&2
  exit 1
fi
trap cleanup EXIT

temp_base="$(select_temp_base)"
build_dir="$(mktemp -d "${temp_base}/flpdf-qpdf-stream-codecs.XXXXXXXX")"
verify_build_directory_path
open_build_directory
verify_build_directory_identity
check_source_state "pinned source has tracked-file changes"

probe="${build_dir_fd_path}/qpdf_stream_codecs_probe"
c++ -std=c++17 -DQPDF_DISABLE_QTC \
  "-I${qpdf_source}/include" \
  "-I${qpdf_source}/libqpdf" \
  "${repo_root}/tests/oracle/qpdf_stream_codecs_probe.cc" \
  "${qpdf_source}/libqpdf/Pipeline.cc" \
  "${qpdf_source}/libqpdf/Pl_ASCII85Decoder.cc" \
  "${qpdf_source}/libqpdf/Pl_ASCIIHexDecoder.cc" \
  "${qpdf_source}/libqpdf/Pl_RunLength.cc" \
  -o "${probe}"

if ! check_source_state || ! verify_build_directory_identity ||
  [[ -L "${probe}" || ! -f "${probe}" || ! -x "${probe}" ]] ||
  [[ "$(stat -c '%u' -- "${probe}")" != "${UID}" ]]; then
  echo "qpdf-stream-codecs-diff.sh: compiled probe is not a trusted build artifact" >&2
  exit 1
fi

cd "${repo_root}"
QPDF_STREAM_CODECS_PROBE="${probe}" \
  cargo test -p flpdf --lib \
  pipeline::stream_codecs_oracle::qpdf_stream_codecs_differential \
  -- --ignored --exact
