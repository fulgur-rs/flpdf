#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" != 2 ]]; then
    echo "usage: generate-qpdf-api-golden.sh INPUT.pdf OUTPUT.hex" >&2
    exit 2
fi

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
input_pdf="$(realpath -e -- "$1")"
output_hex="$2"
qpdf_source="$("${repo_root}/scripts/fetch-qpdf-source.sh" --print-path)"
qpdf_source="$(cd "${qpdf_source}" && pwd -P)"
qpdf_commit="3b97c9bd266b7c32ea36d3536e22dab77412886d"

check_source_state() {
    local actual_commit
    actual_commit="$(git -C "${qpdf_source}" rev-parse --verify HEAD)"
    if [[ "${actual_commit}" != "${qpdf_commit}" ]]; then
        echo "generate-qpdf-api-golden.sh: pinned source commit mismatch" >&2
        return 1
    fi
    if [[ -n "$(git -C "${qpdf_source}" status --porcelain --untracked-files=no)" ]]; then
        echo "generate-qpdf-api-golden.sh: pinned source has tracked-file changes" >&2
        return 1
    fi
}

check_source_state

build_dir="$(TMPDIR=/tmp mktemp -d -t flpdf-qpdf-api-golden-XXXXXXXX)"
build_dir="$(realpath -e -- "${build_dir}")"
case "${build_dir}" in
    /tmp/flpdf-qpdf-api-golden-*) ;;
    *)
        echo "generate-qpdf-api-golden.sh: unsafe build directory" >&2
        exit 1
        ;;
esac
cleanup() {
    case "${build_dir:-}" in
        /tmp/flpdf-qpdf-api-golden-*) rm -rf -- "${build_dir}" ;;
    esac
}
trap cleanup EXIT

cmake -S "${qpdf_source}" -B "${build_dir}" \
    -DBUILD_STATIC_LIBS=OFF \
    -DBUILD_SHARED_LIBS=ON \
    -DREQUIRE_CRYPTO_NATIVE=OFF \
    -DCMAKE_BUILD_TYPE=Release >/dev/null
cmake --build "${build_dir}" --target libqpdf --parallel >/dev/null

generator="${build_dir}/qpdf-removed-source-container-preserve-api"
c++ -std=c++17 \
    -DPOINTERHOLDER_TRANSITION=4 \
    -I"${qpdf_source}/include" \
    -I"${qpdf_source}/libqpdf" \
    "${repo_root}/tests/oracle/qpdf_removed_source_container_preserve_api.cc" \
    -L"${build_dir}/libqpdf" \
    -Wl,--disable-new-dtags \
    "-Wl,-rpath,${build_dir}/libqpdf" \
    -lqpdf \
    -o "${generator}"

check_source_state
probe_lib_dir="$(cd "${build_dir}/libqpdf" && pwd -P)"
probe_library_path="${probe_lib_dir}${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
loader_output="$(LD_LIBRARY_PATH="${probe_library_path}" ldd "${generator}")"
resolved_libqpdf="$(awk '$1 ~ /^libqpdf\.so/ && $2 == "=>" { print $3; exit }' <<<"${loader_output}")"
if [[ -z "${resolved_libqpdf}" ]]; then
    echo "generate-qpdf-api-golden.sh: generator did not resolve libqpdf" >&2
    exit 1
fi
resolved_libqpdf="$(realpath -e -- "${resolved_libqpdf}")"
case "${resolved_libqpdf}" in
    "${probe_lib_dir}"/*) ;;
    *)
        echo "generate-qpdf-api-golden.sh: generator resolved an unpinned libqpdf" >&2
        exit 1
        ;;
esac

raw_output="${build_dir}/removed-source-container-preserve-qpdf-api.pdf"
generated_hex="${build_dir}/removed-source-container-preserve-qpdf-api.pdf.hex"
LD_LIBRARY_PATH="${probe_library_path}" "${generator}" "${input_pdf}" "${raw_output}"

python3 - "${raw_output}" "${generated_hex}" <<'PY'
from pathlib import Path
import sys

encoded = Path(sys.argv[1]).read_bytes().hex()
Path(sys.argv[2]).write_text(
    "\n".join(encoded[index : index + 128] for index in range(0, len(encoded), 128))
    + "\n",
    encoding="ascii",
)
PY

mv -f -- "${generated_hex}" "${output_hex}"
check_source_state
