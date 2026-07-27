# qpdf Stateful RC4 Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a crate-private, stateful RC4 component matching qpdf 11.9.0, migrate every flpdf RC4 consumer to it, and remove the old one-shot implementation and unused external dependency.

**Architecture:** `security::rc4::Rc4` owns qpdf's 256-byte permutation plus the two PRGA indices and exposes allocating and in-place processing over one retained state. PDF security-handler consumers construct a fresh component for each PDF Algorithm 3/5/6/7 pass, while focused unit and live-oracle tests exercise state reuse across calls. `Pl_RC4`, pipeline chunking, crypto-provider policy, and all public encryption configuration APIs remain outside this issue.

**Tech Stack:** Rust 2021 workspace; `std::ffi::CStr`; existing `PrimitiveError`; qpdf 11.9.0 at commit `3b97c9bd266b7c32ea36d3536e22dab77412886d`; C++17 oracle probe; Cargo tests, Clippy, strict private-item rustdoc, `scripts/qpdf-module-docs.py`, and `scripts/patch-coverage.sh`.

## Global Constraints

- Work only in `/home/ubuntu/flpdf/.worktrees/flpdf-qynx-2-1-rc4-core` on `feature/flpdf-qynx-2-1-rc4-core`.
- The component is `pub(crate)` and must not appear in flpdf's public API.
- `Rc4::new` accepts every non-empty explicit key, including keys longer than 256 bytes; only an empty key returns `PrimitiveError::InvalidLength`.
- `Rc4::from_c_str` uses the bytes before the terminating NUL and rejects an empty C string.
- `process` and `process_in_place` retain state across calls; empty input does not advance state.
- Every PDF Algorithm 3/5/6/7 pass constructs a fresh `Rc4`; state must not cross distinct keys or passes.
- In the final state, no `security::primitives::rc4` compatibility wrapper, external `rc4` crate use, duplicate KSA/PRGA, provider trait, `PlRc4`, or pipeline chunking may remain. Task 1 temporarily retains the old one-shot function so every intermediate commit builds; Task 3 migrates all consumers and deletes it atomically.
- qpdf behavior is taken from the pinned source tree resolved by `scripts/fetch-qpdf-source.sh --print-path`; the tree is read-only and must remain clean.
- Changed executable lines under `crates/flpdf/src` and `crates/flpdf-cli/src` must have 100% patch coverage against `origin/main`.
- Keep the existing RC4 weak-crypto policy and the public `EncryptParams::rc4` configuration API unchanged.

---

## File map

| Path | Responsibility |
|---|---|
| `crates/flpdf/src/security/rc4.rs` | Sole RC4 KSA/PRGA implementation, stateful API, unit tests, and Rust side of the qpdf differential |
| `crates/flpdf/src/security/mod.rs` | Declare the crate-private `rc4` module and describe the internal security boundary |
| `crates/flpdf/src/security/primitives.rs` | Retain AES/MD5/SHA2 and `PrimitiveError`; Task 3 removes the old RC4 implementation and tests atomically with consumer cutover |
| `crates/flpdf/src/security/standard.rs` | Route all production PDF RC4 password, string, and stream operations directly through `Rc4` |
| `crates/flpdf/src/filters.rs` | Route RC4-generating test helpers through `Rc4` |
| `tests/oracle/qpdf_rc4_probe.cc` | Exercise qpdf `RC4_native` in explicit/C-string, one-shot/split, and separate/in-place modes |
| `scripts/qpdf-rc4-diff.sh` | Build the pinned qpdf probe outside both trees and run the ignored live differential |
| `Cargo.toml`, `crates/flpdf/Cargo.toml`, `Cargo.lock` | Remove the unused external `rc4` dependency |
| `docs/qpdf-correspondence.md` | Mark `RC4.cc`/`RC4_native.cc` as a completed dedicated mirror without claiming `Pl_RC4` |
| `docs/qpdf-module-doc-index.md` | Generated module-correspondence index containing `security/rc4.rs` |

### Task 1: Stateful RC4 component

**Files:**
- Create: `crates/flpdf/src/security/rc4.rs`
- Modify: `crates/flpdf/src/security/mod.rs:1-10`

**Interfaces:**
- Consumes: `crate::security::primitives::PrimitiveError`
- Produces: `pub(crate) struct Rc4`; `Rc4::new(&[u8]) -> Result<Rc4, PrimitiveError>`; `Rc4::from_c_str(&CStr) -> Result<Rc4, PrimitiveError>`; `Rc4::process(&mut self, &[u8]) -> Vec<u8>`; `Rc4::process_in_place(&mut self, &mut [u8])`

- [ ] **Step 1: Add the module declaration and RED component tests**

