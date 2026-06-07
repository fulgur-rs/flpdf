//! Typed permission configuration for writer-side `/Encrypt` `/P` encoding.
//!
//! [`Permissions`](crate::Permissions) (in `reader.rs`) is the read-only
//! view of an already-encrypted document's `/P` bitfield, with one accessor
//! per capability bit. This module is its writer-side counterpart: a typed
//! configuration that callers populate and then encode into the `/P`
//! bitfield via [`PermissionsConfig::to_p_bits`], which the various
//! `/Encrypt` dictionary builders in `security::standard` consume.
//!
//! The reader and writer types are intentionally separate so the read path
//! cannot accidentally widen its `/P` interpretation when the writer gains
//! new options.

/// Print-permission level (PDF 1.7 §7.6.3.2 Table 22, bits 3 and 12).
///
/// Encoded as two separate bits in `/P`: bit 3 grants any printing, and
/// bit 12 (R≥3 only) grants high-quality printing. The three meaningful
/// combinations are:
///
/// | Variant | Bit 3 | Bit 12 | Reader behavior                          |
/// |---------|-------|--------|------------------------------------------|
/// | `None`  | 0     | 0      | Printing denied                          |
/// | `Low`   | 1     | 0      | Print allowed at degraded resolution     |
/// | `High`  | 1     | 1      | Print allowed at full quality            |
///
/// `(bit3=0, bit12=1)` is ignored by readers (bit 12 only takes effect when
/// bit 3 is also set), so [`PermissionsConfig::from_p_bits`] decodes it as
/// [`PrintPermission::None`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrintPermission {
    /// Printing denied (bit 3 cleared).
    None,
    /// Print allowed at degraded resolution (bit 3 set, bit 12 cleared).
    Low,
    /// Print allowed at full quality (bits 3 and 12 set). The default.
    #[default]
    High,
}

/// Typed `/P` permission configuration for the writer side of the Standard
/// security handler.
///
/// Fields correspond one-to-one to ISO 32000-1 §7.6.3.2 Table 22 bit
/// assignments for revisions ≥ 3 (the only revisions flpdf emits today —
/// V=1/R=2 reuses the same encoding and ignores the R≥3-specific bits).
/// Encode to the `/P` integer via [`Self::to_p_bits`]; decode an
/// already-stored `/P` via [`Self::from_p_bits`].
///
/// # Bit mapping
///
/// | Field             | Bit | Mask     | Spec entry (Table 22)              |
/// |-------------------|-----|----------|------------------------------------|
/// | `print` (Low/High)| 3   | `0x004`  | Print                              |
/// | `modify_contents` | 4   | `0x008`  | Modify document contents           |
/// | `extract`         | 5   | `0x010`  | Copy text and graphics             |
/// | `annotate`        | 6   | `0x020`  | Modify annotations / fill forms    |
/// | `fill_forms`      | 9   | `0x100`  | Fill interactive form fields (R≥3) |
/// | `accessibility`   | 10  | `0x200`  | Extract for accessibility (R≥3)    |
/// | `assemble`        | 11  | `0x400`  | Assemble pages (R≥3)               |
/// | `print` (High)    | 12  | `0x800`  | Print high quality (R≥3)           |
///
/// Bits 1-2 are spec-reserved and must be 0; bits 7-8 and 13-32 are
/// spec-reserved and must be 1 for R≥3. [`Self::to_p_bits`] enforces all
/// three reserved-bit rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionsConfig {
    /// Bit 3 (any print) + bit 12 (high quality print). See [`PrintPermission`].
    pub print: PrintPermission,
    /// Bit 4 — modify document contents by means other than annotation/form edits.
    pub modify_contents: bool,
    /// Bit 5 — copy text and graphics out of the document.
    pub extract: bool,
    /// Bit 6 — add / modify annotations and (for R<3) fill interactive form fields.
    pub annotate: bool,
    /// Bit 9 (R≥3) — fill existing interactive form fields. Distinct from
    /// the broader `annotate` capability above; readers honor both.
    pub fill_forms: bool,
    /// Bit 10 (R≥3) — extract text and graphics for accessibility tools.
    /// Deprecated in PDF 2.0 (accessibility is unconditionally permitted);
    /// emitted for ISO 32000-1 readers' benefit.
    pub accessibility: bool,
    /// Bit 11 (R≥3) — insert, rotate, or delete pages, and create document
    /// outline / thumbnail items.
    pub assemble: bool,
}

impl Default for PermissionsConfig {
    /// All capabilities granted (no restrictions). Encodes to `/P = -4`,
    /// the value qpdf emits when no `--print`/`--modify`/etc. flags are
    /// passed to `--encrypt`, and the value present in this project's
    /// existing encrypted test fixtures.
    fn default() -> Self {
        Self {
            print: PrintPermission::High,
            modify_contents: true,
            extract: true,
            annotate: true,
            fill_forms: true,
            accessibility: true,
            assemble: true,
        }
    }
}

