use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use brace::{Guid, Node};

use super::call::{self, ClientInfo, MANAGER_CONTEXT};
use super::keyx::{self, ClientKey};
use super::stream::MessageStream;
use super::{IFACE_REGMNGR, IFACE_VRS, NULL_GUID, NativeError, frame, ntlm};
use crate::session::{SessionError, VERSION};
use crate::transport::{MAX_RESPONSE_BODY, Response};
use crate::wire::Wire;

pub const DEFAULT_PORT: u16 = 1541;
const TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTarget {
    pub host: String,
    pub port: u16,
    pub ib: String,
}

impl NativeTarget {
    pub fn is_native(address: &str) -> bool {
        let a = address.trim_start();
        let lower = a.to_ascii_lowercase();
        lower.starts_with("tcp://")
            || (!lower.starts_with("http://")
                && !lower.starts_with("https://")
                && lower
                    .split(';')
                    .any(|f| f.split_once('=').is_some_and(|(k, _)| k.trim() == "srvr")))
    }

    pub fn parse(address: &str) -> Result<Self, String> {
        let a = address.trim();
        if a.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("tcp://")) {
            let rest = &a[6..];
            let (server, ib) = rest
                .split_once('/')
                .ok_or_else(|| format!("{address:?}: expected tcp://host[:port]/infobase"))?;
            let ib = ib.trim_end_matches('/');
            return Self::from_parts(address, server, ib);
        }
        let fields = connection_string(a).map_err(|e| format!("{address:?}: {e}"))?;
        let get = |key: &str| {
            fields
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v.as_str())
        };
        let server = get("Srvr").ok_or_else(|| format!("{address:?}: no Srvr"))?;
        let ib = get("Ref").ok_or_else(|| format!("{address:?}: no Ref"))?;
        Self::from_parts(address, server, ib)
    }

    fn from_parts(address: &str, server: &str, ib: &str) -> Result<Self, String> {
        if server.contains(',') {
            return Err(format!(
                "{address:?}: several servers in one address are not supported"
            ));
        }
        if ib.is_empty() {
            return Err(format!("{address:?}: no infobase name"));
        }
        let (host, port) = split_host_port(server)
            .ok_or_else(|| format!("{address:?}: bad server {server:?}, expected host[:port]"))?;
        Ok(Self {
            host,
            port,
            ib: ib.to_string(),
        })
    }

    pub fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

fn initial_seq(r: u16) -> u32 {
    1000 + u32::from(r) % 29_000
}

fn server_build_of(message: &str) -> Option<String> {
    message.match_indices('(').find_map(|(i, _)| {
        let inner = message[i + 1..].split(')').next()?;
        let (_, server) = inner.split_once(" - ")?;
        let server = server.trim();
        crate::session::is_build(server).then(|| server.to_string())
    })
}

pub fn split_host_port(s: &str) -> Option<(String, u16)> {
    let s = s.trim();
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (h, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if after.is_empty() => DEFAULT_PORT,
            None => return None,
        };
        (h.to_string(), port)
    } else {
        match s.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().ok()?),
            None => (s.to_string(), DEFAULT_PORT),
        }
    };
    let bare_colon = !s.starts_with('[') && host.contains(':');
    if host.is_empty() || host.contains(['/', ' ']) || bare_colon || port == 0 {
        return None;
    }
    Some((host, port))
}

fn connection_string(s: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| *c == ';' || c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            return Ok(out);
        }
        let key: String = chars.by_ref().take_while(|c| *c != '=').collect();
        let key = key.trim().to_string();
        if key.is_empty() || key.contains(';') {
            return Err("expected key=value pairs separated by ;".into());
        }
        let mut value = String::new();
        while chars.peek().is_some_and(|c| *c != ';' && c.is_whitespace()) {
            chars.next();
        }
        if chars.peek() == Some(&'"') {
            chars.next();
            loop {
                match chars.next() {
                    Some('"') if chars.peek() == Some(&'"') => {
                        chars.next();
                        value.push('"');
                    }
                    Some('"') => break,
                    Some(c) => value.push(c),
                    None => return Err(format!("unclosed quote in the value of {key}")),
                }
            }
            while chars.peek().is_some_and(|c| *c != ';') {
                if !chars.next().is_some_and(char::is_whitespace) {
                    return Err(format!("text after the quoted value of {key}"));
                }
            }
        } else {
            value = chars.by_ref().take_while(|c| *c != ';').collect();
            value = value.trim().to_string();
        }
        out.push((key, value));
    }
}

#[derive(Debug, Clone)]
pub struct NativeOptions {
    pub worker: Option<(String, u16)>,
    pub version: String,
    pub computer: String,
    pub os_user: String,
    pub auto_version: bool,
}

impl Default for NativeOptions {
    fn default() -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let computer = env("COMPUTERNAME")
            .or_else(|| env("HOSTNAME"))
            .unwrap_or_else(|| "client".to_string());
        let user = env("USERNAME")
            .or_else(|| env("USER"))
            .unwrap_or_else(|| "user".to_string());
        Self {
            worker: None,
            version: VERSION.to_string(),
            os_user: format!("{computer}\\{user}"),
            computer,
            auto_version: true,
        }
    }
}