Add `pub(crate) mod rc4;` next to `password`, `primitives`, and `standard` in
`security/mod.rs`. Create `security/rc4.rs` with the module doc, imports, and
tests below, but without defining `Rc4` yet:

```rust
//! qpdf correspondence: Mirrors qpdf 11.9.0 libqpdf/RC4.cc and libqpdf/RC4_native.cc.
//! Stateful RC4 compatibility component for legacy PDF encryption.

use std::ffi::CStr;

use crate::security::primitives::PrimitiveError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_6229_five_byte_key_keystream() {
        let mut cipher = Rc4::new(&[1, 2, 3, 4, 5]).unwrap();
        assert_eq!(
            cipher.process(&[0; 16]),
            [
                0xb2, 0x39, 0x63, 0x05, 0xf0, 0x3d, 0xc0, 0x27,
                0xcc, 0xc3, 0x52, 0x4a, 0x0a, 0x11, 0x18, 0xa8,
            ]
        );
    }

    #[test]
    fn classic_key_plaintext_vector() {
        let mut cipher = Rc4::new(b"Key").unwrap();
        assert_eq!(
            cipher.process(b"Plaintext"),
            [0xbb, 0xf3, 0x16, 0xe8, 0xd9, 0x40, 0xaf, 0x0a, 0xd3]
        );
    }

    #[test]
    fn accepts_qpdf_explicit_key_lengths() {
        for len in [1, 5, 16, 256, 300] {
            let key = (0..len).map(|i| i as u8).collect::<Vec<_>>();
            let mut cipher = Rc4::new(&key).unwrap();
            assert_eq!(cipher.process(&[0]).len(), 1, "key length {len}");
        }
    }

    #[test]
    fn bytes_after_ksa_window_do_not_change_state() {
        let prefix = (0..=255).map(|i| i as u8).collect::<Vec<_>>();
        let mut key_a = prefix.clone();
        key_a.extend_from_slice(&[1, 2, 3]);
        let mut key_b = prefix;
        key_b.extend_from_slice(&[9, 8, 7]);

        let mut a = Rc4::new(&key_a).unwrap();
        let mut b = Rc4::new(&key_b).unwrap();
        assert_eq!(a.process(&[0; 64]), b.process(&[0; 64]));
    }

    #[test]
    fn split_calls_retain_the_same_state_as_one_call() {
        let input = b"state must continue across process calls";
        let mut one_shot = Rc4::new(b"split-key").unwrap();
        let expected = one_shot.process(input);

        let mut split = Rc4::new(b"split-key").unwrap();
        let mut actual = split.process(&input[..7]);
        actual.extend(split.process(&input[7..]));
        assert_eq!(actual, expected);
    }

    #[test]
    fn allocating_and_in_place_processing_match() {
        let input = b"same input and output pointers are supported";
        let mut allocating = Rc4::new(b"in-place-key").unwrap();
        let expected = allocating.process(input);

        let mut actual = input.to_vec();
        Rc4::new(b"in-place-key")
            .unwrap()
            .process_in_place(&mut actual);
        assert_eq!(actual, expected);
    }

    #[test]
    fn c_string_mode_excludes_the_terminating_nul() {
        let c_key = CStr::from_bytes_with_nul(b"Key\0").unwrap();
        let mut c_string = Rc4::from_c_str(c_key).unwrap();
        let mut explicit = Rc4::new(b"Key").unwrap();
        assert_eq!(c_string.process(&[0; 32]), explicit.process(&[0; 32]));
    }

    #[test]
    fn empty_input_does_not_advance_state() {
        let mut after_empty = Rc4::new(b"Key").unwrap();
        assert!(after_empty.process(&[]).is_empty());
        let mut empty_in_place = [];
        after_empty.process_in_place(&mut empty_in_place);

        let mut fresh = Rc4::new(b"Key").unwrap();
        assert_eq!(after_empty.process(b"next"), fresh.process(b"next"));
    }

    #[test]
    fn empty_explicit_and_c_string_keys_are_rejected() {
        assert!(matches!(
            Rc4::new(b""),
            Err(PrimitiveError::InvalidLength)
        ));
        let empty = CStr::from_bytes_with_nul(b"\0").unwrap();
        assert!(matches!(
            Rc4::from_c_str(empty),
            Err(PrimitiveError::InvalidLength)
        ));
    }
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p flpdf --lib security::rc4::tests
```

Expected: compilation fails because `Rc4` is not defined. The failure must
come from the new test module, not from an unrelated baseline error.

- [ ] **Step 3: Implement the minimal qpdf-shaped state machine**

Insert this definition before the test module:

