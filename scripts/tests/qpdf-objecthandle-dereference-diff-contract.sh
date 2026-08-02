#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd -P)"
runner="${repo_root}/scripts/qpdf-objecthandle-dereference-diff.sh"
probe="${repo_root}/tests/oracle/qpdf_objecthandle_dereference_probe.cc"

grep -F 'QPDFObjectHandle.hh' "${probe}"
grep -F 'root.isIndirect()' "${probe}"
grep -F 'root.isDictionary()' "${probe}"
grep -F 'root.getParsedOffset()' "${probe}"
grep -F 'root.hasKey("/Pages")' "${probe}"
grep -F 'pages.isDictionary()' "${probe}"
grep -F 'pages.getParsedOffset()' "${probe}"

grep -F 'fetch-qpdf-source.sh' "${runner}"
grep -F '3b97c9bd266b7c32ea36d3536e22dab77412886d' "${runner}"
grep -F 'status --porcelain --untracked-files=no' "${runner}"
grep -F -- '-I"${qpdf_source}/include"' "${runner}"
grep -F -- '-I"${qpdf_source}/libqpdf"' "${runner}"
grep -F 'ldd "${probe_binary}"' "${runner}"
grep -F 'probe resolved an unpinned libqpdf' "${runner}"

if "${runner}" >/dev/null 2>&1; then
  echo "runner accepted missing input" >&2
  exit 1
fi
