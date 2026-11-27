use crate::session::SessionError;
use crate::transport::{HttpTransport, Response};

pub trait Wire: Send {
    fn exchange(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Response, SessionError>;

    fn exchange_once(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Response, SessionError> {
        self.exchange(method, path, headers, body)
    }

    fn set_version(&mut self, _build: &str) {}
}

impl Wire for HttpTransport {
    fn exchange(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Response, SessionError> {
        Ok(HttpTransport::exchange(self, method, path, headers, body)?)
    }

    fn exchange_once(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Response, SessionError> {
        Ok(HttpTransport::exchange_once(
            self, method, path, headers, body,
        )?)
    }
}

impl<W: Wire + ?Sized> Wire for Box<W> {
    fn exchange(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Response, SessionError> {
        (**self).exchange(method, path, headers, body)
    }

    fn exchange_once(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Response, SessionError> {
        (**self).exchange_once(method, path, headers, body)
    }

    fn set_version(&mut self, build: &str) {
        (**self).set_version(build)
    }
}