```rust
/// Stateful RC4 compatibility cipher mirroring qpdf's `RC4_native`.
///
/// RC4 is cryptographically broken and is retained only for legacy PDF
/// compatibility. Higher layers own the weak-crypto policy.
pub(crate) struct Rc4 {
    state: [u8; 256],
    x: u8,
    y: u8,
}

impl Rc4 {
    /// Initialize RC4 from an explicit non-empty key.
    pub(crate) fn new(key: &[u8]) -> Result<Self, PrimitiveError> {
        if key.is_empty() {
            return Err(PrimitiveError::InvalidLength);
        }

        let mut state = [0; 256];
        for (i, byte) in state.iter_mut().enumerate() {
            *byte = i as u8;
        }
        let mut key_index = 0;
        let mut state_index = 0_u8;
        for i in 0..256 {
            state_index = state_index
                .wrapping_add(key[key_index])
                .wrapping_add(state[i]);
            state.swap(i, usize::from(state_index));
            key_index = (key_index + 1) % key.len();
        }

        Ok(Self {
            state,
            x: 0,
            y: 0,
        })
    }

    /// Initialize RC4 using qpdf's NUL-terminated key mode.
    pub(crate) fn from_c_str(key: &CStr) -> Result<Self, PrimitiveError> {
        Self::new(key.to_bytes())
    }

    /// Return an encrypted/decrypted copy of `input`, retaining stream state.
    pub(crate) fn process(&mut self, input: &[u8]) -> Vec<u8> {
        let mut output = input.to_vec();
        self.process_in_place(&mut output);
        output
    }

    /// Encrypt/decrypt `data` in place, retaining stream state.
    pub(crate) fn process_in_place(&mut self, data: &mut [u8]) {
        for byte in data {
            self.x = self.x.wrapping_add(1);
            self.y = self.y.wrapping_add(self.state[usize::from(self.x)]);
            self.state
                .swap(usize::from(self.x), usize::from(self.y));
            let key_index = self.state[usize::from(self.x)]
                .wrapping_add(self.state[usize::from(self.y)]);
            *byte ^= self.state[usize::from(key_index)];
        }
    }
}
```

Do not add `Clone`, `Copy`, a public re-export, a one-shot free function, or a
key-length upper bound.

- [ ] **Step 4: Run focused tests and make them GREEN**

Run:

```bash
cargo test -p flpdf --lib security::rc4::tests
cargo fmt --all -- --check
```

Expected: all nine RC4 component tests pass and formatting is clean.

- [ ] **Step 5: Preserve a buildable transition until consumer cutover**

Do not edit `security/primitives.rs` in this task. Its old one-shot function
must remain temporarily because `security/standard.rs` and `filters.rs` still
compile against it. Task 3 deletes that implementation, its tests, and its
documentation in the same commit that migrates every consumer.

Run:

```bash
cargo test -p flpdf --lib security::primitives::tests::rc4
cargo check -p flpdf
```

Expected: the legacy characterization tests and the crate build still pass.
The temporary duplicate exists only across Task 1 and Task 2 commits and is
removed by Task 3.

- [ ] **Step 6: Verify the new component boundary and commit**

Run:

```bash
rg -n "struct Rc4|process_in_place" crates/flpdf/src/security
cargo test -p flpdf --lib security::rc4::tests
cargo test -p flpdf --lib security::primitives::tests
cargo check -p flpdf
git diff --check
```

Expected: the new stateful type appears only in `security/rc4.rs`; both
focused suites and the crate build pass. The old function remains reachable
only until Task 3.

Commit:

```bash
git add crates/flpdf/src/security/mod.rs crates/flpdf/src/security/rc4.rs
git commit -m "feat(rc4): add qpdf stateful core"
```

### Task 2: qpdf 11.9.0 differential oracle

**Files:**
- Create: `tests/oracle/qpdf_rc4_probe.cc`
- Create: `scripts/qpdf-rc4-diff.sh`
- Modify: `crates/flpdf/src/security/rc4.rs`

**Interfaces:**
- Consumes: `Rc4::{new,from_c_str,process,process_in_place}` from Task 1; pinned qpdf `RC4_native(unsigned char const*, int)` and `process(unsigned char const*, size_t, unsigned char*)`
- Produces: `QPDF_RC4_PROBE` test boundary and `scripts/qpdf-rc4-diff.sh`

- [ ] **Step 1: Add the RED Rust differential harness**

Extend the `security/rc4.rs` test module with owned oracle cases:

