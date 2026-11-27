pub mod call;
pub mod frame;
pub mod keyx;
pub mod liveness;
pub mod ntlm;
pub(crate) mod rsa;
pub mod stream;
pub mod transport;

pub const OBJ_CREATOR: &str = "e23134a2-14ff-4160-ba5f-ccef04e3786f";
pub const IFACE_CREATOR: &str = "7f58f27d-5ad8-43a1-aa1e-c982f41bed5c";
pub const IFACE_REGMNGR: &str = "73b7d3a3-fe0b-4fdf-ba70-b74b3589ffc3";
pub const IFACE_VRS: &str = "bc15bd01-10bf-413c-a856-ddc907fcd123";
pub const T_GUIDREF: &str = "ae135932-4f94-44df-92c1-c91f15a92848";
pub const T_TICKET: &str = "671507fd-50a9-4b63-b70e-58b3d364f48f";
pub const T_OBJREF: &str = "3ace6d91-51bb-4344-9388-c8105ad4ad11";
pub const T_ADDR: &str = "dd5dcb0e-62b5-4d01-8305-6f87bbc772c6";
pub const T_HTTP: &str = "bea56fd3-3695-49fc-bbc5-c8bc15f33d41";
pub const EXC_RETRY_CONNECT: &str = "05a83ab3-14ea-4d74-aef1-3128b035c350";
pub const CTX_REGMNGR: &str = "4ecea39c-b4c9-453e-aed0-ce8a24cc71c2";
pub const CTX_IB: &str = "1e01900d-82fd-413a-bfe5-571a4c30a685";
pub const NULL_GUID: &str = "00000000-0000-0000-0000-000000000000";

pub const BOM: &[u8] = b"\xef\xbb\xbf";

#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("not a 1C server: greeting {0}")]
    NotOneC(String),
    #[error("connection closed by the server")]
    Closed,
    #[error("{0} exceeds {1} bytes")]
    TooLarge(&'static str, usize),
    #[error("malformed answer: {0}")]
    Malformed(String),
    #[error("key exchange: {0}")]
    Crypto(String),
    #[error(
        "the server requires security level {0} (an encrypted channel); only level 0 is implemented, \
         lower the security level of the server or connect over /WS"
    )]
    SecurityLevel(u8),
    #[error("the server refused the key exchange")]
    KeyRefused,
    #[error("server exception: {0}")]
    Server(String),
    #[error("the server asks to connect again: {0}")]
    Retry(String),
    #[error("random source failed: {0}")]
    Random(String),
}
