# Opt-in Decode Limits + Filter Chain Length Cap (flpdf-hn1g.4) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (or subagent-driven-development) to implement this plan task-by-task.

**Goal:** Mitigate decode-side resource exhaustion by (1) an unconditional `/Filter` chain length cap on the decode path and (2) an opt-in decompression output-size limit API (`DecodeLimits` + `decode_stream_data_with_limits`), without changing the existing public `decode_stream_data` signature.

**Architecture:** Scope is **API-only** (confirmed): the opt-in output limit is a library API embedders choose to use (qpdf's `Pl_Flate::setMemoryLimit` model); flpdf's own document paths stay unbounded per threat-model §4. The chain cap is unconditional because it funnels through the single decode chain function `decode_stream_data_with_filters_and_crypt`. All work is in `crates/flpdf/src/filters.rs` plus a `docs/threat-model.md` update. Errors use `Error::Unsupported` (matches the documented convention for depth/oversized limits; `Error` is not `#[non_exhaustive]`, so no new variant).

**Tech Stack:** Rust (`flpdf`), `flate2::read::ZlibDecoder`, `cargo test`, `scripts/patch-coverage.sh`.

**Key facts:**
- `decode_stream_data` (pub) → `decode_stream_data_with_filters` → `decode_stream_data_with_filters_and_crypt` (the chain loop). Also `decode_stream_data_with_crypt_filter` (pub(crate)) → `_and_crypt`.
- `apply_single_filter_decode` (FlateDecode `read_to_end` at ~496-500; LZWDecode → `lzw_decode`) returns `Result<_, String>` mapped to `Error::Unsupported` by the caller.
- `filters` is `pub mod` (no lib.rs re-export); new public items are reachable as `flpdf::filters::*`.
- Post-hoc length checks do NOT work for Flate (`read_to_end` OOMs first) — must bound the read itself.

---

### Task 1: Unconditional `/Filter` chain length cap (decode only)

**Files:**
- Modify: `crates/flpdf/src/filters.rs` (add const; array branch of `decode_stream_data_with_filters_and_crypt` ~line 124)
- Test: `crates/flpdf/src/filters.rs` `#[cfg(test)] mod tests`

**Step 1: Write the failing test**

```rust
#[test]
fn decode_rejects_overlong_filter_chain() {
    // 17 filters (> MAX_FILTER_CHAIN_LEN = 16) on the decode path is rejected
    // before any stage runs. The data is irrelevant; the cap trips first.
    let mut dict = Dictionary::new();
    dict.insert(
        "Filter",
        Object::Array(vec![Object::Name(b"FlateDecode".to_vec()); 17]),
    );
    let err = decode_stream_data(&dict, b"anything");
    assert!(
        matches!(err, Err(Error::Unsupported(ref m)) if m.contains("filter chain length")),
        "got {err:?}"
    );
}

#[test]
fn decode_accepts_max_length_filter_chain() {
    // Exactly MAX_FILTER_CHAIN_LEN (16) ASCIIHexDecode stages round-trips (each
    // stage is identity here: hex-encode applied 16 times, then this many decodes).
    // Build by encoding 16 times so the 16-deep decode chain reproduces the input.
    let original = b"hello";
    let mut data = original.to_vec();
    for _ in 0..16 {
        data = encode_stream_data(
            &{
                let mut d = Dictionary::new();
                d.insert("Filter", Object::Name(b"ASCIIHexDecode".to_vec()));
                d
            },
            &data,
        )
        .unwrap();
    }
    let mut dict = Dictionary::new();
    dict.insert(
        "Filter",
        Object::Array(vec![Object::Name(b"ASCIIHexDecode".to_vec()); 16]),
    );
    let decoded = decode_stream_data(&dict, &data).unwrap();
    assert_eq!(decoded, original);
}
```

**Step 2: Run to verify failure**

Run: `cargo test -p flpdf --lib decode_rejects_overlong_filter_chain`
Expected: FAIL — without the cap, the 17×Flate chain attempts to decode `b"anything"` as Flate and errors with a *flate* message (not "filter chain length"), so the `contains("filter chain length")` assertion fails.

**Step 3: Implement**

Add near the top of `filters.rs` (after imports / other consts):
```rust
/// Maximum number of stages a `/Filter` chain may declare on the **decode**
/// path. Real PDFs use at most a few stages; this rejects only pathological
/// input where each stage re-expands the previous (multiplicative blow-up).
/// Unlike qpdf — which imposes no chain-length cap — flpdf rejects such chains
/// outright; this is an intentional divergence, not a compatibility target.
/// The encode path (writer output, not untrusted) is not capped.
const MAX_FILTER_CHAIN_LEN: usize = 16;
```

In `decode_stream_data_with_filters_and_crypt`, at the start of the `if let Some(filters) = filter.as_array() {` branch (before the `let mut decoded = ...` / loop):
```rust
            if let Some(filters) = filter.as_array() {
                if filters.len() > MAX_FILTER_CHAIN_LEN {
                    return Err(Error::Unsupported(format!(
                        "filter chain length {} exceeds maximum of {MAX_FILTER_CHAIN_LEN}",
                        filters.len()
                    )));
                }
                let mut decoded = stream_data.to_vec();
                // ... existing loop unchanged ...
```

**Step 4: Run to verify pass**

Run: `cargo test -p flpdf --lib decode_rejects_overlong_filter_chain decode_accepts_max_length_filter_chain`
Expected: both PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/filters.rs
git commit -m "feat(flpdf): cap /Filter decode chain length at 16 stages (flpdf-hn1g.4)"
```

---

### Task 2: Opt-in decode output limit (`DecodeLimits` + `decode_stream_data_with_limits`)

**Files:**
- Modify: `crates/flpdf/src/filters.rs` (new public type + fn; thread `max_output` through `_with_filters`, `_and_crypt`, `apply_single_filter_decode`, `lzw_decode`; bound Flate + LZW)
- Test: `crates/flpdf/src/filters.rs` `#[cfg(test)] mod tests`

**Step 1: Write the failing tests**

```rust
#[test]
fn flate_decode_honors_output_limit() {
    // 2000 'A' bytes compress small but decode large. A limit below 2000 is
    // rejected; a limit >= 2000 succeeds. Boundary: exactly 2000 succeeds.
    let raw = vec![b'A'; 2000];
    let mut dict = Dictionary::new();
    dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    let encoded = encode_stream_data(&dict, &raw).unwrap();

    // Under limit -> Unsupported.
    let err = decode_stream_data_with_limits(
        &dict,
        &encoded,
        DecodeLimits { max_output: Some(1999) },
    );
    assert!(
        matches!(err, Err(Error::Unsupported(ref m)) if m.contains("exceeds configured limit")),
        "got {err:?}"
    );
    // Exactly at limit -> Ok (boundary: take(limit+1) reads all 2000, len == limit).
    let ok = decode_stream_data_with_limits(
        &dict,
        &encoded,
        DecodeLimits { max_output: Some(2000) },
    )
    .unwrap();
    assert_eq!(ok.len(), 2000);
}

#[test]
fn lzw_decode_honors_output_limit() {
    // Build LZW-encoded data whose decoded length exceeds a small limit. flpdf
    // has no LZW encoder, so encode via a known fixture: a run of identical bytes
    // that LZW-decodes to > limit. Use a precomputed LZW stream for `b"-----..."`.
    // (Implementer: reuse an existing LZW round-trip fixture in the test module,
    // or craft minimal LZW bytes; assert the limit path trips.)
    // Pseudostructure:
    let lzw_bytes: &[u8] = LZW_FIXTURE_DECODING_TO_300_BYTES; // see existing lzw tests
    let mut dict = Dictionary::new();
    dict.insert("Filter", Object::Name(b"LZWDecode".to_vec()));
    let err = decode_stream_data_with_limits(
        &dict,
        lzw_bytes,
        DecodeLimits { max_output: Some(100) },
    );
    assert!(
        matches!(err, Err(Error::Unsupported(ref m)) if m.contains("exceeds configured limit")),
        "got {err:?}"
    );
    // Unbounded still decodes fully.
    let full = decode_stream_data(&dict, lzw_bytes).unwrap();
    assert!(full.len() > 100);
}

#[test]
fn decode_stream_data_is_unbounded_by_default() {
    // The legacy entry point keeps decoding arbitrarily large output (DecodeLimits
    // default = max_output None), guaranteeing backward compatibility.
    let raw = vec![b'Z'; 5000];
    let mut dict = Dictionary::new();
    dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    let encoded = encode_stream_data(&dict, &raw).unwrap();
    assert_eq!(decode_stream_data(&dict, &encoded).unwrap().len(), 5000);
    assert_eq!(DecodeLimits::default().max_output, None);
}
```
**Note for implementer (LZW fixture):** the test module already has LZW round-trip tests — reuse one of those encoded byte fixtures (decoding to a known length) instead of `LZW_FIXTURE_...`. Pick a limit below that known length. If no reusable fixture exists, hand-craft minimal LZW bytes that decode to a short repeated run and assert the boundary.

**Step 2: Run to verify failure**

Run: `cargo test -p flpdf --lib flate_decode_honors_output_limit`
Expected: FAIL to compile — `DecodeLimits` / `decode_stream_data_with_limits` do not exist yet.

**Step 3: Implement**

(a) Add the public API (near `decode_stream_data`):
```rust
/// Opt-in limits applied while decoding a stream's filter chain.
///
/// Default is unlimited, matching [`decode_stream_data`]. Embedders processing
/// untrusted input can set [`max_output`](Self::max_output) to bound the
/// decompressed size of each `FlateDecode` / `LZWDecode` stage (qpdf's
/// `Pl_Flate::setMemoryLimit` analogue), trading completeness for a memory
/// ceiling.
#[derive(Clone, Copy, Debug, Default)]
pub struct DecodeLimits {
    /// Maximum decompressed byte count permitted out of any single
    /// `FlateDecode` / `LZWDecode` stage. `None` (default) is unlimited.
    pub max_output: Option<usize>,
}

/// Decode a stream's filter chain like [`decode_stream_data`], enforcing the
/// opt-in [`DecodeLimits`].
///
/// # Errors
///
/// Returns [`Error::Unsupported`] for the same reasons as [`decode_stream_data`],
/// plus when a `FlateDecode` / `LZWDecode` stage's decompressed output exceeds
/// [`DecodeLimits::max_output`], or when the `/Filter` chain exceeds the fixed
/// stage cap.
pub fn decode_stream_data_with_limits(
    dict: &Dictionary,
    stream_data: &[u8],
    limits: DecodeLimits,
) -> Result<Vec<u8>> {
    decode_stream_data_with_filters(dict.get("Filter"), dict.get("DecodeParms"), stream_data, limits)
}
```

(b) Thread `limits: DecodeLimits` through:
- `decode_stream_data` (line 44): call `decode_stream_data_with_filters(..., DecodeLimits::default())`.
- `decode_stream_data_with_filters` (line 90): add `limits: DecodeLimits` param; forward to `_and_crypt`.
- `decode_stream_data_with_crypt_filter` (line 60): pass `DecodeLimits::default()` to `_and_crypt` (encryption path stays unbounded).
- `decode_stream_data_with_filters_and_crypt` (line 102): add `limits: DecodeLimits`; in BOTH the single-name branch and the array loop, pass `limits.max_output` to `apply_single_filter_decode`. (The internal closure `_with_filters`' default-crypt error path is unchanged.)
- `apply_single_filter_decode` (line 489): add `max_output: Option<usize>`.

(c) FlateDecode block — bound the read:
```rust
    if filter_name == b"FlateDecode" {
        let mut decoded = Vec::new();
        match max_output {
            Some(limit) => {
                // Bound the allocation during decode: a post-hoc length check
                // cannot help because read_to_end would OOM first on a bomb.
                // take(limit+1) lets output of exactly `limit` succeed while one
                // byte over is read as truncated and rejected. saturating_add
                // guards the (absurd) usize::MAX limit from overflowing.
                ZlibDecoder::new(stream_data)
                    .take((limit as u64).saturating_add(1))
                    .read_to_end(&mut decoded)
                    .map_err(|error| error.to_string())?;
                if decoded.len() > limit {
                    return Err(format!(
                        "decoded output exceeds configured limit of {limit} bytes"
                    ));
                }
            }
            None => {
                ZlibDecoder::new(stream_data)
                    .read_to_end(&mut decoded)
                    .map_err(|error| error.to_string())?;
            }
        }
        return Ok(decoded);
    }
```

(d) LZWDecode — pass the limit:
```rust
        return lzw_decode(stream_data, early_change, max_output);
```
and in `lzw_decode` signature add `max_output: Option<usize>`; after `output.extend_from_slice(&entry);`:
```rust
        output.extend_from_slice(&entry);
        if let Some(limit) = max_output {
            if output.len() > limit {
                return Err(format!(
                    "decoded output exceeds configured limit of {limit} bytes"
                ));
            }
        }
```

(e) Update the existing test-module LZW call(s) to `lzw_decode(data, early_change, None)` if any call `lzw_decode` directly.

**Step 4: Run to verify pass**

Run: `cargo test -p flpdf --lib flate_decode_honors_output_limit lzw_decode_honors_output_limit decode_stream_data_is_unbounded_by_default`
Expected: all PASS. Then full `cargo test -p flpdf` (all decode callers compile with the threaded default).

**Step 5: Commit**

```bash
git add crates/flpdf/src/filters.rs
git commit -m "feat(flpdf): opt-in DecodeLimits output cap for Flate/LZW decode (flpdf-hn1g.4)"
```

---

### Task 3: Update `docs/threat-model.md`

**Files:**
- Modify: `docs/threat-model.md` (§4 wording + §8 gap row)

**Step 1: §4 — replace the "no cap / no length limit / planned" wording**

In the bullet under §4 "Bounded memory or processing time":
- Change `/Filter chains have no length limit, allowing multiplicative expansion across stages;` → note that decode-path filter chains are now capped at 16 stages.
- Change `Opt-in decode limits comparable to qpdf's Pl_Flate::memory_limit are planned (§8).` → state they are now available via `filters::DecodeLimits` / `decode_stream_data_with_limits` (opt-in; default unbounded), while stream decode still places no cap by default.

Keep the honest framing: default behavior is still unbounded output (compression bombs remain out of scope per §4); only the chain cap is always-on, and the output limit is opt-in.

**Step 2: §8 — update the gap row**

Update the row:
`| No opt-in decode-output limits and no /Filter chain length cap (compression bombs covered by §4, but mitigations are worth offering). | §4 mitigation | flpdf-hn1g.4 |`
to reflect that both mitigations now exist (chain cap always-on; output limit opt-in via `DecodeLimits`). Either remove the row (gap closed) or restate it as "delivered" consistent with how other delivered items are tracked in the doc. Implementer: match the doc's existing convention for closed gaps; if unclear, restate the row noting the cap is in place and the opt-in API is provided.

**Step 3: Commit**

```bash
git add docs/threat-model.md
git commit -m "docs(threat-model): chain cap always-on + opt-in DecodeLimits (flpdf-hn1g.4)"
```

---

### Task 4: Quality gates + changed-line coverage

**Step 1:** `cargo test --workspace 2>&1 | rg "test result:|error\[|FAILED"` → all ok, 0 failed.

**Step 2:** `cargo fmt --all && cargo fmt --all --check && cargo clippy -p flpdf --all-targets -- -D warnings` → clean. (`#![forbid(unsafe_code)]` is on main; confirm no `unsafe` introduced.)

**Step 3:** `scripts/patch-coverage.sh --base main` → flpdf changed lines 100%. The three new error paths (chain cap, Flate over-limit, LZW over-limit) and the `None`/`Some` Flate arms must each be exercised; add tests or `// cov:ignore: <reason>` (note in PR) for any truly unreachable line.

**Step 4 (qualitative):** Confirm boundary tests exist (exactly limit OK / limit+1 rejected for Flate; LZW over-limit) and that `decode_stream_data` backward-compat (unbounded) is asserted, per CLAUDE.md test-coverage gate step 4.

**Step 5:** REQUIRED SUB-SKILL: superpowers:verification-before-completion — confirm real command output before claiming done.