```rust
use std::path::Path;
use std::process::Command;

#[derive(Clone, Copy)]
enum OracleKeyMode {
    Explicit,
    CStr,
}

struct OracleCase {
    name: &'static str,
    mode: OracleKeyMode,
    key: Vec<u8>,
    input: Vec<u8>,
    split: usize,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn oracle_cases() -> Vec<OracleCase> {
    vec![
        OracleCase {
            name: "explicit-one-byte-empty-input",
            mode: OracleKeyMode::Explicit,
            key: vec![0x7f],
            input: vec![],
            split: 0,
        },
        OracleCase {
            name: "explicit-five-byte-rfc",
            mode: OracleKeyMode::Explicit,
            key: vec![1, 2, 3, 4, 5],
            input: vec![0; 32],
            split: 7,
        },
        OracleCase {
            name: "explicit-sixteen-byte-in-place",
            mode: OracleKeyMode::Explicit,
            key: (0..16).collect(),
            input: (0..97).map(|i| (i * 17) as u8).collect(),
            split: 31,
        },
        OracleCase {
            name: "explicit-256-byte-key",
            mode: OracleKeyMode::Explicit,
            key: (0..=255).collect(),
            input: (0..64).collect(),
            split: 1,
        },
        OracleCase {
            name: "explicit-key-over-256",
            mode: OracleKeyMode::Explicit,
            key: (0..300).map(|i| (i * 29) as u8).collect(),
            input: (0..129).map(|i| (i * 11) as u8).collect(),
            split: 128,
        },
        OracleCase {
            name: "c-string-first-nul",
            mode: OracleKeyMode::CStr,
            key: b"Key\0ignored suffix".to_vec(),
            input: b"Plaintext split across calls".to_vec(),
            split: 9,
        },
    ]
}

fn flpdf_records(case: &OracleCase) -> String {
    let new_cipher = || match case.mode {
        OracleKeyMode::Explicit => Rc4::new(&case.key).unwrap(),
        OracleKeyMode::CStr => {
            Rc4::from_c_str(CStr::from_bytes_until_nul(&case.key).unwrap()).unwrap()
        }
    };

    let mut one_shot = new_cipher();
    let one = one_shot.process(&case.input);
    let mut split_cipher = new_cipher();
    let mut split = split_cipher.process(&case.input[..case.split]);
    split.extend(split_cipher.process(&case.input[case.split..]));
    let mut in_place = case.input.clone();
    new_cipher().process_in_place(&mut in_place);
    format!(
        "one\t{}\nsplit\t{}\nin-place\t{}\n",
        hex(&one),
        hex(&split),
        hex(&in_place)
    )
}

fn run_qpdf_probe(probe: &Path, case: &OracleCase) -> String {
    let mode = match case.mode {
        OracleKeyMode::Explicit => "explicit",
        OracleKeyMode::CStr => "cstr",
    };
    let output = Command::new(probe)
        .args([
            mode,
            &hex(&case.key),
            &hex(&case.input),
            &case.split.to_string(),
        ])
        .output()
        .expect("execute qpdf RC4 probe");
    assert!(
        output.status.success(),
        "qpdf RC4 probe failed for {}: {}",
        case.name,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("probe output is ASCII")
}

#[test]
#[ignore = "live qpdf 11.9.0 RC4 oracle"]
fn qpdf_rc4_differential() {
    let probe = std::env::var_os("QPDF_RC4_PROBE")
        .expect("set QPDF_RC4_PROBE to the qpdf 11.9.0 probe");
    for case in oracle_cases() {
        assert_eq!(
            flpdf_records(&case),
            run_qpdf_probe(Path::new(&probe), &case),
            "case {}",
            case.name
        );
    }
}
```

Keep `flpdf_records` and case construction covered by ordinary tests. Add:

```rust
#[test]
fn oracle_cases_have_matching_one_split_and_in_place_records() {
    for case in oracle_cases() {
        let records = flpdf_records(&case);
        let mut lines = records.lines().map(|line| line.split_once('\t').unwrap().1);
        let one = lines.next().unwrap();
        assert_eq!(lines.next(), Some(one), "split case {}", case.name);
        assert_eq!(lines.next(), Some(one), "in-place case {}", case.name);
    }
}
```

- [ ] **Step 2: Run the live test and verify RED**

Run:

```bash
cargo test -p flpdf --lib security::rc4::tests::oracle_cases_have_matching_one_split_and_in_place_records
cargo test -p flpdf --lib security::rc4::tests::qpdf_rc4_differential -- --ignored --exact
```

Expected: the ordinary harness test passes; the live test fails with the
missing `QPDF_RC4_PROBE` diagnostic. This is the RED boundary for the absent
probe/script, not an RC4 algorithm failure.

- [ ] **Step 3: Implement the C++ probe**

Create `tests/oracle/qpdf_rc4_probe.cc`. It must:

