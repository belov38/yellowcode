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
        "/e1cib/cmi/commands" if path.contains("categories=4") => (204, b""),
        "/e1cib/cmi/commands" => (200, fx!("md-recon", "commands_subtree.anon.xml")),
        "/e1cib/logForm" if path.contains("cmd=dataDescrWMethods") => {
            const FORMS: &[(&str, &[u8])] = &[
                (
                    "formId=urn:form:md:0:907e129b-76ad-573c-b2d1-020ec1b3f493&",
                    fx!("forms-recon", "descr_catalog.anon.xml"),
                ),
                (
                    "formId=urn:form:md:0:29dda46a-c782-5121-8767-383920ca9e10&",
                    fx!("forms-recon", "descr_document.anon.xml"),
                ),
                (
                    "formId=urn:form:md:3:101308b8-e8d5-5aba-b88f-1083cce12a34&",
                    fx!("forms-recon", "descr_inforeg_record.anon.xml"),
                ),
            ];
            FORMS.iter().find(|(id, _)| path.contains(id)).map_or(
                (400, fx!("forms-recon", "descr_unknown.400.xml")),
                |(_, b)| (200, b),
            )
        }
        _ => anonymous(path),
    }
}

const GLUE_REFS: &[u8] = "<response xmlns=\"http://v8.1c.ru/8.2/mngsrv/ws\"><refs>\
<item typeId=\"ac1f5d81-bfa8-535b-92cb-3f2d8df6ee8e\"><ref id=\"773bd010-e89b-11f1-a6fc-00090faa0001\" valid=\"true\">Клей</ref></item>\
</refs></response>"
    .as_bytes();

const DOC_REFS: &[u8] = "<response xmlns=\"http://v8.1c.ru/8.2/mngsrv/ws\"><refs>\
<item typeId=\"823bd9bd-c8c1-5684-a22b-1fc207056319\"><ref id=\"7ec35a2c-e89b-11f1-a6fc-00090faa0001\" valid=\"true\">Приходная накладная ПФ-1 от 01.11.2026 1:00:00</ref></item>\
<item typeId=\"ac1f5d81-bfa8-535b-92cb-3f2d8df6ee8e\"><ref id=\"773bd010-e89b-11f1-a6fc-00090faa0001\" valid=\"true\">Клей</ref></item>\
</refs></response>"
    .as_bytes();

fn object_with(path: &str, query: &'static [u8], refs: &'static [u8]) -> (u16, &'static [u8]) {
    const LOGIN: &[u8] = br#"<response xmlns="http://v8.1c.ru/8.2/mngsrv/ws" xmlns:app="http://v8.1c.ru/8.2/managed-application/core"><app:config>f5a2a218748dff4da1889d34ee683a0400000000</app:config><app:types>ead894c1823387428e82740b895be334</app:types><app:rolesID>0</app:rolesID></response>"#;
    let p = path.split('?').next().unwrap_or("");
    match p {
        "/e1cib/login" => (200, LOGIN),
        "/e1cib/types" => (200, fx!("object-recon", "types.xml")),
        "/e1cib/types/view/ebea5293-0eb5-5a14-b9ae-1694a21167f2/values" => {
            (200, fx!("enum-recon", "a_kinds.resp.xml"))
        }
        "/e1cib/misc" if path.contains("cmd=refs") => (200, refs),
        "/e1cib/logForm" if path.contains("cmd=query") => (200, query),
        "/e1cib/logForm" if path.contains("cmd=processFormsClose") => (200, b""),
        "/e1cib/logForm" if path.contains("cmd=dataDescrWMethods") => {
            if path.contains("formId=urn:form:md:0:907e129b-76ad-573c-b2d1-020ec1b3f493&") {
                (200, fx!("object-recon", "descr_catalog.xml"))
            } else if path.contains("formId=urn:form:md:0:29dda46a-c782-5121-8767-383920ca9e10&") {
                (200, fx!("object-recon", "descr_document.xml"))
            } else {
                (400, fx!("forms-recon", "descr_unknown.400.xml"))
            }
        }
        _ => anonymous(path),
    }
}

pub fn object_catalog(path: &str) -> (u16, &'static [u8]) {
    object_with(path, fx!("object-recon", "query_catalog.xml"), GLUE_REFS)
}

pub fn object_document(path: &str) -> (u16, &'static [u8]) {
    object_with(path, fx!("object-recon", "query_document.xml"), DOC_REFS)
}

pub fn object_missing(path: &str) -> (u16, &'static [u8]) {
    if path.contains("cmd=query") {
        return (400, fx!("object-recon", "query_missing.400.xml"));
    }
    object_catalog(path)
}

pub fn object_catalog_reader(path: &str) -> (u16, &'static [u8]) {
    object_with(
        path,
        fx!("object-recon", "query_catalog.reader.xml"),
        GLUE_REFS,
    )
}

pub fn list(path: &str) -> (u16, &'static [u8]) {
    let p = path.split('?').next().unwrap_or("");
    match p {
        "/e1cib/types" => (200, fx!("dlist-recon", "types.f1.xml")),
        "/e1cib/logForm" if path.contains("cmd=query") => {
            (200, fx!("dlist-recon", "catalog_query.xml"))
        }
        "/e1cib/logForm" if path.contains("cmd=elements") => {
            (200, fx!("dlist-recon", "catalog_elements.xml"))
        }
        "/e1cib/logForm" if path.contains("cmd=processFormsClose") => (200, b""),
        "/e1cib/dlist" => (200, fx!("dlist-recon", "catalog_far.xml")),
        p if p.starts_with("/e1cib/types/view/") => (200, fx!("dlist-recon", "enum_values.xml")),
        _ => anonymous(path),
    }
}

pub fn report(path: &str) -> (u16, &'static [u8]) {
    let p = path.split('?').next().unwrap_or("");
    match p {
        "/e1cib/logForm" if path.contains("cmd=query") => (200, fx!("report-recon", "query.xml")),
        "/e1cib/logForm" if path.contains("cmd=dataDescrWMethods") => {
            (200, fx!("report-recon", "descr.xml"))
        }
        "/e1cib/logForm" if path.contains("cmd=call") => {
            (200, fx!("report-recon", "call_without_content.xml"))
        }
        "/e1cib/logForm" if path.contains("cmd=processFormsClose") => (200, b""),
        "/e1cib/moxel" if path.contains("cmd=getRowContents") => {
            (200, fx!("report-recon", "rows.xml"))
        }
        _ => metadata(path),
    }
}
