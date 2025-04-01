pub use middleware_state::MiddleWareResult;
pub use middleware_trait::MiddleWare;
pub use middleware_types::HeadersColl;
mod middleware_trait {
    use super::{middleware_state::MiddleWareResult, middleware_types::HeadersColl};

    pub trait MiddleWare<S> {
        fn on_header<'a>(&self, headers: &HeadersColl<'a>, app_stat: &S) -> MiddleWareResult {
            MiddleWareResult::Continue
        }
    }
}

mod middleware_state {
    use crate::{ErrorResponse, Response};

    pub enum MiddleWareResult {
        Continue,
        Abort(ErrorResponse),
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
