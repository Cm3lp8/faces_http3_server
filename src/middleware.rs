pub use middleware_state::{MiddleWareFlow, MiddleWareResult};
pub use middleware_trait::MiddleWare;
pub use middleware_types::HeadersColl;
mod middleware_trait {
    use quiche::h3;

    use super::{
        middleware_state::{MiddleWareFlow, MiddleWareResult},
        middleware_types::HeadersColl,
    };

    pub trait MiddleWare<S> {
        fn on_header<'a>(&self, headers: &HeadersColl<'a>, app_stat: &S) -> MiddleWareFlow {
            MiddleWareFlow::Continue
        }
        fn callback(&self) -> Box<dyn FnMut(&mut [h3::Header], &S) -> MiddleWareFlow>;
    }
}

mod middleware_state {
    use quiche::h3;

    use crate::{ErrorResponse, H3Method, Response};

    pub enum MiddleWareFlow {
        Continue,
        Abort(ErrorResponse),
    }

    pub enum MiddleWareResult {
        /// Direcly send it to the response queue
        Abort {
            error_response: ErrorResponse,
            stream_id: u64,
            scid: Vec<u8>,
        },
        Success {
            path: String,
            method: H3Method,
            headers: Vec<h3::Header>,
            stream_id: u64,
            scid: Vec<u8>,
        },
    }
    impl MiddleWareResult {
        pub fn abort(error_response: ErrorResponse, stream_id: u64, scid: &[u8]) -> Self {
            Self::Abort {
                error_response,
                stream_id,
                scid: scid.to_vec(),
            }
        }
        pub fn succest(
            path: &str,
            method: H3Method,
            headers: Vec<h3::Header>,
            stream_id: u64,
            scid: &[u8],
        ) -> Self {
            Self::Success {
                path: path.to_string(),
                method,
                headers,
                stream_id,
                scid: scid.to_vec(),
            }
        }
        pub fn stream_id(&self) -> Result<u64, ()> {
            match self {
                Self::Success {
                    path: _,
                    method: _,
                    headers: _,
                    stream_id,
                    scid: _,
                } => Ok(*stream_id),
                Self::Abort {
                    error_response: _,
                    stream_id,
                    scid: _,
                } => Ok(*stream_id),
                _ => Err(()),
            }
        }
        pub fn scid(&self) -> Result<Vec<u8>, ()> {
            match self {
                Self::Success {
                    path: _,
                    method: _,
                    headers: _,
                    stream_id: _,
                    scid,
                } => Ok(scid.to_vec()),
                Self::Abort {
                    error_response: _,
                    stream_id: _,
                    scid,
                } => Ok(scid.to_vec()),
                _ => Err(()),
            }
        }
    }
}

mod middleware_types {
    use quiche::h3::{self, NameValue};

    pub enum HeadersColl<'a> {
        HeadersPost(&'a mut [h3::Header]),
        HeadersGet(&'a mut [h3::Header]),
    }

    impl<'a> HeadersColl<'a> {
        pub fn path(&mut self) -> &str {
            "l"
        }
        pub fn display(&self) -> String {
            let mut string = String::new();
            let mut coll: Vec<String> = vec![];
            match self {
                Self::HeadersGet(headers) => {
                    for hdr in headers.iter() {
                        coll.push(format!(
                            "|{} = {}|",
                            String::from_utf8_lossy(hdr.name()),
                            String::from_utf8_lossy(hdr.value())
                        ));
                    }
                }
                Self::HeadersPost(headers) => {
                    for hdr in headers.iter() {
                        coll.push(format!(
                            "|{} = {}|",
                            String::from_utf8_lossy(hdr.name()),
                            String::from_utf8_lossy(hdr.value())
                        ));
                    }
                }
            }
            coll.join("\n").to_owned()
        }
    }
}
