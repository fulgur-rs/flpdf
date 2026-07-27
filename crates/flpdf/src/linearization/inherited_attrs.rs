use std::io::{Read, Seek};

use crate::{Pdf, Result};

pub(crate) fn push_inherited_attributes_to_pages<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<()> {
    let Some(prepared) = crate::pages::repair::prepare_for_optimization(pdf)? else {
        return Ok(());
    };
    crate::optimization::inherited_attrs::push_inherited_attributes_to_pages(
        pdf, &prepared, true, false,
    )
}