```cpp
#include <qpdf/RC4_native.hh>

#include <cstdlib>
#include <iomanip>
#include <iostream>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>

namespace
{
    std::vector<unsigned char>
    decode(std::string const& value)
    {
        if ((value.size() % 2) != 0) {
            throw std::runtime_error("odd-length hex");
        }
        std::vector<unsigned char> result;
        result.reserve(value.size() / 2 + 1);
        for (size_t i = 0; i < value.size(); i += 2) {
            auto byte = std::stoul(value.substr(i, 2), nullptr, 16);
            result.push_back(static_cast<unsigned char>(byte));
        }
        return result;
    }

    std::string
    encode(std::vector<unsigned char> const& value)
    {
        std::ostringstream result;
        result << std::hex << std::setfill('0');
        for (auto byte: value) {
            result << std::setw(2) << static_cast<unsigned int>(byte);
        }
        return result.str();
    }

    RC4_native
    make_cipher(
        std::vector<unsigned char> const& key,
        size_t explicit_key_len,
        bool c_string)
    {
        return RC4_native(
            key.data(),
            c_string ? -1 : static_cast<int>(explicit_key_len));
    }
}

int
main(int argc, char* argv[])
{
    try {
        if (argc != 5) {
            throw std::runtime_error(
                "usage: qpdf_rc4_probe explicit|cstr KEY_HEX INPUT_HEX SPLIT");
        }
        bool c_string = std::string(argv[1]) == "cstr";
        if (!c_string && std::string(argv[1]) != "explicit") {
            throw std::runtime_error("invalid key mode");
        }
        auto key = decode(argv[2]);
        auto input = decode(argv[3]);
        size_t split_at = std::stoull(argv[4]);
        if (split_at > input.size()) {
            throw std::runtime_error("split exceeds input");
        }
        size_t explicit_key_len = key.size();
        size_t input_len = input.size();
        key.push_back(0);
        input.push_back(0);

        auto one_cipher = make_cipher(key, explicit_key_len, c_string);
        std::vector<unsigned char> one(input_len == 0 ? 1 : input_len);
        one_cipher.process(input.data(), input_len, one.data());
        one.resize(input_len);

        auto split_cipher = make_cipher(key, explicit_key_len, c_string);
        std::vector<unsigned char> split(input_len == 0 ? 1 : input_len);
        split_cipher.process(input.data(), split_at, split.data());
        split_cipher.process(
            input.data() + split_at,
            input_len - split_at,
            split.data() + split_at);
        split.resize(input_len);

        auto in_place_cipher = make_cipher(key, explicit_key_len, c_string);
        auto in_place = input;
        in_place_cipher.process(
            in_place.data(), input_len, in_place.data());
        in_place.resize(input_len);

        std::cout << "one\t" << encode(one) << "\n"
                  << "split\t" << encode(split) << "\n"
                  << "in-place\t" << encode(in_place) << "\n";
        return 0;
    } catch (std::exception const& error) {
        std::cerr << "qpdf_rc4_probe: " << error.what() << "\n";
        return 2;
    }
}
```

The explicit key length passed to qpdf excludes the appended sentinel NUL.
Do not invoke qpdf on an empty key because qpdf 11.9.0 assumes its operational
precondition and would divide by zero in KSA.

- [ ] **Step 4: Implement the pinned-source build script**

Create executable `scripts/qpdf-rc4-diff.sh` using the same pin and cleanliness
rules as `scripts/qpdf-tokenizer-diff.sh`, but compile only the probe and
`RC4_native.cc`:

```bash
#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
qpdf_source="$(
  cd "$("${repo_root}/scripts/fetch-qpdf-source.sh" --print-path)"
  pwd -P
)"
qpdf_commit="3b97c9bd266b7c32ea36d3536e22dab77412886d"
build_dir=

path_is_within() {
  local child="$1"
  local parent="$2"
  [[ "${child}" == "${parent}" || "${child}" == "${parent}/"* ]]
}

cleanup() {
  local status=$?
  trap - EXIT
  if [[ -n "${build_dir}" && -d "${build_dir}" ]]; then
    rm -rf -- "${build_dir}"
  fi
  if [[ -n "$(git -C "${qpdf_source}" status --porcelain --untracked-files=no)" ]]; then
    echo "qpdf-rc4-diff.sh: probe modified pinned tracked source" >&2
    status=1
  fi
  exit "${status}"
}
trap cleanup EXIT

temp_base="${TMPDIR:-/tmp}"
if [[ -L "${temp_base}" || ! -d "${temp_base}" ]]; then
  echo "qpdf-rc4-diff.sh: TMPDIR is not a real directory" >&2
  exit 1
fi
temp_base="$(cd "${temp_base}" && pwd -P)"
if path_is_within "${temp_base}" "${repo_root}" ||
  path_is_within "${temp_base}" "${qpdf_source}"; then
  echo "qpdf-rc4-diff.sh: TMPDIR must be outside the repository and pinned source" >&2
  exit 1
fi
build_dir="$(mktemp -d "${temp_base}/flpdf-qpdf-rc4.XXXXXXXX")"
build_dir="$(cd "${build_dir}" && pwd -P)"
if path_is_within "${build_dir}" "${repo_root}" ||
  path_is_within "${build_dir}" "${qpdf_source}" ||
  [[ "$(stat -c '%u' -- "${build_dir}")" != "${UID}" ]] ||
  [[ "$(stat -c '%a' -- "${build_dir}")" != 700 ]]; then
  echo "qpdf-rc4-diff.sh: build directory is not private and external" >&2
  exit 1
fi

if [[ "$(git -C "${qpdf_source}" rev-parse HEAD)" != "${qpdf_commit}" ]]; then
  echo "qpdf-rc4-diff.sh: pinned source is not at ${qpdf_commit}" >&2
  exit 1
fi
if [[ -n "$(git -C "${qpdf_source}" status --porcelain --untracked-files=no)" ]]; then
  echo "qpdf-rc4-diff.sh: pinned source has tracked-file changes" >&2
  exit 1
fi

probe="${build_dir}/qpdf_rc4_probe"
c++ -std=c++17 \
  -I"${qpdf_source}/libqpdf" \
  "${repo_root}/tests/oracle/qpdf_rc4_probe.cc" \
  "${qpdf_source}/libqpdf/RC4_native.cc" \
  -o "${probe}"

cd "${repo_root}"
QPDF_RC4_PROBE="${probe}" \
  cargo test -p flpdf --lib \
  security::rc4::tests::qpdf_rc4_differential -- --ignored --exact
```

