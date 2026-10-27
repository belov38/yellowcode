use md_model::parts::Picture;
use memory_budget::{BudgetError, BudgetedText, Category, Charged, MemoryBudget};
use std::fmt::Write;

pub fn managed_picture_xml(
    budget: &MemoryBudget,
    picture: &Picture,
    limit: u64,
) -> Result<Charged<String>, BudgetError> {
    let mut out = BudgetedText::new(budget, Category::Metadata, limit)?;
    let written = write!(
        out,
        "{}\r\n<ExtPicture xmlns=\"http://v8.1c.ru/8.3/xcf/extrnprops\" xmlns:xr=\"http://v8.1c.ru/8.3/xcf/readable\" \
         xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
         version=\"{}\">\r\n\t<Picture>\r\n\t\t<xr:Abs>Picture.{}</xr:Abs>\r\n\
         \t\t<xr:LoadTransparent>{}</xr:LoadTransparent>",
        crate::codec::parts::XML_HEAD,
        crate::DUMP_VERSION,
        picture.ext,
        picture.load_transparent,
    );
    if written.is_err() {
        return Err(out.error().copied().unwrap_or(BudgetError::Overflow));
    }
    if let Some((x, y)) = picture.transparent_pixel
        && write!(out, "\r\n\t\t<xr:TransparentPixel x=\"{x}\" y=\"{y}\"/>").is_err()
    {
        return Err(out.error().copied().unwrap_or(BudgetError::Overflow));
    }
    out.try_push_str("\r\n\t</Picture>\r\n</ExtPicture>")?;
    out.into_string()
}
