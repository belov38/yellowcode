use super::{read_request, write_response};
use crate::{DeadlineReader, close_gracefully, conn::DeadlineWriter};
use std::{
    io,
    net::{SocketAddr, TcpListener, TcpStream},
    sync::Arc,
    time::{Duration, Instant},
};
use yellowcode_server::{
    process::{ProcessError, RuntimeProcess},
    service::{RequestData, ResponseData},
};

pub struct EndpointConfig {
    pub listen: SocketAddr,
    pub read_timeout: Duration,
    pub write_timeout: Duration,
}
#[derive(Debug)]
pub enum StartError {
    Config,
    Process(ProcessError),
    Io(io::Error),
}
impl From<io::Error> for StartError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct Endpoint {
    listener: TcpListener,
    process: Arc<RuntimeProcess>,
    config: EndpointConfig,
    origin: String,
    sequence: u128,
}
impl Endpoint {
    pub fn start(
        config: EndpointConfig,
        open: impl FnOnce() -> Result<Arc<RuntimeProcess>, ProcessError>,
    ) -> Result<Self, StartError> {
        if !config.listen.ip().is_loopback()
            || config.read_timeout.is_zero()
            || config.write_timeout.is_zero()
            || config.read_timeout > Duration::from_secs(60)
            || config.write_timeout > Duration::from_secs(60)
        {
            return Err(StartError::Config);
        }
        let process = open().map_err(StartError::Process)?;
        let listener = TcpListener::bind(config.listen)?;
        listener.set_nonblocking(true)?;
        let origin = format!("http://{}", listener.local_addr()?);
        Ok(Self {
            listener,
            process,
            config,
            origin,
            sequence: 0,
        })
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub fn accept_one(&mut self) -> io::Result<bool> {
        let stream = match self.listener.accept() {
            Ok((stream, _)) => stream,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        stream.set_nonblocking(false)?;
        let result = self.connection(&stream);
        close_gracefully(&stream);
        result.map(|()| true)
    }
    fn connection(&mut self, stream: &TcpStream) -> io::Result<()> {
        let mut reader = DeadlineReader {
            stream,
            deadline: Instant::now() + self.config.read_timeout,
        };
        let (response, head_only) = match read_request(&mut reader, &self.origin) {
            Ok(request) => {
                let head_only = request.method == "HEAD";
                (self.dispatch(&request), head_only)
            }
            Err(error) => (failure(error.status()), false),
        };
        let response = if write_response(&mut io::sink(), &response, true).is_err() {
            failure(500)
        } else {
            response
        };
        stream.set_nonblocking(true)?;
        let mut writer = DeadlineWriter {
            stream,
            deadline: Instant::now() + self.config.write_timeout,
            wire_bytes: 0,
        };
        let result = write_response(&mut writer, &response, head_only);
        result.and(stream.set_nonblocking(false))
    }
    fn dispatch(&mut self, request: &RequestData) -> ResponseData {
        if request.headers.iter().any(|(name, _)| {
            name.eq_ignore_ascii_case("Authorization")
                || name.eq_ignore_ascii_case("Cookie")
                || name.eq_ignore_ascii_case("IBSession")
        }) {
            return failure(501);
        }
        self.sequence = match self.sequence.checked_add(1) {
            Some(next) => next,
            None => return failure(503),
        };
        let context = self.process.context(self.sequence.to_le_bytes());
        match self.process.service_call_fresh(&context, None, request) {
            Ok(response) => response,
            Err(error) => failure(error.status()),
        }
    }
}
fn failure(status: u16) -> ResponseData {
    ResponseData {
        status,
        headers: vec![],
        body: vec![],
    }
}
