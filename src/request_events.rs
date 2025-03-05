pub use request_event::RequestEvent;
mod request_event {
    use std::path::PathBuf;

    use quiche::h3;

    use crate::{route_handler::ReqArgs, H3Method};

    use super::*;

    pub struct RequestEvent {
        path: String,
        headers: Vec<h3::Header>,
        method: H3Method,
        args: Option<Vec<ReqArgs>>,
        file_path: Option<PathBuf>,
        body: Vec<u8>,
        is_end: bool,
    }
    impl RequestEvent {
        pub fn new(
            path: &str,
            method: H3Method,
            headers: Vec<h3::Header>,
            args: Option<Vec<ReqArgs>>,
            file_path: Option<PathBuf>,
            body: Option<Vec<u8>>,
            is_end: bool,
        ) -> Self {
            Self {
                path: path.to_owned(),
                method,
                headers,
                args,
                file_path,
                body: body.unwrap_or(vec![]),
                is_end,
            }
        }
        pub fn get_file_path(&self) -> Option<&PathBuf> {
            self.file_path.as_ref()
        }
        pub fn path(&self) -> &str {
            self.path.as_str()
        }
        pub fn method(&self) -> H3Method {
            self.method
        }
        pub fn headers(&self) -> &Vec<h3::Header> {
            &self.headers
        }
        pub fn is_end(&self) -> bool {
            self.is_end
        }
        pub fn body_size(&self) -> usize {
            self.body.len()
        }
        pub fn extend_body_data(&mut self, data: &[u8]) {
            self.body.extend_from_slice(data);
        }
        pub fn args(&self) -> Option<&Vec<ReqArgs>> {
            self.args.as_ref()
        }
        pub fn take_body(&mut self) -> Vec<u8> {
            std::mem::replace(&mut self.body, Vec::with_capacity(1))
        }
        pub fn as_body(&self) -> &Vec<u8> {
            &self.body
        }
    }
}
