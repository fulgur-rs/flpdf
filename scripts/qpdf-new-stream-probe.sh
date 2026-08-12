#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" != 0 ]]; then
  echo "usage: qpdf-new-stream-probe.sh" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
qpdf_source="$("${repo_root}/scripts/fetch-qpdf-source.sh" --print-path)"
qpdf_source="$(cd "${qpdf_source}" && pwd -P)"
qpdf_commit="3b97c9bd266b7c32ea36d3536e22dab77412886d"

actual_commit="$(git -C "${qpdf_source}" rev-parse --verify HEAD)"
if [[ "${actual_commit}" != "${qpdf_commit}" ]]; then
  echo "qpdf-new-stream-probe.sh: pinned source commit mismatch" >&2
  exit 1
fi
if [[ -n "$(git -C "${qpdf_source}" status --porcelain --untracked-files=no)" ]]; then
  echo "qpdf-new-stream-probe.sh: pinned source has tracked-file changes" >&2
  exit 1
fi

build_dir="$(TMPDIR=/tmp mktemp -d -t flpdf-qpdf-new-stream-XXXXXXXX)"
build_dir="$(realpath -e -- "${build_dir}")"
case "${build_dir}" in
  /tmp/flpdf-qpdf-new-stream-*) ;;
  *)
    echo "qpdf-new-stream-probe.sh: unsafe build directory" >&2
    exit 1
    ;;
esac
cleanup() {
  case "${build_dir:-}" in
    /tmp/flpdf-qpdf-new-stream-*) rm -rf -- "${build_dir}" ;;
  esac
}
trap cleanup EXIT

cmake -S "${qpdf_source}" -B "${build_dir}" \
  -DBUILD_STATIC_LIBS=OFF \
  -DBUILD_SHARED_LIBS=ON \
  -DREQUIRE_CRYPTO_NATIVE=OFF \
  -DCMAKE_BUILD_TYPE=Release >/dev/null
cmake --build "${build_dir}" --target libqpdf --parallel >/dev/null

probe_binary="${build_dir}/qpdf-new-stream-probe"
c++ -std=c++17 \
  -DPOINTERHOLDER_TRANSITION=4 \
  -I"${qpdf_source}/include" \
  -I"${qpdf_source}/libqpdf" \
  "${repo_root}/tests/oracle/qpdf_new_stream_probe.cc" \
  -L"${build_dir}/libqpdf" \
  -Wl,--disable-new-dtags \
  "-Wl,-rpath,${build_dir}/libqpdf" \
  -lqpdf \
  -o "${probe_binary}"

probe_lib_dir="$(cd "${build_dir}/libqpdf" && pwd -P)"
probe_library_path="${probe_lib_dir}${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
loader_output="$(LD_LIBRARY_PATH="${probe_library_path}" ldd "${probe_binary}")"
resolved_libqpdf="$(awk '$1 ~ /^libqpdf\.so/ && $2 == "=>" { print $3; exit }' <<<"${loader_output}")"
resolved_libqpdf="$(realpath -e -- "${resolved_libqpdf}")"
case "${resolved_libqpdf}" in
  "${probe_lib_dir}"/*) ;;
  *)
    echo "qpdf-new-stream-probe.sh: probe resolved an unpinned libqpdf" >&2
    exit 1
    ;;
esac

LD_LIBRARY_PATH="${probe_library_path}" "${probe_binary}"

actual_commit="$(git -C "${qpdf_source}" rev-parse --verify HEAD)"
if [[ "${actual_commit}" != "${qpdf_commit}" || -n "$(git -C "${qpdf_source}" status --porcelain --untracked-files=no)" ]]; then
  echo "qpdf-new-stream-probe.sh: pinned source changed during probe" >&2
  exit 1
fi