This preserves the approved private-build contract even when `TMPDIR` is
overridden. Mark the file executable:

```bash
chmod +x scripts/qpdf-rc4-diff.sh
```

- [ ] **Step 5: Run the live oracle and commit**

Run:

```bash
scripts/qpdf-rc4-diff.sh
git -C "$(scripts/fetch-qpdf-source.sh --print-path)" status --short
cargo test -p flpdf --lib security::rc4::tests
git diff --check
```

Expected: all explicit and C-string cases match qpdf for one-shot, retained
split state, and in-place processing; the pinned source status is empty; all
ordinary RC4 tests pass.

Commit:

```bash
git add crates/flpdf/src/security/rc4.rs scripts/qpdf-rc4-diff.sh tests/oracle/qpdf_rc4_probe.cc
git commit -m "test(rc4): compare state machine with qpdf"
```

### Task 3: Consumer cutover and old-route deletion

**Files:**
- Modify: `crates/flpdf/src/security/primitives.rs:1-92,208-246`
- Modify: `crates/flpdf/src/security/standard.rs:45-46,568-688,738-887,1497-1525,1615-1632,1953-2078`
- Modify: `crates/flpdf/src/filters.rs:861-928`
- Modify: `Cargo.toml:25-40`
- Modify: `crates/flpdf/Cargo.toml:16-27`
- Modify: `Cargo.lock`

**Interfaces:**
- Consumes: `crate::security::rc4::Rc4` from Task 1
- Produces: every production and test-only RC4 cipher operation uses `Rc4` directly; no old free-function route or external dependency remains

- [ ] **Step 1: Record the complete pre-cutover inventory**

Run and save the output in the terminal log:

```bash
rg -n "security::primitives::rc4|\\brc4\\s*\\(" crates --glob '*.rs'
rg -n '^rc4 =|rc4\\.workspace' Cargo.toml crates/*/Cargo.toml
rg -n 'state\\.swap|wrapping_add\\(.*state' crates/flpdf/src --glob '*.rs'
```

Classify `EncryptParams::rc4(...)` and RC4 enum/configuration names separately:
they are public configuration consumers and remain unchanged. Every bare
`rc4(key, data)` cipher call must disappear.

- [ ] **Step 2: Run characterization suites before editing**

Run:

```bash
cargo test -p flpdf --lib security::standard::tests
cargo test -p flpdf --lib filters::tests::decode_stream_data_with_decryption
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf-cli --test cli_tests
cargo test -p flpdf-cli --test encrypt_cli_tests
cargo test -p flpdf-cli --test encrypt_decrypt_matrix_tests
```

Expected: all existing RC4 reader, writer, filter, and CLI behavior is GREEN
before the route-only refactor.

- [ ] **Step 3: Migrate production security-handler calls directly**

Change the import in `security/standard.rs` to:

```rust
use crate::security::primitives::{md5, sha256, sha384, sha512};
use crate::security::rc4::Rc4;
```

Replace each fallible one-shot call:

```rust
rc4(&file_key, &mut data)?;
```

with a fresh component:

```rust
let mut cipher = Rc4::new(&file_key)?;
cipher.process_in_place(&mut data);
```

Apply the same exact pattern to `encrypted`, `candidate`, `buf`, `bytes`, and
each `xor_key`. In the 19/20-pass loops, construct `Rc4` inside the loop after
the pass-specific key is derived:

