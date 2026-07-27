#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd -P)"
fixture_root="$(mktemp -d)"

cleanup() {
  rm -rf -- "${fixture_root}"
}
trap cleanup EXIT

fixture_include="${fixture_root}/include"
probe="${fixture_root}/qpdf_rc4_probe"
mkdir -p "${fixture_include}/qpdf"

cat >"${fixture_include}/qpdf/RC4_native.hh" <<'EOF'
#pragma once

#include <cstddef>

class RC4_native
{
  public:
    RC4_native(unsigned char const*, int)
    {
    }

    void
    process(
        unsigned char const* input,
        std::size_t length,
        unsigned char* output)
    {
        for (std::size_t i = 0; i < length; ++i) {
            output[i] = input[i];
        }
    }
};
EOF

c++ -std=c++17 \
  -I"${fixture_include}" \
  "${repo_root}/tests/oracle/qpdf_rc4_probe.cc" \
  -o "${probe}"

assert_rejected() {
  local expected="$1"
  shift
  local output

  if output="$("${probe}" "$@" 2>&1)"; then
    echo "qpdf-rc4-probe contract: malformed input unexpectedly succeeded: $*" >&2
    exit 1
  fi
  if [[ "${output}" != "qpdf_rc4_probe: ${expected}" ]]; then
    echo \
      "qpdf-rc4-probe contract: expected '${expected}', got '${output}'" \
      >&2
    exit 1
  fi
}

assert_rejected "invalid hex" explicit 0g "" 0
assert_rejected "invalid split" explicit 00 "" 0junk
assert_rejected "empty C-string key" cstr "" "" 0

printf 'qpdf-rc4-probe contract: PASS\n'
