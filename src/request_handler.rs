pub use request_hndlr::RequestHandler;

pub use crate::request_manager::{
    H3Method, RequestForm, RequestFormBuilder, RequestManager, RequestManagerBuilder, RequestType,
};
pub use request_temp_table::ReqArgs;
pub use request_temp_table::RequestsTable;
mod request_temp_table;
mod request_hndlr {

    use std::{
        collections::HashMap,
        env::args,
        sync::{Arc, Mutex},
    };

    use crate::{
        request_events::{self, RequestEvent},
        request_manager::RequestManagerInner,
        server_init::quiche_http3_server::{self, Client},
    };
    use quiche::{
        h3::{self, NameValue},
        Connection,
    };

    use self::request_temp_table::RequestsTable;

    use super::*;

    pub struct RequestHandler {
        inner: Arc<Mutex<RequestManagerInner>>,
    }

    impl RequestHandler {
        pub fn new(request_mngr_inner: Arc<Mutex<RequestManagerInner>>) -> Self {
            RequestHandler {
                inner: request_mngr_inner,
            }
        }
        pub fn write_body_packet(&self, stream_id: u64, conn_id: &str, packet: &[u8], end: bool) {
            let guard = &*self.inner.lock().unwrap();
            guard
                .request_states()
                .write_body_packet(conn_id.to_owned(), stream_id, packet, end);
        }
        pub fn print_entries(&self) {
            let guard = &*self.inner.lock().unwrap();
            for i in guard.request_formats().iter() {
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
        fn get_requests_from_path(&self, path: &str, cb: impl FnOnce(Option<&Vec<RequestForm>>)) {
            let guard = &*self.inner.lock().unwrap();
            cb(guard.request_formats().get(path));
        }
        pub fn handle_finished_stream(
            &self,
            conn_id: &str,
            stream_id: u64,
            client: &mut quiche_http3_server::QClient,
        ) {
            let guard = &*self.inner.lock().unwrap();
            if let Ok(request_event) = guard
                .request_states()
                .build_request_event(conn_id.to_owned(), stream_id)
            {
                println!(" !!!!found req !!!");
                let is_end = request_event.is_end();
                if let Some((request_form, _)) = guard
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
                        H3Method::POST => {
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
                        _ => {}
                    }
                }
            } else {
                println!("did not fonud");
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
            let guard = &*self.inner.lock().unwrap();
            if let Ok(method) = H3Method::parse(method.unwrap()) {
                println!("adding partial repsonse");
                guard.request_states().add_partial_request(
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
                println!("returning because more frames");
                return;
            }

            if let Ok(request_event) = guard
                .request_states()
                .build_request_event(conn_id, stream_id)
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
        pub fn get_requests_from_path_and_method_and_request_type(
            &self,
            path: &str,
            methode: H3Method,
            request_type: RequestType,
            cb: impl FnOnce(Option<&RequestForm>),
        ) {
            self.get_requests_from_path(path, |request_coll: Option<&Vec<RequestForm>>| {
                if request_coll.is_none() {
                    cb(None)
                } else {
                    let coll: &Vec<RequestForm> = request_coll.unwrap();
                    if let Some(found_request) = coll.iter().find(|item| {
                        item.method() == &methode && *item.request_type() == request_type
                    }) {
                        cb(Some(found_request));
                    } else {
                        cb(None)
                    }
                }
            });
        }
        /// Search for the corresponding request format to build the response and send it by triggering the
        /// associated callback.
        pub fn get_requests_from_path_and_method<'b>(
            &self,
            path: &'b str,
            methode: H3Method,
        ) -> Option<(&RequestForm, Option<Vec<&'b str>>)> {
            //if param in path

            /*
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
            */
            None
        }
    }
}
