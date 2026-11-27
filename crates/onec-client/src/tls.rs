use std::io;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{CertificateError, ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::transport::TransportError;

pub(crate) type TlsStream = StreamOwned<ClientConnection, TcpStream>;

#[derive(Clone)]
pub(crate) struct Tls {
    config: Arc<ClientConfig>,
    host: String,
    name: ServerName<'static>,
}

impl Tls {
    pub(crate) fn new(host: &str, ca_files: &[PathBuf]) -> Result<Self, TransportError> {
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let name = ServerName::try_from(bare.to_string())
            .map_err(|_| TransportError::Tls(format!("{host:?} is not a valid server name")))?;
        let mut roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        for path in ca_files {
            for cert in read_pem_certs(path)? {
                roots.add(cert).map_err(|e| TransportError::CaCert {
                    path: path.clone(),
                    reason: format!("not a usable CA certificate: {e}"),
                })?;
            }
        }
        let config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|e| TransportError::Tls(format!("configuration: {e}")))?
                .with_root_certificates(roots)
                .with_no_client_auth();
        Ok(Self {
            config: Arc::new(config),
            host: host.to_string(),
            name,
        })
    }

    pub(crate) fn connect(&self, mut sock: TcpStream) -> Result<TlsStream, TransportError> {
        let mut conn = ClientConnection::new(self.config.clone(), self.name.clone())
            .map_err(|e| TransportError::Tls(describe(&e, &self.host)))?;
        while conn.is_handshaking() {
            conn.complete_io(&mut sock)
                .map_err(|e| match classify(e, &self.host) {
                    TransportError::Io(e) => TransportError::Tls(handshake_io(&e)),
                    other => other,
                })?;
        }
        Ok(StreamOwned::new(conn, sock))
    }
}

fn handshake_io(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            "handshake timed out: the server accepted the connection but did not answer".to_string()
        }
        io::ErrorKind::UnexpectedEof => {
            "handshake failed: the server closed the connection (is it an https:// server?)"
                .to_string()
        }
        _ => format!("handshake failed: {e}"),
    }
}

fn read_pem_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, TransportError> {
    let err = |reason: String| TransportError::CaCert {
        path: path.to_path_buf(),
        reason,
    };
    let bytes = std::fs::read(path).map_err(|e| err(e.to_string()))?;
    let certs = CertificateDer::pem_slice_iter(&bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| err(format!("bad PEM: {e}")))?;
    if certs.is_empty() {
        return Err(err("no CERTIFICATE block in the file".to_string()));
    }
    Ok(certs)
}

pub(crate) fn classify(e: io::Error, host: &str) -> TransportError {
    match e
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
    {
        Some(tls) => TransportError::Tls(describe(tls, host)),
        None => TransportError::Io(e),
    }
}

fn describe(e: &rustls::Error, host: &str) -> String {
    match e {
        rustls::Error::InvalidCertificate(c) => match c {
            CertificateError::UnknownIssuer => {
                "server certificate is not trusted (unknown issuer); pass its CA with --ca-cert"
                    .to_string()
            }
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
                format!("server certificate is not valid for host {host:?}")
            }
            CertificateError::Expired | CertificateError::ExpiredContext { .. } => {
                "server certificate has expired".to_string()
            }
            CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => {
                "server certificate is not valid yet".to_string()
            }
            other => format!("server certificate is rejected: {other:?}"),
        },
        rustls::Error::AlertReceived(alert @ rustls::AlertDescription::CertificateRequired) => {
            format!(
                "protocol error: the server sent the alert {alert:?}: it asks for a client certificate, which is not \
             supported"
            )
        }
        rustls::Error::AlertReceived(alert) => {
            format!("protocol error: the server sent the alert {alert:?}")
        }
        other => format!("protocol error: {other}"),
    }
}
