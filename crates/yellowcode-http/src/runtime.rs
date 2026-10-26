mod endpoint;
mod input;
pub use endpoint::{Endpoint, EndpointConfig, StartError};
pub use input::{InputError, read_request};
use yellowcode_server::{
    process::{NativeContext, Reuse, RuntimeProcess, SessionHandle},
    service::{RequestData, ResponseData, ServiceError},
    users::Credentials,
};

pub fn serve(
    process: &RuntimeProcess,
    context: &NativeContext,
    handle: SessionHandle,
    credentials: Option<Credentials<'_>>,
    reuse: Reuse,
    request: &RequestData,
) -> Result<ResponseData, ServiceError> {
    process.service_call(context, handle, credentials, reuse, request)
}
pub fn write_response(
    writer: &mut dyn std::io::Write,
    response: &ResponseData,
    head_only: bool,
) -> std::io::Result<()> {
    crate::transport::write_owned_response(
        writer,
        response.status,
        &response.headers,
        &response.body,
        head_only,
    )
}
