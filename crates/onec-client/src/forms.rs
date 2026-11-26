use crate::answer::NS_MODULES;
use crate::body;
use crate::session::{RandomSource, Session, SessionError};
use crate::xml::escape_text;

pub fn close_request(fid: &str) -> Vec<u8> {
    format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?><request xmlns=\"http://v8.1c.ru/8.2/mngsrv/ws\" \
xmlns:cfg=\"http://v8.1c.ru/8.1/data/enterprise/current-config\" xmlns:v8=\"http://v8.1c.ru/8.1/data/core\" \
xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\
<id xmlns=\"{NS_MODULES}\">{}</id></request>",
        escape_text(fid)
    )
    .into_bytes()
}

impl<R: RandomSource> Session<R> {
    pub(crate) fn close_form(&mut self, fid: &str) -> Result<(), SessionError> {
        let req = body::zstd_compress(&close_request(fid));
        let h = self.headers("application/xml", true, true);
        self.step(
            "logForm/processFormsClose",
            "POST",
            "/e1cib/logForm?cmd=processFormsClose",
            &h,
            &req,
            &[200],
        )?;
        Ok(())
    }
}
