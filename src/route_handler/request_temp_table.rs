pub use req_temp_table::RequestsTable;
pub use request_argument_parser::ReqArgs;
mod reception_status {
    use super::*;

    pub struct ReceptionStatus {
        percentage_written: Option<usize>,
    }
    impl ReceptionStatus {
        pub fn new(percentage_written: Option<usize>) -> Self {
            ReceptionStatus { percentage_written }
        }
        pub fn has_something_to_update(&self) -> bool {
            if let Some(p_w) = self.percentage_written {
                true
            } else {
                false
            }
        }
        pub fn get_percentage_written_to_string(&self) -> Option<String> {
            if let Some(perc) = self.percentage_written {
                Some(perc.to_string())
            } else {
                None
            }
        }
        pub fn is_end(&self) -> bool {
            if let Some(progress) = self.percentage_written {
                progress == 100
            } else {
                false
            }
        }
        ///
        /// return the progress body
        ///
        /// # Example
        /// let status = ReceptionStatus::new(5);
        ///
        /// let body = status.body();
        ///
        /// assert!(&body == b"progress=5");
        ///
        pub fn body(&self) -> Option<Vec<u8>> {
            if let Some(progress) = self.percentage_written {
                Some(format!("s??%progress={}", progress).as_bytes().to_vec())
            } else {
                None
            }
        }
    }
}
mod req_temp_table {
    use quiche::h3;
    use request_argument_parser::ReqArgs;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use crate::{
        request_events::{self, RequestEvent},
        H3Method,
    };

    use self::reception_status::ReceptionStatus;

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
        pub fn set_intermediate_headers_send(&self, stream_id: u64, conn_id: String) {
            if let Some(entry) = self.table.lock().unwrap().get_mut(&(conn_id, stream_id)) {
                entry.progress_header_sent = true;
            }
        }
        pub fn get_reception_status_infos(
            &self,
            stream_id: u64,
            conn_id: String,
        ) -> Option<(ReceptionStatus, bool)> {
            if let Some(entry) = self.table.lock().unwrap().get_mut(&(conn_id, stream_id)) {
                let headers_send = entry.progress_header_sent;
                if let Some(content_length) = entry.content_length {
                    let written = entry.written();
                    let percentage_written =
                        ((written as f32 / content_length as f32) * 100f32) as usize;

                    if let Some(prec_value) = entry.precedent_percentage_written {
                        if prec_value == percentage_written {
                            entry.precedent_percentage_written = Some(percentage_written);
                            return Some((ReceptionStatus::new(None), headers_send));
                        }
                        entry.precedent_percentage_written = Some(percentage_written);
                        return Some((
                            ReceptionStatus::new(Some(percentage_written)),
                            headers_send,
                        ));
                    }
                    entry.precedent_percentage_written = Some(percentage_written);
                    return Some((ReceptionStatus::new(Some(percentage_written)), headers_send));
                }
                return None;
            }
            None
        }
        /// Once all the data is received by the server, this takes infos from it, builds the request event  that trigger a reponse for the client.
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
                if let Some(req_event) = request_event {
                    Ok(req_event)
                } else {
                    Err(())
                }
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
        ) -> Result<usize, ()> {
            let mut total_written: Result<usize, ()> = Err(());
            if let Some(entry) = self.table.lock().unwrap().get_mut(&(conn_id, stream_id)) {
                entry.extend_data(packet, is_end);
                total_written = Ok(entry.written());
            }
            total_written
        }
        /// Keep track of a client request based on unique connexion_id and stream_id
        pub fn add_partial_request(
            &self,
            conn_id: String,
            stream_id: u64,
            method: H3Method,
            headers: &[h3::Header],
            path: &str,
            content_length: Option<usize>,
            is_end: bool,
        ) {
            let partial_request = PartialReq::new(
                conn_id.clone(),
                stream_id,
                method,
                headers,
                path,
                content_length,
                is_end,
            );
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
        headers: Option<Vec<h3::Header>>,
        method: H3Method,
        path: String,
        args: Option<Vec<ReqArgs>>,
        content_length: Option<usize>,
        precedent_percentage_written: Option<usize>,
        progress_header_sent: bool,
        body: Vec<u8>,
        is_end: bool,
    }
    impl PartialReq {
        pub fn new(
            conn_id: String,
            stream_id: u64,
            method: H3Method,
            headers: &[h3::Header],
            path: &str,
            content_length: Option<usize>,
            is_end: bool,
        ) -> Self {
            let (path, args) = ReqArgs::parse_args(path);

            Self {
                conn_id,
                stream_id,
                headers: Some(headers.to_vec()),
                method,
                path,
                args,
                content_length,
                precedent_percentage_written: None,
                progress_header_sent: false,
                body: vec![],
                is_end,
            }
        }
        pub fn extend_data(&mut self, packet: &[u8], is_end: bool) {
            self.is_end = is_end;
            self.body.extend_from_slice(packet);
        }
        pub fn written(&self) -> usize {
            self.body.len()
        }
        pub fn to_request_event(&mut self) -> Option<RequestEvent> {
            if let Some(headers) = self.headers.as_ref() {
                Some(RequestEvent::new(
                    self.path.as_str(),
                    self.method,
                    headers.clone(),
                    self.args.take(),
                    Some(std::mem::replace(&mut self.body, vec![])),
                    self.is_end,
                ))
            } else {
                None
            }
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