impl PermissionsConfig {
    /// All capabilities denied (most restrictive). Useful as a starting
    /// point for `--encrypt --print=none --modify=none …` style invocations
    /// where the caller will then enable specific bits.
    pub fn none() -> Self {
        Self {
            print: PrintPermission::None,
            modify_contents: false,
            extract: false,
            annotate: false,
            fill_forms: false,
            accessibility: false,
            assemble: false,
        }
    }

    /// Encode this configuration into the signed 32-bit `/P` value per
    /// ISO 32000-1 §7.6.3.2 Table 22 (R≥3 format).
    ///
    /// Bits 1-2 are left at 0; bits 7-8 and bits 13-32 are set to 1 per
    /// the reserved-bit rules. The two-bit print encoding is:
    ///
    /// - `PrintPermission::None`:  bit 3 = 0, bit 12 = 0
    /// - `PrintPermission::Low`:   bit 3 = 1, bit 12 = 0
    /// - `PrintPermission::High`:  bit 3 = 1, bit 12 = 1
    ///
    /// The result is typically negative because the reserved bits 13-32
    /// include the sign bit (bit 31).
    pub fn to_p_bits(&self) -> i32 {
        let mut bits: u32 = 0;

        // Bits 1-2: reserved, must be 0 (already 0).
        // Bit 3 (0x004): any print permission.
        if self.print != PrintPermission::None {
            bits |= 0x004;
        }
        // Bit 4 (0x008): modify_contents.
        if self.modify_contents {
            bits |= 0x008;
        }
        // Bit 5 (0x010): extract.
        if self.extract {
            bits |= 0x010;
        }
        // Bit 6 (0x020): annotate.
        if self.annotate {
            bits |= 0x020;
        }
        // Bits 7-8 (0x040, 0x080): reserved, must be 1 for R≥3.
        bits |= 0x040 | 0x080;
        // Bit 9 (0x100): fill_forms (R≥3).
        if self.fill_forms {
            bits |= 0x100;
        }
        // Bit 10 (0x200): accessibility (R≥3).
        if self.accessibility {
            bits |= 0x200;
        }
        // Bit 11 (0x400): assemble (R≥3).
        if self.assemble {
            bits |= 0x400;
        }
        // Bit 12 (0x800): print_high_quality — only meaningful when bit 3 is set.
        if self.print == PrintPermission::High {
            bits |= 0x800;
        }
        // Bits 13-32: reserved, must be 1 for R≥3.
        bits |= 0xFFFF_F000;

        bits as i32
    }

