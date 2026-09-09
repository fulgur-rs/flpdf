//! qpdf correspondence: QPDFObjGen.hh/QPDF.cc raw xref identity and valid indirect-reference boundary.
//!
//! This is intentionally separate from [`crate::ObjectRef`]. qpdf keeps the
//! signed integer object/generation pair while reading xref rows
//! (`include/qpdf/QPDFObjGen.hh:29-86`), then applies the stricter indirect
//! reference boundary while parsing `N G R`
//! (`libqpdf/QPDFParser.cc:157-178`).

use crate::ObjectRef;

/// Raw qpdf object/generation identity for xref registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct QpdfObjGen {
    object: i32,
    generation: i32,
}

impl QpdfObjGen {
    /// Construct qpdf's signed object/generation pair.
    pub(crate) const fn new(object: i32, generation: i32) -> Self {
        Self { object, generation }
    }

    /// Convert a Rust object reference through qpdf's checked `int` boundary
    /// (`QIntC::to_int`, `include/qpdf/QPDF.hh:1429-1444`).
    pub(crate) fn try_from_object_ref(object_ref: ObjectRef) -> crate::Result<Self> {
        let object = i32::try_from(object_ref.number).map_err(|_| {
            crate::Error::System(format!(
                "integer out of range converting {} from a 4-byte unsigned type to a 4-byte signed type",
                object_ref.number
            ))
        })?;
        Ok(Self::new(object, i32::from(object_ref.generation)))
    }

    /// Match `QPDFObjGen::isIndirect`: only object number zero is non-indirect.
    pub(crate) const fn is_indirect(self) -> bool {
        self.object != 0
    }

    /// Return qpdf's object number (`QPDFObjGen::getObj`).
    pub(crate) const fn get_obj(self) -> i32 {
        self.object
    }

    /// Return qpdf's generation (`QPDFObjGen::getGen`).
    pub(crate) const fn get_gen(self) -> i32 {
        self.generation
    }

    /// Convert only a valid parsed indirect reference to flpdf's `ObjectRef`.
    ///
    /// qpdf's parser rejects object numbers below one and generations outside
    /// `0..65535` when it sees an indirect `N G R` reference. Raw xref rows are
    /// allowed to exist outside that boundary until this conversion point.
    pub(crate) fn to_object_ref(self) -> Option<ObjectRef> {
        if !self.is_indirect() || self.get_obj() < 1 || !(0..65_535).contains(&self.get_gen()) {
            return None;
        }
        Some(ObjectRef::new(
            u32::try_from(self.object).ok()?,
            u16::try_from(self.generation).ok()?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::QpdfObjGen;
    use crate::ObjectRef;

    #[test]
    fn qpdf_obj_gen_uses_qpdf_indirect_and_reference_boundaries() {
        let free = QpdfObjGen::new(0, 65_536);
        assert_eq!(free.get_obj(), 0);
        assert_eq!(free.get_gen(), 65_536);
        assert!(!free.is_indirect());
        assert_eq!(
            QpdfObjGen::new(7, 0).to_object_ref(),
            Some(ObjectRef::new(7, 0))
        );
        assert_eq!(
            QpdfObjGen::new(7, 65_534).to_object_ref(),
            Some(ObjectRef::new(7, 65_534))
        );
        assert_eq!(QpdfObjGen::new(7, 65_535).to_object_ref(), None);
        assert_eq!(QpdfObjGen::new(0, 65_536).to_object_ref(), None);
    }

    #[test]
    fn qpdf_obj_gen_orders_object_number_before_generation() {
        assert!(QpdfObjGen::new(7, 99) < QpdfObjGen::new(8, 0));
        assert!(QpdfObjGen::new(7, 0) < QpdfObjGen::new(7, 1));
    }

    #[test]
    fn object_ref_conversion_rejects_qpdf_signed_integer_overflow() {
        let error = QpdfObjGen::try_from_object_ref(ObjectRef::new(
            u32::try_from(i64::from(i32::MAX) + 1).unwrap(),
            0,
        ))
        .expect_err("qpdf QIntC::to_int must reject object numbers above INT_MAX");
        assert!(error
            .to_string()
            .contains("integer out of range converting"));
    }
}
