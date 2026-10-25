use std::io::Write;

pub struct Out<'a> {
    pub w: &'a mut dyn Write,
    pub closed: bool,
}

impl Out<'_> {
    fn note<T>(&mut self, r: std::io::Result<T>) -> std::io::Result<T> {
        if let Err(e) = &r
            && e.kind() == std::io::ErrorKind::BrokenPipe
        {
            self.closed = true;
        }
        r
    }
}

impl Write for Out<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let r = self.w.write(buf);
        self.note(r)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let r = self.w.flush();
        self.note(r)
    }
}