```rust
for i in 1_u8..=19 {
    let xor_key: Vec<u8> = file_key.iter().map(|&byte| byte ^ i).collect();
    let mut cipher = Rc4::new(&xor_key)?;
    cipher.process_in_place(&mut data);
}
```

Do not reuse `cipher` between loop iterations. Convert the RC4 match arms to
blocks because processing itself is infallible:

```rust
StringCipher::Rc4 { key } => {
    let mut cipher = Rc4::new(key)?;
    cipher.process_in_place(bytes);
    Ok(())
}
```

and:

```rust
StringEncryptCipher::Rc4 { key } => {
    let mut cipher = Rc4::new(key)?;
    cipher.process_in_place(bytes);
    Ok(())
}
```

- [ ] **Step 4: Migrate test-only ciphertext helpers**

In `security/standard.rs` tests and `filters.rs` tests, import
`crate::security::rc4::Rc4` and replace every old helper call with:

```rust
Rc4::new(key).unwrap().process_in_place(&mut encrypted);
```

Use the actual local buffer name at each site (`nested`, `in_dict`,
`in_stream_dict`, `secret`, `rc4_string`, or `encrypted`). Do not add a test
wrapper that recreates the deleted one-shot API.

- [ ] **Step 5: Delete the old primitive implementation and duplicate tests**

Delete `security::primitives::rc4`, its KSA/PRGA comments, the module-level
RC4 notice/link, and the four RC4 tests from `primitives.rs`. Change its first
correspondence line to:

```rust
//! qpdf correspondence: Rust crypto-crate substitution for qpdf AES, MD5, and SHA2 native implementations.
```

Keep `PrimitiveError` in `primitives.rs`; `Rc4` imports it from there. Do not
change the `From<PrimitiveError> for Error` bridge.

- [ ] **Step 6: Verify consumer GREEN before dependency cleanup**

Run:

```bash
cargo test -p flpdf --lib security::standard::tests
cargo test -p flpdf --lib filters::tests::decode_stream_data_with_decryption
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf-cli --test cli_tests
cargo test -p flpdf-cli --test encrypt_cli_tests
cargo test -p flpdf-cli --test encrypt_decrypt_matrix_tests
```

Expected: the same characterization suites remain GREEN after direct cutover.

- [ ] **Step 7: Remove the external dependency and update the lockfile**

Delete:

```toml
rc4 = "0.1"
```

from the workspace dependencies and:

```toml
rc4.workspace = true
```

from `crates/flpdf/Cargo.toml`. Update the lockfile through Cargo:

```bash
cargo check -p flpdf
```

Inspect the lock diff. The `rc4` package entry must disappear; unrelated
package versions must not change.

- [ ] **Step 8: Enforce the negative contract and commit**

Run:

```bash
if rg -n "security::primitives::rc4|pub\\(crate\\) fn rc4" crates --glob '*.rs'; then
  exit 1
fi
if rg -n '^rc4 =|rc4\\.workspace' Cargo.toml crates/*/Cargo.toml; then
  exit 1
fi
rg -n "\\brc4\\s*\\(" crates --glob '*.rs'
rg -n 'state\\.swap|wrapping_add\\(.*state' crates/flpdf/src --glob '*.rs'
cargo fmt --all -- --check
git diff --check
```

Expected: the guarded searches find no deleted implementation/import/dependency;
the remaining `rc4(` matches are only `EncryptParams::rc4` configuration
constructors; KSA/PRGA state manipulation appears only in `security/rc4.rs`.

Commit:

```bash
git add Cargo.toml Cargo.lock crates/flpdf/Cargo.toml crates/flpdf/src/security/primitives.rs crates/flpdf/src/security/standard.rs crates/flpdf/src/filters.rs
git commit -m "refactor(rc4): cut consumers over to stateful core"
```

### Task 4: Correspondence docs and complete verification

**Files:**
- Modify: `crates/flpdf/src/security/mod.rs:1-7`
- Modify: `docs/qpdf-correspondence.md:105-130,170-182`
- Modify: `docs/qpdf-module-doc-index.md`

**Interfaces:**
- Consumes: completed implementation and cutover from Tasks 1-3
- Produces: truthful qpdf correspondence, regenerated module index, CI-equivalent evidence, and a clean committed branch ready for the required whole-branch review

- [ ] **Step 1: Update the human-maintained correspondence**

In `security/mod.rs`, remove the stale external-type example `rc4::Rc4`; keep
the statement that internal types never enter the public API.

In `docs/qpdf-correspondence.md`:

- split `RC4_native` out of the AES/MD5/SHA2 external-substitution row;
- add `RC4.cc` / `RC4_native.cc` mapped to `security/rc4.rs` with status `✅`;
- state that it covers explicit and C-string keys, retained state, and
  separate/in-place processing;
