#!/usr/bin/env bash
set -euo pipefail

# Generate the qpdf 11.9.0 golden for the extra-header-text writer route.
#
# QPDFWriter::setExtraHeaderText (libqpdf/QPDFWriter.cc:269) has no QPDFJob or
# command-line binding, so this golden has to come from the C++ API rather than
# the qpdf CLI. The generator itself lives in
# tests/oracle/qpdf_extra_header_generate_api.cc; this script builds it against
# the pinned qpdf source tree (scripts/fetch-qpdf-source.sh) exactly the way
# scripts/generate-qpdf-api-golden.sh does.

if [[ "$#" != 3 ]]; then
    echo "usage: generate-qpdf-extra-header-golden.sh INPUT.pdf OUTPUT.pdf HEADER_TEXT" >&2
    exit 2
fi

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
input_pdf="$(realpath -e -- "$1")"
output_pdf="$2"
header_text="$3"
qpdf_source="$("${repo_root}/scripts/fetch-qpdf-source.sh" --print-path)"
qpdf_source="$(cd "${qpdf_source}" && pwd -P)"
qpdf_commit="3b97c9bd266b7c32ea36d3536e22dab77412886d"

check_source_state() {
    local actual_commit
    actual_commit="$(git -C "${qpdf_source}" rev-parse --verify HEAD)"
    if [[ "${actual_commit}" != "${qpdf_commit}" ]]; then
        echo "generate-qpdf-extra-header-golden.sh: pinned source commit mismatch" >&2
        return 1
    fi
    if [[ -n "$(git -C "${qpdf_source}" status --porcelain --untracked-files=no)" ]]; then
        echo "generate-qpdf-extra-header-golden.sh: pinned source has tracked-file changes" >&2
        return 1
    fi
}

check_source_state

build_dir="$(TMPDIR=/tmp mktemp -d -t flpdf-qpdf-extra-header-XXXXXXXX)"
build_dir="$(realpath -e -- "${build_dir}")"
case "${build_dir}" in
    /tmp/flpdf-qpdf-extra-header-*) ;;
    *)
        echo "generate-qpdf-extra-header-golden.sh: unsafe build directory" >&2
        exit 1
        ;;
esac
cleanup() {
    case "${build_dir:-}" in
        /tmp/flpdf-qpdf-extra-header-*) rm -rf -- "${build_dir}" ;;
    esac
}
trap cleanup EXIT

cmake -S "${qpdf_source}" -B "${build_dir}" \
    -DBUILD_STATIC_LIBS=OFF \
    -DBUILD_SHARED_LIBS=ON \
    -DREQUIRE_CRYPTO_NATIVE=OFF \
    -DCMAKE_BUILD_TYPE=Release >/dev/null
cmake --build "${build_dir}" --target libqpdf --parallel >/dev/null

generator="${build_dir}/qpdf-extra-header-generate-api"
c++ -std=c++17 \
    -DPOINTERHOLDER_TRANSITION=4 \
    -I"${qpdf_source}/include" \
    -I"${qpdf_source}/libqpdf" \
    "${repo_root}/tests/oracle/qpdf_extra_header_generate_api.cc" \
    -L"${build_dir}/libqpdf" \
    -Wl,--disable-new-dtags \
    "-Wl,-rpath,${build_dir}/libqpdf" \
    -lqpdf \
    -o "${generator}"

check_source_state
generator_lib_dir="$(cd "${build_dir}/libqpdf" && pwd -P)"
generator_library_path="${generator_lib_dir}${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
loader_output="$(LD_LIBRARY_PATH="${generator_library_path}" ldd "${generator}")"
resolved_libqpdf="$(awk '$1 ~ /^libqpdf\.so/ && $2 == "=>" { print $3; exit }' <<<"${loader_output}")"
if [[ -z "${resolved_libqpdf}" ]]; then
    echo "generate-qpdf-extra-header-golden.sh: generator did not resolve libqpdf" >&2
    exit 1
fi
resolved_libqpdf="$(realpath -e -- "${resolved_libqpdf}")"
case "${resolved_libqpdf}" in
    "${generator_lib_dir}"/*) ;;
    *)
        echo "generate-qpdf-extra-header-golden.sh: generator resolved an unpinned libqpdf" >&2
        exit 1
        ;;
esac

raw_output="${build_dir}/extra-header-generate.pdf"
LD_LIBRARY_PATH="${generator_library_path}" \
    "${generator}" "${input_pdf}" "${raw_output}" "${header_text}"

# Self-check: with an empty header the same API call must reproduce the qpdf
# CLI output for this option set, which proves the generator's writer
# configuration matches `qpdf --qdf --object-streams=generate --static-id`.
control_api="${build_dir}/control-api.pdf"
control_cli="${build_dir}/control-cli.pdf"
LD_LIBRARY_PATH="${generator_library_path}" \
    "${generator}" "${input_pdf}" "${control_api}" ""
qpdf --qdf --object-streams=generate --static-id --warning-exit-0 \
    "${input_pdf}" "${control_cli}"
if ! cmp -s "${control_api}" "${control_cli}"; then
    echo "generate-qpdf-extra-header-golden.sh: API control output differs from the CLI" >&2
    exit 1
fi

mv -f -- "${raw_output}" "${output_pdf}"
check_source_state
