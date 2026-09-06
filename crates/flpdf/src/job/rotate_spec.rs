//! qpdf correspondence: `QPDFJob::parseRotationParameter`.
//! (`libqpdf/QPDFJob.cc:369-415`) and its private `RotationSpec` state.
//!
//! Rotation parsing keeps qpdf's raw range bytes separate from the angle and
//! relative flag. Range syntax is owned by `QUtil::parse_numrange`, so the JSON
//! and direct CLI rotation routes cannot drift into separate page-range ASTs.

use crate::qutil::parse_numrange;
use crate::{Error, Result};

/// qpdf's private `QPDFJob::RotationSpec` (`include/qpdf/QPDFJob.hh:426-435`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationSpec {
    /// Rotation angle in degrees; `-` parameters retain a negative angle.
    pub angle: i32,
    /// Whether qpdf adds the angle to the inherited page rotation.
    pub relative: bool,
}

/// Parsed `QPDFJob::parseRotationParameter` result before map insertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationParameter {
    /// qpdf's raw range map key; defaulted to `1-z` when omitted or empty.
    pub range: Vec<u8>,
    /// The qpdf rotation state stored under `range`.
    pub spec: RotationSpec,
}

/// Parse one qpdf rotation parameter.
///
/// This is the shared parser for job JSON and direct CLI rotation. The range
/// is validated with `QUtil::parse_numrange(..., 0)` exactly as qpdf does;
/// resolution against the real page count belongs to each consumer's
/// `handleRotations` boundary.
pub fn parse_rotation_parameter(parameter: &[u8]) -> Result<RotationParameter> {
    let Some(colon) = parameter.iter().position(|&byte| byte == b':') else {
        return parse_rotation_parts(parameter, parameter, None);
    };
    let angle = &parameter[..colon];
    let range = if colon + 1 < parameter.len() {
        &parameter[colon + 1..]
    } else {
        &[]
    };
    parse_rotation_parts(parameter, angle, Some(range))
}

fn parse_rotation_parts(
    parameter: &[u8],
    angle_bytes: &[u8],
    range_bytes: Option<&[u8]>,
) -> Result<RotationParameter> {
    let mut angle = angle_bytes;
    let mut relative = false;
    if let Some((&sign, rest)) = angle.split_first() {
        if sign == b'+' || sign == b'-' {
            relative = true;
            angle = rest;
        } else if !sign.is_ascii_digit() {
            angle = &[];
        }
    }
    let negative = angle_bytes.first() == Some(&b'-');
    let range = match range_bytes {
        Some(bytes) if !bytes.is_empty() => bytes.to_vec(),
        _ => b"1-z".to_vec(),
    };
    if parse_numrange(&range, 0).is_err() {
        return Err(invalid_rotation_parameter(parameter));
    }
    let angle_value = match angle {
        b"0" => 0,
        b"90" => 90,
        b"180" => 180,
        b"270" => 270,
        _ => return Err(invalid_rotation_parameter(parameter)),
    };
    let angle = if negative { -angle_value } else { angle_value };
    Ok(RotationParameter {
        range,
        spec: RotationSpec { angle, relative },
    })
}

fn invalid_rotation_parameter(parameter: &[u8]) -> Error {
    let mut message = b"invalid parameter to rotate: ".to_vec();
    message.extend_from_slice(parameter);
    Error::Usage(crate::UsageError::new(message))
}

#[cfg(test)]
mod tests {
    use super::{parse_rotation_parameter, RotationSpec};
    use crate::Error;

    #[test]
    fn parse_rotation_parameter_keeps_qpdf_angle_relative_and_raw_range() {
        let parsed = parse_rotation_parameter(b"-90:1-5,x3").unwrap();
        assert_eq!(parsed.range, b"1-5,x3");
        assert_eq!(
            parsed.spec,
            RotationSpec {
                angle: -90,
                relative: true
            }
        );
    }

    #[test]
    fn parse_rotation_parameter_defaults_missing_and_trailing_ranges() {
        assert_eq!(
            parse_rotation_parameter(b"90").unwrap().range,
            b"1-z".to_vec()
        );
        assert_eq!(
            parse_rotation_parameter(b"+180:").unwrap().range,
            b"1-z".to_vec()
        );
    }

    #[test]
    fn parse_rotation_parameter_preserves_relative_zero() {
        assert_eq!(
            parse_rotation_parameter(b"-0:1").unwrap().spec,
            RotationSpec {
                angle: 0,
                relative: true
            }
        );
    }

    #[test]
    fn parse_rotation_parameter_rejects_invalid_angle_or_range() {
        assert!(parse_rotation_parameter(b"45").is_err());
        assert!(parse_rotation_parameter(b"90:1-").is_err());
        assert!(parse_rotation_parameter(b"x90").is_err());
    }

    #[test]
    fn parse_rotation_parameter_accepts_angle_270_after_range_validation() {
        assert_eq!(parse_rotation_parameter(b"270:1").unwrap().spec.angle, 270);
    }

    #[test]
    fn parse_rotation_parameter_keeps_invalid_bytes_in_error() {
        let error = parse_rotation_parameter(b"90:1-\xff").unwrap_err();
        assert!(matches!(error, Error::Usage(_)));
        assert_eq!(
            error.raw_message(),
            Some(b"invalid parameter to rotate: 90:1-\xff".as_slice())
        );
    }

    #[test]
    fn parse_rotation_parameter_keeps_direct_nul_bytes_until_qutil() {
        let error = parse_rotation_parameter(b"90\0junk").unwrap_err();
        assert!(matches!(error, Error::Usage(_)));
        assert_eq!(
            error.raw_message(),
            Some(b"invalid parameter to rotate: 90".as_slice())
        );
        let parsed = parse_rotation_parameter(b"90:1\0junk").unwrap();
        assert_eq!(parsed.range, b"1\0junk");
    }
}