    /// Decode a signed 32-bit `/P` value back into a typed
    /// [`PermissionsConfig`]. Reserved bits (1-2, 7-8, 13-32) are ignored,
    /// so `from_p_bits(p).to_p_bits()` will normalize a partially-conforming
    /// `/P` to the canonical R≥3 form.
    ///
    /// `(bit 3 = 0, bit 12 = 1)` decodes as [`PrintPermission::None`]
    /// (high-quality print requires any-print to take effect per spec).
    pub fn from_p_bits(bits: i32) -> Self {
        let bits = bits as u32;
        let any_print = bits & 0x004 != 0;
        let high_print = bits & 0x800 != 0;
        let print = if !any_print {
            PrintPermission::None
        } else if high_print {
            PrintPermission::High
        } else {
            PrintPermission::Low
        };
        Self {
            print,
            modify_contents: bits & 0x008 != 0,
            extract: bits & 0x010 != 0,
            annotate: bits & 0x020 != 0,
            fill_forms: bits & 0x100 != 0,
            accessibility: bits & 0x200 != 0,
            assemble: bits & 0x400 != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PermissionsConfig::default()` (all granted) must encode to the
    /// canonical "all permissions" value `/P = -4` that qpdf emits and that
    /// this project's existing encrypted fixtures carry.
    #[test]
    fn default_config_encodes_to_minus_four() {
        assert_eq!(PermissionsConfig::default().to_p_bits(), -4);
    }

    /// And the inverse: `/P = -4` (the qpdf-canonical "all permissions"
    /// value that every fixture in `tests/fixtures/encrypted/` carries)
    /// must decode back to `PermissionsConfig::default()`. Implied by the
    /// 128-case round-trip below, asserted explicitly here to make the
    /// qpdf-parity guarantee visible at the named entry point.
    #[test]
    fn from_p_bits_decodes_qpdf_default_minus_four_to_default() {
        assert_eq!(
            PermissionsConfig::from_p_bits(-4),
            PermissionsConfig::default()
        );
    }

    /// `PermissionsConfig::none()` (all denied) leaves only the reserved
    /// bits set — bits 7-8 (= 0x0C0) and bits 13-32 (= 0xFFFF_F000). The
    /// combined u32 is `0xFFFF_F0C0`, which as i32 is `-3904`.
    #[test]
    fn none_config_encodes_only_reserved_bits() {
        let bits = PermissionsConfig::none().to_p_bits();
        assert_eq!(bits as u32, 0xFFFF_F0C0);
        assert_eq!(bits, -3904);
    }

    /// Round-trip every combination of boolean fields × every print level.
    /// 128 cases (2^7 booleans × 3 print levels) — exhaustive for the
    /// configuration space, catches any encode/decode asymmetry.
    #[test]
    fn round_trip_encode_decode_for_all_combinations() {
        let prints = [
            PrintPermission::None,
            PrintPermission::Low,
            PrintPermission::High,
        ];
        for &print in &prints {
            for modify_contents in [false, true] {
                for extract in [false, true] {
                    for annotate in [false, true] {
                        for fill_forms in [false, true] {
                            for accessibility in [false, true] {
                                for assemble in [false, true] {
                                    let original = PermissionsConfig {
                                        print,
                                        modify_contents,
                                        extract,
                                        annotate,
                                        fill_forms,
                                        accessibility,
                                        assemble,
                                    };
                                    let bits = original.to_p_bits();
                                    let decoded = PermissionsConfig::from_p_bits(bits);
                                    assert_eq!(
                                        decoded, original,
                                        "round-trip failed for {original:?} (bits = {bits} / 0x{:08X})",
                                        bits as u32,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// `from_p_bits` must ignore reserved bits (1-2, 7-8, 13-32): two
    /// inputs that differ only in reserved-bit values must decode to the
    /// same [`PermissionsConfig`]. Verified by toggling each reserved-bit
    /// mask against the canonical "all denied" value.
    #[test]
    fn from_p_bits_ignores_reserved_bit_variations() {
        let baseline = PermissionsConfig::from_p_bits(-3904); // 0xFFFF_F0C0 (canonical none)
        let no_reserved = PermissionsConfig::from_p_bits(0); // all bits 0
        let only_bit1 = PermissionsConfig::from_p_bits(0x0000_0001);
        let only_bit2 = PermissionsConfig::from_p_bits(0x0000_0002);
        let only_reserved_hi = PermissionsConfig::from_p_bits(0xFFFF_F000u32 as i32);

        assert_eq!(baseline, no_reserved);
        assert_eq!(baseline, only_bit1);
        assert_eq!(baseline, only_bit2);
        assert_eq!(baseline, only_reserved_hi);
    }

    /// `(bit 3 = 0, bit 12 = 1)` decodes as `PrintPermission::None` per
    /// the spec's "bit 12 only takes effect when bit 3 is set" rule.
    /// Re-encoding then strips bit 12 — verifying that `to_p_bits` does
    /// not surprise callers with stale bits from a hand-crafted input.
    #[test]
    fn decode_of_high_bit_without_any_print_falls_back_to_none() {
        let bits_with_only_high = 0x800_u32 as i32; // bit 12 set, bit 3 cleared
        let decoded = PermissionsConfig::from_p_bits(bits_with_only_high);
        assert_eq!(decoded.print, PrintPermission::None);

        let re_encoded = decoded.to_p_bits();
        assert_eq!(
            re_encoded as u32 & 0x800,
            0,
            "re-encoded /P must not carry a stale bit 12 when print=None"
        );
    }

    /// PDF 1.7 §7.6.3.2 reserved-bit guarantees: every encoded `/P` from
    /// `to_p_bits` must have bits 1-2 cleared, bits 7-8 set, and bits 13-32
    /// set, regardless of which capability bits the config grants.
    #[test]
    fn to_p_bits_enforces_reserved_bit_invariants() {
        // Cover both ends of the spectrum to make sure the invariants hold
        // regardless of capability bits.
        for config in [PermissionsConfig::default(), PermissionsConfig::none()] {
            let bits = config.to_p_bits() as u32;
            assert_eq!(
                bits & 0x003,
                0,
                "bits 1-2 must be 0 (config = {config:?}, bits = 0x{bits:08X})"
            );
            assert_eq!(
                bits & 0x0C0,
                0x0C0,
                "bits 7-8 must be 1 (config = {config:?}, bits = 0x{bits:08X})"
            );
            assert_eq!(
                bits & 0xFFFF_F000,
                0xFFFF_F000,
                "bits 13-32 must be 1 (config = {config:?}, bits = 0x{bits:08X})"
            );
        }
    }

    /// PrintPermission encoding regression: each variant maps to a
    /// well-defined `/P` bit pattern. Pin the three known-good values so
    /// any later change to `to_p_bits` is caught.
    #[test]
    fn print_permission_encodes_bits_3_and_12_correctly() {
        let none = PermissionsConfig {
            print: PrintPermission::None,
            ..PermissionsConfig::none()
        };
        let low = PermissionsConfig {
            print: PrintPermission::Low,
            ..PermissionsConfig::none()
        };
        let high = PermissionsConfig {
            print: PrintPermission::High,
            ..PermissionsConfig::none()
        };
        // none: bits 3 and 12 both clear.
        assert_eq!(none.to_p_bits() as u32 & (0x004 | 0x800), 0);
        // low: bit 3 set, bit 12 clear.
        assert_eq!(low.to_p_bits() as u32 & (0x004 | 0x800), 0x004);
        // high: both bits set.
        assert_eq!(high.to_p_bits() as u32 & (0x004 | 0x800), 0x004 | 0x800);
    }

    /// PrintPermission::default() must be `High` so `PermissionsConfig::default()`
    /// (all granted) actually enables high-quality printing.
    #[test]
    fn print_permission_default_is_high() {
        assert_eq!(PrintPermission::default(), PrintPermission::High);
    }
}