- leave `Pl_AES_PDF / Pl_RC4` as `🔀` and explicitly point `Pl_RC4` follow-up
  work to `flpdf-qynx.2.2`;
- remove `RC4.cc` from the later `Buffer.cc / MD5.cc / RC4.cc` external-crate
  grouping so the component is not classified twice;
- do not refresh unrelated snapshot line counts.

- [ ] **Step 2: Regenerate and validate the module index**

Run:

```bash
python3 scripts/qpdf-module-docs.py --write
python3 scripts/qpdf-module-docs.py --check
python3 -m unittest scripts.tests.test_qpdf_module_docs
```

Expected: `docs/qpdf-module-doc-index.md` contains the exact
`security/rc4.rs` correspondence line, the generator check is clean, and its
contract tests pass.

- [ ] **Step 3: Run focused oracle and consumer gates**

Run:

```bash
scripts/qpdf-rc4-diff.sh
cargo test -p flpdf --lib security::rc4::tests
cargo test -p flpdf --lib security::standard::tests
cargo test -p flpdf --lib filters::tests::decode_stream_data_with_decryption
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf-cli --test cli_tests
cargo test -p flpdf-cli --test encrypt_cli_tests
cargo test -p flpdf-cli --test encrypt_decrypt_matrix_tests
```

Expected: live qpdf parity and every RC4 consumer regression pass.

- [ ] **Step 4: Run workspace quality gates**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
cargo test
python3 scripts/qpdf-module-docs.py --check
git diff --check
```

Expected: every command exits zero.

- [ ] **Step 5: Commit documentation**

```bash
git add crates/flpdf/src/security/mod.rs docs/qpdf-correspondence.md docs/qpdf-module-doc-index.md
git commit -m "docs(rc4): record qpdf component parity"
```

The worktree must be clean before coverage because the patch gate compares
committed `HEAD` with its base.

- [ ] **Step 6: Measure fresh 100% changed-line coverage**

Run:

```bash
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected: the authoritative patch gate reports 100% for all changed executable
lines. If a reachable line is uncovered, add a focused behavioral test, commit
it, clean coverage data, and rerun both commands. Do not add a reasonless
`cov:ignore`.

- [ ] **Step 7: Final boundary audit**

Run:

```bash
git status --short
git log --oneline origin/main..HEAD
rg -n "struct Rc4|state\\.swap|process_in_place" crates/flpdf/src/security
if rg -n "security::primitives::rc4|pub\\(crate\\) fn rc4" crates --glob '*.rs'; then
  exit 1
fi
if rg -n '^rc4 =|rc4\\.workspace' Cargo.toml crates/*/Cargo.toml; then
  exit 1
fi
rg -n "Pl_RC4|flpdf-qynx\\.2\\.2" docs/qpdf-correspondence.md docs/superpowers/specs/2026-07-28-qpdf-stateful-rc4-core-design.md
git -C "$(scripts/fetch-qpdf-source.sh --print-path)" status --short
```

Expected: the worktree and pinned qpdf tree are clean; `Rc4` is the sole
state-machine implementation; old routes and dependencies are absent;
`Pl_RC4` remains explicitly incomplete and assigned to `.2.2`.

### Task 5: Controller-owned final review and publication

These steps are controller-owned and run only after the Task 4 scoped review
is clean.

- [ ] **Step 1: Run the required whole-branch review**

Generate a review package from the branch start to `HEAD`, dispatch the
most-capable final reviewer, and complete the single permitted fix wave plus
scoped re-review if findings exist. Do not close the Bead while any
load-bearing finding remains.

- [ ] **Step 2: Close Beads state and push both stores**

Record exact test and coverage evidence, close only `flpdf-qynx.2.1`, and keep
the parent and `.2.2` open:

```bash
bd update flpdf-qynx.2.1 --append-notes "Implemented crate-private qpdf 11.9.0 stateful RC4 core; migrated every cipher consumer; removed the old one-shot route and external rc4 dependency; live oracle, focused/workspace/clippy/rustdoc/module-doc gates pass; fresh patch coverage is 100% vs origin/main."
bd close flpdf-qynx.2.1 --reason="Stateful RC4 core, direct consumer cutover, old-route deletion, qpdf differential, and 100% changed-line coverage complete."
bd dolt push
git push
```

Before pushing, state that the branch contains the scoped commits and that
the Bead will be closed. Expected: both pushes succeed.

- [ ] **Step 3: Verify remote publication**

Run:

```bash
git status --short
git branch -vv
bd show flpdf-qynx.2.1
bd show flpdf-qynx.2.2
bd dolt push
```

Expected: the branch is clean and tracks its remote at the same commit;
`.2.1` is closed with evidence; `.2.2` remains open and is no longer blocked
by `.2.1`; the final Beads push succeeds.
