use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{Read, Write},
    net::TcpListener,
};

fn frame(input: &mut impl Read) -> Vec<u8> {
    let mut length = [0; 8];
    input.read_exact(&mut length).unwrap();
    let length = usize::try_from(u64::from_le_bytes(length)).unwrap();
    assert!(length <= 64 << 10);
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes).unwrap();
    bytes
}

fn blob(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&u64::try_from(bytes.len()).unwrap().to_le_bytes());
    output.extend_from_slice(bytes);
}

struct Facts<'a>(&'a [u8]);
impl<'a> Facts<'a> {
    fn take(&mut self, length: usize) -> &'a [u8] {
        let (head, tail) = self.0.split_at(length);
        self.0 = tail;
        head
    }
    fn number(&mut self) -> usize {
        usize::try_from(u64::from_le_bytes(self.take(8).try_into().unwrap())).unwrap()
    }
    fn text(&mut self) {
        let length = self.number();
        std::str::from_utf8(self.take(length)).unwrap();
    }
    fn check(mut self) {
        assert_eq!(self.take(4), b"YCF1");
        let flags = self.take(4);
        assert!(flags[0] <= 1 && flags[1] <= 2 && flags[2..] == [0, 0]);
        for _ in 0..3 {
            let count = self.number();
            assert!(count <= self.0.len() / 24);
            for _ in 0..count {
                self.take(16);
                self.text();
            }
        }
        let headers = self.number();
        assert!(headers <= self.0.len() / 24);
        for _ in 0..headers {
            self.take(16);
            let length = self.number();
            assert!(length <= self.0.len() / 8);
            self.take(length * 8);
        }
        let order = self.number();
        assert!(order <= self.0.len() / 16);
        self.take(order * 16);
        let patterns = self.number();
        assert!(patterns <= self.0.len() / 24);
        for _ in 0..patterns {
            self.take(16);
            self.text();
        }
        assert!(self.0.is_empty());
    }
}

fn main() {
    assert_eq!(
        std::env::args().skip(1).collect::<Vec<_>>(),
        ["--internal-viewer-analysis"]
    );
    let mut input = std::io::stdin().lock();
    let metadata = frame(&mut input);
    let metadata = std::str::from_utf8(&metadata).unwrap();
    let mut fields = BTreeMap::new();
    for field in metadata
        .strip_prefix('{')
        .unwrap()
        .strip_suffix('}')
        .unwrap()
        .split(',')
    {
        let (name, value) = field.split_once(':').unwrap();
        assert!(fields.insert(name.trim(), value.trim()).is_none());
    }
    assert_eq!(
        fields,
        BTreeMap::from([
            ("\"op\"", "\"form_xml\""),
            ("\"platform\"", "\"8.5.1.1529\""),
            ("\"snapshot\"", "0"),
            ("\"form_ordinal\"", "0"),
            ("\"owner\"", "\"ExternalDataProcessor.ЙодЁлка\""),
            ("\"shared\"", "true"),
        ])
    );
    let source = frame(&mut input);
    let source = source.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&source);
    let source = std::str::from_utf8(source).unwrap().trim();
    assert!(source.starts_with('{') && source.ends_with('}'));
    let facts = frame(&mut input);
    Facts(&facts).check();
    assert_eq!(input.read(&mut [0]).unwrap(), 0);

    let executable = std::env::current_exe().unwrap();
    let directory = executable.parent().unwrap();
    let deferred = match directory.file_name().unwrap().to_str().unwrap() {
        "complete" => false,
        "deferred" => true,
        other => panic!("unexpected form probe mode: {other}"),
    };
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let mut receipt = options
        .open(directory.join(format!("receipt-{}", std::process::id())))
        .unwrap();
    writeln!(receipt, "{}", std::process::id()).unwrap();
    writeln!(receipt, "{}", listener.local_addr().unwrap()).unwrap();
    receipt.flush().unwrap();

    let mut body = b"YFR1".to_vec();
    body.extend_from_slice(&[0, 2, u8::from(deferred)]);
    blob(
        &mut body,
        b"\xef\xbb\xbf<Form xmlns=\"http://v8.1c.ru/8.3/xcf/logform\"/>\r\n",
    );
    body.extend_from_slice(&1u64.to_le_bytes());
    blob(&mut body, "Items/йё.bin".as_bytes());
    blob(&mut body, &[0, 255, 42]);
    let mut output = std::io::stdout().lock();
    output
        .write_all(&(body.len() as u64).to_le_bytes())
        .unwrap();
    output.write_all(&body).unwrap();
    output.flush().unwrap();
    std::process::exit(0);
}
