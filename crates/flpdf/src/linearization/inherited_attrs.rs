use std::io::{Read, Seek};

use crate::{Pdf, Result};

pub(crate) fn push_inherited_attributes_to_pages<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<()> {
    crate::pages::repair::push_inherited_attributes_to_pages(pdf)
}
