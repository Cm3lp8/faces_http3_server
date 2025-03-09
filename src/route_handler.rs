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
        time::Duration,
    };

    use crate::{
        request_response::ResponseHead,
        route_events::{self, EventType, RouteEvent},
        route_manager::{DataManagement, RouteManagerInner},
        server_config,
        server_init::quiche_http3_server::{self, Client},
        BodyStorage, RouteEventListener, ServerConfig,
    };
    use mio::Waker;
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
            response_head: &ResponseHead,
            waker: &Arc<Waker>,
            last_time: &Arc<Mutex<Duration>>,
        ) {
            let guard = &*self.inner.lock().unwrap();
            if let Ok(route_event) =
                guard
                    .routes_states()
                    .build_route_event(conn_id, stream_id, EventType::OnFinished)
            {
                let is_end = route_event.is_end();
                if let Some((route_form, _)) =
                    guard.get_routes_from_path_and_method(route_event.path(), route_event.method())
                {
                    match route_form.method() {
                        H3Method::GET => {
                            /*stream shutdown send response*/
                            let response =
                                if let Some(event_subscriber) = route_form.event_subscriber() {
                                    event_subscriber.on_event(route_event)
                                } else {
                                    None
                                };

                            client
                                .conn()
                                .stream_shutdown(stream_id, quiche::Shutdown::Read, 0)
                                .unwrap();
                            if let Ok((headers, body)) =
                                route_form.build_response(stream_id, conn_id, response)
                            {
                                match body {
                                    Some(body) => {
                                        if !client.headers_send(stream_id) {
                                            info!("header sendttt stream ([{stream_id}])");
                                            if let Ok(n) = quiche_http3_server::send_more_header(
                                                client,
                                                stream_id,
                                                headers.clone(),
                                                false,
                                            ) {
                                                client.set_body_size_to_body_sending_tracker(
                                                    stream_id,
                                                    body.bytes_len(),
                                                );
                                                client.set_headers_send(stream_id, true);
                                            }
                                        } else {
                                            response_head.send_response(body, last_time);
                                            if let Err(e) = waker.wake() {
                                                error!("Failed to wake poll [{:?}]", e);
                                            };
                                        }
                                    }
                                    None => {
                                        if let Err(_) = quiche_http3_server::send_response(
                                            client,
                                            stream_id,
                                            headers,
                                            vec![],
                                            is_end,
                                        ) {
                                            error!("Failed sending h3 response after GET request")
                                        }
                                    }
                                }
                            }
                        }
                        H3Method::POST => {
                            let response =
                                if let Some(event_subscriber) = route_form.event_subscriber() {
                                    event_subscriber.on_event(route_event)
                                } else {
                                    None
                                };
                            if let Ok((headers, body)) =
                                route_form.build_response(stream_id, conn_id, response)
                            {
                                match body {
                                    Some(body) => {
                                        if !client.headers_send(stream_id) {
                                            if let Ok(n) = quiche_http3_server::send_more_header(
                                                client,
                                                stream_id,
                                                headers.clone(),
                                                false,
                                            ) {
                                                client.set_body_size_to_body_sending_tracker(
                                                    stream_id,
                                                    body.bytes_len(),
                                                );
                                                client.set_headers_send(stream_id, true);
                                                response_head.send_response(body, last_time);
                                                if let Err(e) = waker.wake() {
                                                    error!("Failed to wake poll [{:?}]", e);
                                                };
                                            }
                                        } else {
                                            response_head.send_response(body, last_time);
                                            if let Err(e) = waker.wake() {
                                                error!("Failed to wake poll [{:?}]", e);
                                            };
                                        }
                                    }
                                    None => {
                                        if let Err(_) =
                                            quiche_http3_server::send_response_when_finished(
                                                client,
                                                stream_id,
                                                headers,
                                                vec![],
                                                true,
                                            )
                                        {
                                            error!(
                                        "Failed to send response after a post request [{:?}]",
                                        stream_id
                                    )
                                        } else {
                                            info!("shutdown");
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
            response_head: &ResponseHead,
            waker: &Arc<Waker>,
            last_time: &Arc<Mutex<Duration>>,
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

            let mut data_management: Option<DataManagement> = None;
            let mut event_subscriber: Option<Arc<dyn RouteEventListener + 'static + Send + Sync>> =
                None;
            if let Ok(method) = H3Method::parse(method.unwrap()) {
                if let Some((route_form, _)) =
                    guard.get_routes_from_path_and_method(path.unwrap(), method)
                {
                    data_management = route_form.data_management_type();
                    event_subscriber = route_form.event_subscriber();
                }
                guard.routes_states().add_partial_request(
                    server_config,
                    conn_id.clone(),
                    stream_id,
                    method,
                    data_management,
                    event_subscriber,
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

            if let Ok(route_event) = guard.routes_states().build_route_event(
                conn_id.as_str(),
                stream_id,
                EventType::OnHeader,
            ) {
                let is_end = route_event.is_end();
                if let Some((route_form, _)) =
                    self.get_routes_from_path_and_method(route_event.path(), route_event.method())
                {
                    match route_form.method() {
                        H3Method::GET => {
                            /*stream shutdown send response*/

                            let response =
                                if let Some(event_subscriber) = route_form.event_subscriber() {
                                    event_subscriber.on_finished(route_event)
                                } else {
                                    None
                                };
                            client
                                .conn()
                                .stream_shutdown(stream_id, quiche::Shutdown::Read, 0)
                                .unwrap();
                            if let Ok((headers, body)) =
                                route_form.build_response(stream_id, conn_id.as_str(), response)
                            {
                                match body {
                                    Some(body) => {
                                        if let Ok(n) = quiche_http3_server::send_more_header(
                                            client, stream_id, headers, false,
                                        ) {
                                            if let Err(e) = waker.wake() {
                                                error!("Failed to wake poll [{:?}]", e);
                                            };
                                            response_head.send_response(body, last_time);
                                            if let Err(e) = waker.wake() {
                                                error!("Failed to wake poll [{:?}]", e);
                                            };
                                        }
                                    }
                                    None => {
                                        quiche_http3_server::send_response(
                                            client,
                                            stream_id,
                                            headers,
                                            vec![],
                                            is_end,
                                        );
                                    }
                                }
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
                    if let Some(found_request) = coll.iter().find(|item| item.method() == &methode)
                    {
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