pub type Fill = Box<dyn FnMut(&mut [u8]) -> Result<(), NativeError> + Send>;

fn os_fill() -> Fill {
    Box::new(|buf: &mut [u8]| getrandom::fill(buf).map_err(|e| NativeError::Random(e.to_string())))
}

struct Channel {
    stream: MessageStream<TcpStream>,
    session: Guid,
    frames: u64,
    cookie: u64,
}

pub struct NativeTransport {
    target: NativeTarget,
    opts: NativeOptions,
    fill: Fill,
    client_id: Option<[u8; 16]>,
    seq: u32,
    channel: Option<Channel>,
    broken: bool,
}

impl NativeTransport {
    pub fn new(target: NativeTarget, opts: NativeOptions) -> Self {
        Self::with_random(target, opts, os_fill())
    }

    pub fn with_random(target: NativeTarget, opts: NativeOptions, fill: Fill) -> Self {
        Self {
            target,
            opts,
            fill,
            client_id: None,
            seq: 0,
            channel: None,
            broken: false,
        }
    }

    pub fn target(&self) -> &NativeTarget {
        &self.target
    }

    pub fn set_worker(&mut self, worker: Option<(String, u16)>) {
        self.opts.worker = worker;
    }

    pub fn base_location(&self) -> String {
        format!(
            "e1c://server/{}/{}",
            self.target.authority(),
            self.target.ib
        )
    }

    fn connect_string(&self) -> String {
        format!(
            "Srvr=\"{}\";Ref=\"{}\";",
            self.target.authority().replace('"', "\"\""),
            self.target.ib.replace('"', "\"\"")
        )
    }

    fn rand<const N: usize>(&mut self) -> Result<[u8; N], NativeError> {
        let mut b = [0u8; N];
        (self.fill)(&mut b)?;
        Ok(b)
    }

    fn next_seq(&mut self) -> u32 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    fn client(&self) -> ClientInfo {
        ClientInfo {
            app: "1CV8C".into(),
            computer: self.opts.computer.clone(),
            version: self.opts.version.clone(),
            lang: "1ru".into(),
            locale: "1ru_RU".into(),
        }
    }

    fn guid_text(b: [u8; 16]) -> String {
        Guid::from_bytes(b).to_string()
    }

