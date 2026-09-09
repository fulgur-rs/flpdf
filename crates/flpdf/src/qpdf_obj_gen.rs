//! qpdf's raw `QPDFObjGen` identity used while loading cross-reference data.
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
        if self.object < 1 || !(0..65_535).contains(&self.generation) {
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
}
