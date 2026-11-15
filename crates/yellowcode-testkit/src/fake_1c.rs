use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

#[derive(Debug, Clone, Default)]
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(&'static str, &'static str)>,
    pub body: Vec<u8>,
}

pub fn serve(answer: impl Fn(&str) -> Answer + Send + 'static) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let mut r = BufReader::new(stream.try_clone().expect("clone"));
            let mut w = stream;
            loop {
                let mut start = String::new();
                if r.read_line(&mut start).unwrap_or(0) == 0 {
                    break;
                }
                let mut len = 0usize;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':')
                        && k.eq_ignore_ascii_case("content-length")
                    {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; len];
                let _ = r.read_exact(&mut body);
                let path = start.split_whitespace().nth(1).unwrap_or("");
                let a = answer(path);
                let mut head = format!(
                    "HTTP/1.1 {} X\r\nConnection: Keep-Alive\r\nContent-Length: {}\r\n",
                    a.status,
                    a.body.len()
                );
                for (k, v) in &a.headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                head.push_str("\r\n");
                let _ = w.write_all(head.as_bytes());
                let _ = w.write_all(&a.body);
            }
        }
    });
    port
}

pub fn serve_static(answer: fn(&str) -> (u16, &'static [u8])) -> u16 {
    serve(move |path| {
        let (status, body) = answer(path);
        Answer {
            status,
            headers: vec![("vrs-session", "dnJz")],
            body: body.to_vec(),
        }
    })
}

macro_rules! fx {
    ($dir:literal, $name:literal) => {
        include_bytes!(concat!(
            "../../onec-client/tests/fixtures/",
            $dir,
            "/",
            $name
        ))
        .as_slice()
    };
}

pub fn anonymous(path: &str) -> (u16, &'static [u8]) {
    let p = path.split('?').next().unwrap_or("");
    match p {
        "/e1cib/login" => (200, fx!("info-recon", "login.anon.xml")),
        "/e1cib/types" => (200, fx!("info-recon", "types.anon.xml")),
        "/e1cib/misc" => (200, fx!("info-recon", "startInfo.anon.xml")),
        "/e1cib/modules/defs" if path.contains("&id=") => {
            (200, fx!("info-recon", "modules_defs_id.anon.xml"))
        }
        "/e1cib/modules/defs" => (200, fx!("info-recon", "modules_defs.anon.xml")),
        "/e1cib/about" => (200, fx!("info-recon", "about.anon.xml")),
        "/e1cib/additionalAuthSettings" => (200, fx!("auth-recon", "additionalAuthSettings.json")),
        "/e1cib/cmi/cmiStartData" => (200, fx!("info-recon", "cmiStartData.anon.xml")),
        "/e1cib/logout" => (200, b""),
        _ => (404, b""),
    }
}

pub fn metadata(path: &str) -> (u16, &'static [u8]) {
    const LOGIN: &[u8] = br#"<response xmlns="http://v8.1c.ru/8.2/mngsrv/ws" xmlns:app="http://v8.1c.ru/8.2/managed-application/core"><app:config>a089c552b8b33b48b1c61dfb427e6a8d00000000</app:config><app:types>ead894c1823387428e82740b895be334</app:types><app:rolesID>0</app:rolesID></response>"#;
    let p = path.split('?').next().unwrap_or("");
    match p {
        "/e1cib/login" => (200, LOGIN),
        "/e1cib/types" => (200, fx!("md-recon", "types.anon.xml")),
        "/e1cib/types/view" => (200, fx!("md-recon", "types_view.anon.xml")),
        "/e1cib/cmi/cmiStartData" => (200, fx!("md-recon", "cmiStartData.anon.xml")),
        "/e1cib/cmi/commands" => (200, fx!("md-recon", "commands_subtree.anon.xml")),
        _ => anonymous(path),
    }
}