    fn open(&mut self, host: &str, port: u16) -> Result<MessageStream<TcpStream>, NativeError> {
        let addrs: Vec<SocketAddr> = (host, port)
            .to_socket_addrs()
            .map_err(|e| {
                NativeError::Io(io::Error::new(
                    e.kind(),
                    format!("cannot resolve {host}:{port}: {e}"),
                ))
            })?
            .collect();
        let mut last = None;
        let mut tcp = None;
        for a in &addrs {
            match TcpStream::connect_timeout(a, CONNECT_TIMEOUT) {
                Ok(s) => {
                    tcp = Some(s);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let tcp = tcp.ok_or_else(|| {
            let e = last.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"));
            NativeError::Io(io::Error::new(
                e.kind(),
                format!("cannot connect to {host}:{port}: {e}"),
            ))
        })?;
        tcp.set_read_timeout(Some(TIMEOUT))?;
        tcp.set_write_timeout(Some(TIMEOUT))?;
        tcp.set_nodelay(true)?;
        let mut stream = MessageStream::new(tcp);
        stream.expect_greeting()?;
        let id = match self.client_id {
            Some(id) => id,
            None => {
                let id = self.rand::<16>()?;
                self.client_id = Some(id);
                id
            }
        };
        stream.send_signal(&id)?;
        Ok(stream)
    }

    fn key_exchange(
        &mut self,
        stream: &mut MessageStream<TcpStream>,
        first_call: &str,
    ) -> Result<Vec<Node>, NativeError> {
        let fill = &mut self.fill;
        let key = ClientKey::generate(&mut |b: &mut [u8]| fill(b))?;
        stream.send_message(&keyx::request(&key.modulus(), first_call))?;
        let answer = stream.read_message()?;
        let (_, reply) = key.open_answer(&answer)?;
        call::parse_answer(&reply)
    }

    fn text_call(
        stream: &mut MessageStream<TcpStream>,
        text: &str,
    ) -> Result<Vec<Node>, NativeError> {
        stream.send_message(&call::message(text))?;
        call::parse_answer(&stream.read_message()?)
    }

    fn lookup(&mut self) -> Result<(String, u16), NativeError> {
        let (host, port) = (self.target.host.clone(), self.target.port);
        let mut m = self.open(&host, port)?;
        let (ctx, ctx_id) = MANAGER_CONTEXT;
        let first =
            call::create_instance(self.next_seq(), &self.client(), ctx, ctx_id, b"", NULL_GUID);
        let obj = call::created_object(&self.key_exchange(&mut m, &first)?)?;
        let bind = call::bind(&obj, self.next_seq(), IFACE_REGMNGR);
        Self::text_call(&mut m, &bind)?;
        let request_id = Self::guid_text(self.rand::<16>()?);
        let find = call::find_ib(&obj, self.next_seq(), &self.target.ib, "1CV8C", &request_id);
        let worker = call::worker_address(&Self::text_call(&mut m, &find)?)?;
        let release = call::release(&obj, self.next_seq());
        m.send_message(&call::message(&release))?;
        m.get_ref().set_read_timeout(Some(CLOSE_TIMEOUT))?;
        let _ = m.read_message();
        Ok(worker)
    }

    fn connect(&mut self) -> Result<Channel, NativeError> {
        if self.seq == 0 {
            self.seq = initial_seq(u16::from_le_bytes(self.rand::<2>()?));
        }
        let found = self.lookup()?;
        let named = self.opts.worker.is_none();
        let (host, port) = self.opts.worker.clone().unwrap_or(found);
        let mut w = self.open(&host, port).map_err(|e| match e {
            NativeError::Io(io) if named => NativeError::Io(io::Error::new(
                io.kind(),
                format!(
                    "{io} (the worker process the manager named; give its address with --worker HOST:PORT)"
                ),
            )),
            other => other,
        })?;
        let client = self.client();
        let (ctx, ctx_id) = call::worker_context(&self.target.ib);
        let last = Self::guid_text(self.rand::<16>()?);
        let ticket = ntlm::intro_ticket(&self.opts.os_user);
        let intro = call::create_instance(self.next_seq(), &client, &ctx, ctx_id, &ticket, &last);
        self.key_exchange(&mut w, &intro)?;
        let negotiate = call::create_instance(
            self.next_seq(),
            &client,
            &ctx,
            ctx_id,
            &ntlm::negotiate(),
            &last,
        );
        Self::text_call(&mut w, &negotiate)?;
        let mic = self.rand::<16>()?;
        let auth = call::create_instance(
            self.next_seq(),
            &client,
            &ctx,
            ctx_id,
            &ntlm::authenticate_anonymous(&mic),
            &last,
        );
        let session = call::created_object(&Self::text_call(&mut w, &auth)?)?;
        let bind = call::bind(&session, self.next_seq(), IFACE_VRS);
        Self::text_call(&mut w, &bind)?;
        let session = Guid::parse(&session)
            .ok_or_else(|| NativeError::Malformed(format!("bad session object {session:?}")))?;
        let cookie = frame::COOKIE_HIGH | u64::from(u32::from_le_bytes(self.rand::<4>()?));
        Ok(Channel {
            stream: w,
            session,
            frames: 0,
            cookie,
        })
    }

    fn send(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Response, NativeError> {
        if self.broken {
            return Err(NativeError::Broken);
        }
        if self.channel.is_none() {
            let channel = match self.connect() {
                Err(NativeError::Server(m)) if self.opts.auto_version => {
                    match server_build_of(&m).filter(|b| *b != self.opts.version) {
                        Some(build) => {
                            self.opts.version = build;
                            self.opts.auto_version = false;
                            self.connect()?
                        }
                        None => return Err(NativeError::Server(m)),
                    }
                }
                other => other?,
            };
            self.channel = Some(channel);
        }
        let mut h = Vec::with_capacity(headers.len() + 2);
        h.push(("1C-BaseLocation".to_string(), self.base_location()));
        h.extend(headers.iter().cloned());
        h.push(("1C-ConnectString".to_string(), self.connect_string()));
        let http = frame::http_request(method, path, &h, body);
        let seq = self.next_seq();
        let ch = self
            .channel
            .as_mut()
            .ok_or_else(|| NativeError::Malformed("no channel after connect".into()))?;
        let request = frame::request(&ch.session, seq, ch.frames == 0, ch.cookie, &http)?;
        ch.stream.send_message(&request)?;
        let answer = ch.stream.read_message()?;
        ch.frames += 1;
        frame::parse_http(&frame::response_http(&answer, MAX_RESPONSE_BODY)?)
    }

    pub fn close(&mut self) -> Result<(), NativeError> {
        let Some(mut ch) = self.channel.take() else {
            return Ok(());
        };
        let seq = self.next_seq();
        ch.stream.get_ref().set_read_timeout(Some(CLOSE_TIMEOUT))?;
        ch.stream
            .send_message(&frame::close(&ch.session, seq, ch.frames > 0)?)?;
        frame::check_close_answer(&ch.stream.read_message()?)
    }
}

impl Wire for NativeTransport {
    fn exchange(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Response, SessionError> {
        match self.send(method, path, headers, body) {
            Ok(r) => Ok(r),
            Err(e) => {
                if self.channel.take().is_some() {
                    self.broken = true;
                }
                Err(e.into())
            }
        }
    }

    fn set_version(&mut self, build: &str) {
        self.opts.version = build.to_string();
        self.opts.auto_version = false;
    }
}

impl Drop for NativeTransport {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
