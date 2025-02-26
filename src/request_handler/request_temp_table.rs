pub use req_temp_table::RequestsTable;
pub use request_argument_parser::ReqArgs;
mod req_temp_table {
    use request_argument_parser::ReqArgs;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use crate::{
        request_events::{self, RequestEvent},
        H3Method,
    };

    use super::*;

    type ReqId = (String, u64);
    pub struct RequestsTable {
        table: Arc<Mutex<HashMap<ReqId, PartialReq>>>,
    }

    impl RequestsTable {
        pub fn new() -> Self {
            Self {
                table: Arc::new(Mutex::new(HashMap::new())),
            }
        }
        /// Build the request event.  Once builded the registered partial request entry is removed.
        pub fn build_request_event(
            &self,
            conn_id: String,
            stream_id: u64,
        ) -> Result<RequestEvent, ()> {
            let mut can_clean = false;
            let res = if let Some(partial_req) = self
                .table
                .lock()
                .unwrap()
                .get_mut(&(conn_id.clone(), stream_id))
            {
                let request_event = partial_req.to_request_event();

                can_clean = true;
                Ok(request_event)
            } else {
                Err(())
            };

            if can_clean {
                self.table.lock().unwrap().remove(&(conn_id, stream_id));
            }
            res
        }
        pub fn write_body_packet(
            &self,
            conn_id: String,
            stream_id: u64,
            packet: &[u8],
            is_end: bool,
        ) {
            if let Some(entry) = self.table.lock().unwrap().get_mut(&(conn_id, stream_id)) {
                entry.extend_data(packet, is_end);
            }
        }
        /// Keep track of a client request based on unique connexion_id and stream_id
        pub fn add_partial_request(
            &self,
            conn_id: String,
            stream_id: u64,
            method: H3Method,
            path: &str,
            is_end: bool,
        ) {
            let partial_request = PartialReq::new(conn_id.clone(), stream_id, method, path, is_end);
            self.table
                .lock()
                .unwrap()
                .insert((conn_id, stream_id), partial_request);
        }
    }
    #[derive(Debug)]
    struct PartialReq {
        conn_id: String,
        stream_id: u64,
        method: H3Method,
        path: String,
        args: Option<Vec<ReqArgs>>,
        body: Vec<u8>,
        is_end: bool,
    }
    impl PartialReq {
        pub fn new(
            conn_id: String,
            stream_id: u64,
            method: H3Method,
            path: &str,
            is_end: bool,
        ) -> Self {
            let (path, args) = ReqArgs::parse_args(path);
            Self {
                conn_id,
                stream_id,
                method,
                path,
                args,
                body: vec![],
                is_end,
            }
        }
        pub fn extend_data(&mut self, packet: &[u8], is_end: bool) {
            self.is_end = is_end;
            self.body.extend_from_slice(packet);
        }
        pub fn to_request_event(&mut self) -> RequestEvent {
            RequestEvent::new(
                self.path.as_str(),
                self.method,
                self.args.take(),
                Some(std::mem::replace(&mut self.body, vec![])),
                self.is_end,
            )
        }
    }
}

mod request_argument_parser {
    use super::*;
    #[derive(Debug)]
    pub struct ReqArgs {
        parameter: String,
        value: String,
    }

    impl ReqArgs {
        pub fn build_query_string(query_string: &str) -> Vec<ReqArgs> {
            let mut args: Vec<ReqArgs> = vec![];

            for s in query_string.split("&") {
                if let Some((param, value)) = s.split_once("=") {
                    args.push(ReqArgs {
                        parameter: param.to_string(),
                        value: value.to_string(),
                    });
                }
            }
            args
        }
        // Parsing the request path string and split it if query-string is present = (path, vec<ReqArgs>).
        pub fn parse_args(path: &str) -> (String, Option<Vec<ReqArgs>>) {
            match path.split_once("?") {
                Some((path_split, query_string)) => {
                    let req_args = ReqArgs::build_query_string(query_string);
                    (path_split.to_string(), Some(req_args))
                }
                None => (path.to_string(), None),
            }
        }
        pub fn parameter(&self) -> &str {
            self.parameter.as_str()
        }
        pub fn value(&self) -> &str {
            self.value.as_str()
        }
    }
}
mod test_request_argument_parser {
    use self::request_argument_parser::ReqArgs;

    use super::*;

    #[test]
    fn test_request_path_parsing() {
        let path = "/path?name=charles&role=golfer&age=30";

        let (path, args) = ReqArgs::parse_args(path);

        assert!(path.as_str() == "/path");
        assert!(args.is_some());

        let mut name: Option<String> = None;
        let mut role: Option<String> = None;
        let mut age: Option<String> = None;

        for args in args.unwrap() {
            if args.parameter() == "name" {
                name = Some(args.value().to_string())
            }
            if args.parameter() == "role" {
                role = Some(args.value().to_string())
            }
            if args.parameter() == "age" {
                age = Some(args.value().to_string())
            }
        }

        assert!(name.is_some());
        assert!(age.is_some());
        assert!(role.is_some());

        let name = name.unwrap();
        let role = role.unwrap();
        let age = age.unwrap();

        assert!(name.as_str() == "charles");
        assert!(role.as_str() == "golfer");
        assert!(age.as_str() == "30");
    }
}
