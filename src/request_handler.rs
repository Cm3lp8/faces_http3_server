pub use request_hndlr::RequestHandler;

pub use crate::request_manager::{
    H3Method, RequestForm, RequestFormBuilder, RequestManager, RequestManagerBuilder, RequestType,
};
pub use request_temp_table::ReqArgs;
mod request_temp_table;
mod request_hndlr {

    use std::{
        collections::HashMap,
        env::args,
        sync::{Arc, Mutex},
    };

    use crate::{
        request_events::{self, RequestEvent},
        server_init::quiche_http3_server::{self, Client},
    };
    use quiche::{
        h3::{self, NameValue},
        Connection,
    };

    use self::request_temp_table::RequestsTable;

    use super::*;

    pub struct RequestHandler<'a> {
        request_formats: &'a HashMap<&'static str, Vec<RequestForm>>,
        request_states: RequestsTable, //trace_id of the Connection as
                                       //key value is HashMap
                                       //for stream_id u64
    }

    impl<'a> RequestHandler<'a> {
        pub fn new(request_formats: &'a HashMap<&'static str, Vec<RequestForm>>) -> Self {
            RequestHandler {
                request_formats,
                request_states: RequestsTable::new(),
            }
        }
        pub fn write_body_packet(&self, stream_id: u64, conn_id: &str, packet: &[u8], end: bool) {
            self.request_states
                .write_body_packet(conn_id.to_owned(), stream_id, packet, end);
        }
        pub fn print_entries(&self) {
            for i in self.request_formats.iter() {
                println!("entrie [{}]", i.0);
            }
        }
        pub fn fetch_data_stream(
            &self,
            stream_id: u64,
            conn: &mut quiche::Connection,
            h3_conn: &mut h3::Connection,
        ) {
        }
        fn get_requests_from_path(&self, path: &str) -> Option<&Vec<RequestForm>> {
            self.request_formats.get(path)
        }
        pub fn handle_finished_stream(
            &self,
            conn_id: &str,
            stream_id: u64,
            client: &mut quiche_http3_server::QClient,
        ) {
            if let Ok(request_event) = self
                .request_states
                .build_request_event(conn_id.to_owned(), stream_id)
            {
                let is_end = request_event.is_end();
                if let Some((request_form, _)) = self
                    .get_requests_from_path_and_method(request_event.path(), request_event.method())
                {
                    match request_form.method() {
                        H3Method::GET => {
                            /*stream shutdown send response*/

                            client
                                .conn()
                                .stream_shutdown(stream_id, quiche::Shutdown::Read, 0)
                                .unwrap();
                            if let Ok((headers, body)) = request_form.build_response(request_event)
                            {
                                quiche_http3_server::send_response(
                                    client, stream_id, headers, body, is_end,
                                )
                            }
                        }
                        H3Method::POST => { /*create entry in handler state*/ }
                        _ => {}
                    }
                }
            }
        }
        pub fn parse_headers(
            &self,
            headers: &[h3::Header],
            stream_id: u64,
            client: &mut quiche_http3_server::QClient,
            more_frames: bool,
        ) {
            let conn_id = client.conn().trace_id().to_string();
            let mut method: Option<&[u8]> = None;
            let mut path: Option<&str> = None;

            for hdr in headers {
                match hdr.name() {
                    b":method" => method = Some(hdr.value()),
                    b":path" => path = Some(std::str::from_utf8(hdr.value()).unwrap()),
                    _ => {}
                }
            }
            if method.is_none() || path.is_none() {
                return;
            }
            if let Ok(method) = H3Method::parse(method.unwrap()) {
                self.request_states.add_partial_request(
                    conn_id.clone(),
                    stream_id,
                    method,
                    path.unwrap(),
                    !more_frames,
                );
            } else {
                return;
            }

            if more_frames {
                return;
            }

            if let Ok(request_event) = self.request_states.build_request_event(conn_id, stream_id) {
                let is_end = request_event.is_end();
                if let Some((request_form, _)) = self
                    .get_requests_from_path_and_method(request_event.path(), request_event.method())
                {
                    match request_form.method() {
                        H3Method::GET => {
                            /*stream shutdown send response*/

                            client
                                .conn()
                                .stream_shutdown(stream_id, quiche::Shutdown::Read, 0)
                                .unwrap();
                            if let Ok((headers, body)) = request_form.build_response(request_event)
                            {
                                quiche_http3_server::send_response(
                                    client, stream_id, headers, body, is_end,
                                )
                            }
                        }
                        H3Method::POST => { /*create entry in handler state*/ }
                        _ => {}
                    }
                }
            }
        }
        pub fn get_requests_from_path_and_method_and_request_type(
            &self,
            path: &str,
            methode: H3Method,
            request_type: RequestType,
        ) -> Option<&RequestForm> {
            if let Some(request_coll) = self.get_requests_from_path(path) {
                if let Some(found_request) = request_coll
                    .iter()
                    .find(|item| item.method() == &methode && item.request_type() == &request_type)
                {
                    return Some(found_request);
                }
                None
            } else {
                None
            }
        }
        /// Search for the corresponding request format to build the response and send it by triggering the
        /// associated callback.
        pub fn get_requests_from_path_and_method<'b>(
            &self,
            path: &'b str,
            methode: H3Method,
        ) -> Option<(&RequestForm, Option<Vec<&'b str>>)> {
            //if param in path

            let mut path_s = path.to_string();
            let mut param_trail: Option<Vec<&str>> = None;

            if let Some((path, id)) = path.split_once("?") {
                path_s = path.to_string();

                let args_it: Vec<&str> = id.split("&").collect();
                param_trail = Some(args_it);
            }

            if let Some(request_coll) = self.get_requests_from_path(path_s.as_str()) {
                if let Some(found_request) =
                    request_coll.iter().find(|item| item.method() == &methode)
                {
                    return Some((found_request, param_trail));
                }
                None
            } else {
                None
            }
        }
    }
}
