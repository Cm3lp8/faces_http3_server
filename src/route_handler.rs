pub use request_hndlr::RouteHandler;

pub use crate::route_manager::{
    H3Method, RequestType, RouteForm, RouteFormBuilder, RouteManager, RouteManagerBuilder,
};
pub use request_temp_table::ReqArgs;
pub use request_temp_table::RequestsTable;
mod request_temp_table;
mod request_hndlr {

    use std::{
        collections::HashMap,
        env::args,
        fmt::Pointer,
        sync::{Arc, Mutex},
    };

    use crate::{
        request_events::{self, RequestEvent},
        route_manager::RouteManagerInner,
        server_config,
        server_init::quiche_http3_server::{self, Client},
        BodyStorage, ServerConfig,
    };
    use quiche::{
        h3::{self, NameValue},
        Connection,
    };

    use self::request_temp_table::RequestsTable;

    use super::*;

    pub struct RouteHandler {
        inner: Arc<Mutex<RouteManagerInner>>,
    }

    impl RouteHandler {
        pub fn new(route_mngr_inner: Arc<Mutex<RouteManagerInner>>) -> Self {
            RouteHandler {
                inner: route_mngr_inner,
            }
        }
        ///
        /// Send a reception status to the client, only if something can be updated.
        ///
        ///
        pub fn send_reception_status(
            &self,
            client: &mut quiche_http3_server::QClient,
            stream_id: u64,
            conn_id: &str,
        ) -> Result<usize, ()> {
            let guard = &*self.inner.lock().unwrap();
            let reception_status = guard
                .routes_states()
                .get_reception_status_infos(stream_id, conn_id.to_owned());

            if let Some((reception_status, header_send)) = reception_status {
                if reception_status.has_something_to_update() {
                    if let Some(percentage_written) =
                        reception_status.get_percentage_written_to_string()
                    {
                        let headers = vec![
                            h3::Header::new(b":status", b"100"),
                            h3::Header::new(b"x-for", stream_id.to_string().as_bytes()),
                            h3::Header::new(b"x-progress", percentage_written.as_bytes()),
                        ];

                        if !header_send {
                            guard
                                .routes_states()
                                .set_intermediate_headers_send(stream_id, conn_id.to_string());
                            return quiche_http3_server::send_header(
                                client, stream_id, headers, false,
                            );
                        }
                        {
                            if let Some(body) = reception_status.body() {
                                if let Ok(stream_writable) =
                                    client.conn().stream_writable(stream_id, 1350)
                                {
                                    if stream_writable {
                                        if let Err(()) = quiche_http3_server::send_body(
                                            client, stream_id, body, false,
                                        ) {
                                            error!("Failed sending progress body status")
                                        }
                                    }
                                } else {
                                    warn!("stream [{stream_id}] is closed");
                                }
                            }
                        }
                    };
                }
            }
            Ok(0)
        }
        pub fn write_body_packet(
            &self,
            stream_id: u64,
            conn_id: &str,
            packet: &[u8],
            end: bool,
        ) -> Result<usize, ()> {
            let guard = &*self.inner.lock().unwrap();
            let written_data =
                guard
                    .routes_states()
                    .write_body_packet(conn_id.to_owned(), stream_id, packet, end);
            written_data
        }
        pub fn print_entries(&self) {
            let guard = &*self.inner.lock().unwrap();
            for i in guard.routes_formats().iter() {
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
        fn get_routes_from_path(&self, path: &str, cb: impl FnOnce(Option<&Vec<RouteForm>>)) {
            let guard = &*self.inner.lock().unwrap();
            cb(guard.routes_formats().get(path));
        }
        pub fn handle_finished_stream(
            &self,
            conn_id: &str,
            stream_id: u64,
            client: &mut quiche_http3_server::QClient,
        ) {
            let guard = &*self.inner.lock().unwrap();
            if let Ok(request_event) = guard
                .routes_states()
                .build_request_event(conn_id.to_owned(), stream_id)
            {
                let is_end = request_event.is_end();
                if let Some((request_form, _)) = guard
                    .get_routes_from_path_and_method(request_event.path(), request_event.method())
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
                                if let Err(_) = quiche_http3_server::send_response(
                                    client, stream_id, headers, body, is_end,
                                ) {
                                    error!("Failed sending h3 response after GET request")
                                }
                            }
                        }
                        H3Method::POST => {
                            if let Ok((headers, body)) = request_form.build_response(request_event)
                            {
                                if let Err(_) = quiche_http3_server::send_response_when_finished(
                                    client, stream_id, headers, body, true,
                                ) {
                                    error!("Failed to send response after a post request")
                                } else {
                                    match client.conn().stream_shutdown(
                                        stream_id,
                                        quiche::Shutdown::Read,
                                        0,
                                    ) {
                                        Ok(_v) => {}
                                        Err(e) => {
                                            error!(
                                        "error stream_shutdown stream_id [{stream_id}] [{:?}]",
                                        e
                                    )
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            } else {
                debug!("Not found");
            }
        }
        pub fn parse_headers(
            &self,
            server_config: &Arc<ServerConfig>,
            headers: &[h3::Header],
            stream_id: u64,
            client: &mut quiche_http3_server::QClient,
            more_frames: bool,
        ) {
            let conn_id = client.conn().trace_id().to_string();
            let mut method: Option<&[u8]> = None;
            let mut path: Option<&str> = None;
            let mut content_length: Option<usize> = None;

            for hdr in headers {
                match hdr.name() {
                    b":method" => method = Some(hdr.value()),
                    b":path" => path = Some(std::str::from_utf8(hdr.value()).unwrap()),
                    b"content-length" => {
                        if let Some(method) = method {
                            if let Ok(method_parsed) = H3Method::parse(method) {
                                if H3Method::POST == method_parsed || H3Method::PUT == method_parsed
                                {
                                    content_length = Some(
                                    std::str::from_utf8(hdr.value())
                                .unwrap_or_else(|item| {
                                    error!(
                                        "Failed to parse bytes into str, default is \"0\" length "
                                    );
                                    "0"
                                })
                                .parse::<usize>()
                                .unwrap_or_else(|_| {
                                    error!("Failed to parse digit_string to usize. Default is \"0\" length");
                                    0
                                }),
                        )
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            if method.is_none() || path.is_none() {
                return;
            }

            let guard = &*self.inner.lock().unwrap();
            warn!("Ici");

            let mut storage_type: Option<BodyStorage> = None;
            if let Ok(method) = H3Method::parse(method.unwrap()) {
                if let Some((route_form, _)) =
                    guard.get_routes_from_path_and_method(path.unwrap(), method)
                {
                    storage_type = route_form.storage_type();
                }
                guard.routes_states().add_partial_request(
                    server_config,
                    conn_id.clone(),
                    stream_id,
                    method,
                    storage_type,
                    headers,
                    path.unwrap(),
                    content_length,
                    !more_frames,
                );
            } else {
                return;
            }

            if more_frames {
                debug!("returning because more frames");
                return;
            }

            if let Ok(request_event) = guard
                .routes_states()
                .build_request_event(conn_id, stream_id)
            {
                let is_end = request_event.is_end();
                if let Some((request_form, _)) = self
                    .get_routes_from_path_and_method(request_event.path(), request_event.method())
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
                                );
                            }
                        }
                        H3Method::POST => { /*create entry in handler state*/ }
                        _ => {}
                    }
                }
            }
        }
        pub fn get_routes_from_path_and_method_and_request_type(
            &self,
            path: &str,
            methode: H3Method,
            request_type: RequestType,
            cb: impl FnOnce(Option<&RouteForm>),
        ) {
            self.get_routes_from_path(path, |request_coll: Option<&Vec<RouteForm>>| {
                if request_coll.is_none() {
                    cb(None)
                } else {
                    let coll: &Vec<RouteForm> = request_coll.unwrap();
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
        pub fn get_routes_from_path_and_method<'b>(
            &self,
            path: &'b str,
            methode: H3Method,
        ) -> Option<(&RouteForm, Option<Vec<&'b str>>)> {
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
