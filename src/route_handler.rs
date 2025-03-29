pub use request_hndlr::RouteHandler;

use crate::request_response::QueuedRequest;
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
        file_writer::{FileWritable, FileWriterChannel},
        request_response::{
            BodyRequest, ChunkSender, ChunkingStation, ChunksDispatchChannel, HeaderRequest,
        },
        route_events::{self, EventType, RouteEvent},
        route_manager::{DataManagement, RouteManagerInner},
        server_config,
        server_init::quiche_http3_server::{self, Client},
        BodyStorage, HeadersColl, RequestResponse, RouteEventListener, ServerConfig,
    };
    use mio::{net::UdpSocket, Waker};
    use quiche::{
        h3::{self, NameValue},
        Connection,
    };

    use self::request_temp_table::RequestsTable;

    use super::*;

    pub struct RouteHandler<S> {
        inner: Arc<Mutex<RouteManagerInner<S>>>,
    }

    impl<S: Send + Sync + 'static> RouteHandler<S> {
        pub fn new(route_mngr_inner: Arc<Mutex<RouteManagerInner<S>>>) -> Self {
            RouteHandler {
                inner: route_mngr_inner,
            }
        }
        pub fn set_intermediate_headers_send(&self, stream_id: u64, client: &Client) {
            let guard = &*self.inner.lock().unwrap();
            let conn_id = client.conn_ref().trace_id();
            guard
                .routes_states()
                .set_intermediate_headers_send(stream_id, conn_id.to_string());
        }
        pub fn send_reception_status_first(
            &self,
            client: &mut quiche_http3_server::QClient,
            response_sender_high: crossbeam_channel::Sender<QueuedRequest>,
            response_sender_low: crossbeam_channel::Sender<QueuedRequest>,
            stream_id: u64,
            conn_id: &str,
            chunk_dispatch_channel: &ChunksDispatchChannel,
        ) -> Result<usize, ()> {
            let guard = &*self.inner.lock().unwrap();
            let scid = client.conn().source_id().as_ref().to_vec();
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
                            /*
                                                        guard
                                                            .routes_states()
                                                            .set_intermediate_headers_send(stream_id, conn_id.to_string());
                            */

                            let sender =
                                chunk_dispatch_channel.insert_new_channel(stream_id, &scid);
                            let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                                stream_id,
                                &scid,
                                headers.clone(),
                                false,
                                None,
                                None,
                                crate::request_response::HeaderPriority::SendHeader100,
                            );
                            if let Err(_) = chunk_dispatch_channel.send_to_high_priority_queue(
                                stream_id,
                                &scid,
                                QueuedRequest::new_header(header_req),
                            ) {
                                error!("Failed to send header_req")
                            }
                            /*
                                                        return quiche_http3_server::send_header(
                                                            client, stream_id, headers, false,
                                                        );
                            */
                        }
                    };
                }
            }
            Ok(0)
        }
        ///
        /// Send a reception status to the client, only if something can be updated.
        ///
        ///
        pub fn send_reception_status(
            &self,
            client: &mut quiche_http3_server::QClient,
            response_sender_high: crossbeam_channel::Sender<QueuedRequest>,
            response_sender_low: crossbeam_channel::Sender<QueuedRequest>,
            stream_id: u64,
            conn_id: &str,
            chunk_dispatch_channel: &ChunksDispatchChannel,
        ) -> Result<usize, ()> {
            let guard = &*self.inner.lock().unwrap();
            let scid = client.conn().source_id().as_ref().to_vec();
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
                            /*
                                                        guard
                                                            .routes_states()
                                                            .set_intermediate_headers_send(stream_id, conn_id.to_string());
                            */

                            let sender =
                                chunk_dispatch_channel.insert_new_channel(stream_id, &scid);
                            let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                                stream_id,
                                &scid,
                                headers.clone(),
                                false,
                                None,
                                None,
                                crate::request_response::HeaderPriority::SendHeader100,
                            );
                            if let Err(_) = chunk_dispatch_channel.send_to_high_priority_queue(
                                stream_id,
                                &scid,
                                QueuedRequest::new_header(header_req),
                            ) {
                                error!("Failed to send header_req")
                            }
                            /*
                                                        return quiche_http3_server::send_header(
                                                            client, stream_id, headers, false,
                                                        );
                            */
                        }
                        {
                            if let Some(body) = reception_status.body() {
                                chunk_dispatch_channel.send_to_queue(
                                    stream_id,
                                    &scid,
                                    QueuedRequest::BodyProgression(BodyRequest::new(
                                        stream_id, conn_id, &scid, 0, body, false,
                                    )),
                                );

                                /*
                                if let Ok(stream_cap) = client.conn().stream_capacity(stream_id) {
                                    if stream_cap >= body.len() {
                                        if let Err(()) = quiche_http3_server::send_body(
                                            client, stream_id, body, false,
                                        ) {
                                            error!("Failed sending progress body status")
                                        }
                                    }
                                } else {
                                    warn!("stream [{stream_id}] is closed");
                                }*/
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
            scid: &[u8],
            conn_id: &str,
            packet: &[u8],
            end: bool,
        ) -> Result<usize, ()> {
            let guard = &*self.inner.lock().unwrap();
            let written_data = guard.routes_states().write_body_packet(
                conn_id.to_owned(),
                scid,
                stream_id,
                packet,
                end,
            );
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
        fn get_routes_from_path(&self, path: &str, cb: impl FnOnce(Option<&Vec<RouteForm<S>>>)) {
            let guard = &*self.inner.lock().unwrap();
            cb(guard.routes_formats().get(path));
        }
        pub fn handle_finished_stream(
            &self,
            conn_id: &str,
            scid: &[u8],
            stream_id: u64,
            client: &mut quiche_http3_server::QClient,
            response_sender_high: crossbeam_channel::Sender<QueuedRequest>,
            response_sender_low: crossbeam_channel::Sender<QueuedRequest>,
            chunking_station: &ChunkingStation,
            waker: &Arc<Waker>,
            last_time: &Arc<Mutex<Duration>>,
        ) {
            let guard = &*self.inner.lock().unwrap();
            if let Ok(route_event) = guard.routes_states().build_route_event(
                conn_id,
                scid,
                stream_id,
                EventType::OnFinished,
            ) {
                let is_end = route_event.is_end();
                if let Some((route_form, _)) =
                    guard.get_routes_from_path_and_method(route_event.path(), route_event.method())
                {
                    match route_form.method() {
                        H3Method::GET => {}
                        H3Method::POST => {
                            chunking_station
                                .get_chunking_dispatch_channel()
                                .insert_new_channel(stream_id, scid);
                            let body_sender = chunking_station
                                .get_chunking_dispatch_channel()
                                .get_low_priority_sender(stream_id, scid);
                            let header_sender = chunking_station
                                .get_chunking_dispatch_channel()
                                .get_high_priority_sender(stream_id, scid);
                            let mut response =
                                if let Some(event_subscriber) = route_form.event_subscriber() {
                                    event_subscriber.on_event(route_event)
                                } else {
                                    None
                                };
                            if let Some(resp) = &mut response {
                                resp.attach_chunk_sender(body_sender.unwrap())
                            }
                            if let Ok((headers, body)) =
                                route_form.build_response(stream_id, scid, conn_id, response)
                            {
                                match body {
                                    Some(body) => {
                                        if !client.headers_send(stream_id) {
                                            let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                                                  stream_id,
                                                   &scid,
                                                 headers.clone(),
                                               false,
                                                Some(body),
                                                header_sender,
                                                   crate::request_response::HeaderPriority::SendAdditionnalHeader,
                                                 );

                                            if let Err(_) = chunking_station
                                                .get_chunking_dispatch_channel()
                                                .send_to_high_priority_queue(
                                                    stream_id,
                                                    &scid,
                                                    QueuedRequest::new_header(header_req),
                                                )
                                            {
                                                error!("Failed to send header_req")
                                            }
                                            if let Err(e) = waker.wake() {
                                                error!("Failed to wake poll [{:?}]", e);
                                            };

                                            /*
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
                                                info!("Send headers [{:?}]", headers);
                                                client.set_headers_send(stream_id, true);
                                                chunking_station.send_response(body, last_time);

                                                if let Err(e) = waker.wake() {
                                                    error!("Failed to wake poll [{:?}]", e);
                                                };
                                            }*/
                                        } else {
                                            chunking_station.send_response(body, last_time);
                                            if let Err(e) = waker.wake() {
                                                error!("Failed to wake poll [{:?}]", e);
                                            };
                                        }
                                    }
                                    None => {
                                        let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                                                  stream_id,
                                                   &scid,
                                                 headers.clone(),
                                               true,
                                            None,
                                            header_sender,
                                                   crate::request_response::HeaderPriority::SendAdditionnalHeader,
                                                 );
                                        if let Err(_) = chunking_station
                                            .get_chunking_dispatch_channel()
                                            .send_to_high_priority_queue(
                                                stream_id,
                                                &scid,
                                                QueuedRequest::new_header(header_req),
                                            )
                                        {
                                            error!("Failed to send header_req")
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
            chunk_dispatch_channel: &ChunksDispatchChannel,
            mut headers: Vec<h3::Header>,
            stream_id: u64,
            client: &mut quiche_http3_server::QClient,
            socket: &mut UdpSocket,
            response_sender_high: crossbeam_channel::Sender<QueuedRequest>,
            response_sender_low: crossbeam_channel::Sender<QueuedRequest>,
            more_frames: bool,
            response_head: &ChunkingStation,
            waker: &Arc<Waker>,
            last_time: &Arc<Mutex<Duration>>,
            file_writer_channel: FileWriterChannel,
        ) {
            let conn_id = client.conn().trace_id().to_string();
            let scid = client.conn().source_id().as_ref().to_vec();
            let mut method: Option<Vec<u8>> = None;
            let mut path: Option<String> = None;
            let mut content_length: Option<usize> = None;

            {
                for hdr in &headers {
                    match hdr.name() {
                        b":method" => method = Some(hdr.value().to_vec()),
                        b":path" => path = Some(String::from_utf8(hdr.value().to_vec()).unwrap()),
                        b"content-length" => {
                            if let Some(method) = &method {
                                if let Ok(method_parsed) = H3Method::parse(&method) {
                                    if H3Method::POST == method_parsed
                                        || H3Method::PUT == method_parsed
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
            }
            if method.is_none() || path.is_none() {
                return;
            }

            let guard = &*self.inner.lock().unwrap();

            let mut data_management: Option<DataManagement> = None;
            let mut event_subscriber: Option<Arc<dyn RouteEventListener + 'static + Send + Sync>> =
                None;
            if let Ok(method) = H3Method::parse(&method.unwrap()) {
                if let Some((route_form, _)) =
                    guard.get_routes_from_path_and_method(path.as_ref().unwrap().as_str(), method)
                {
                    data_management = route_form.data_management_type();
                    event_subscriber = route_form.event_subscriber();
                    event_subscriber.as_ref().unwrap().on_header();
                    let headers_coll: HeadersColl = method.get_headers_for_middleware(&mut headers);
                    route_form.process_middlewares(&headers_coll, &guard.app_state());
                }
                guard.routes_states().add_partial_request(
                    server_config,
                    conn_id.clone(),
                    stream_id,
                    method,
                    data_management,
                    event_subscriber.clone(),
                    &headers,
                    path.unwrap().as_str(),
                    content_length,
                    !more_frames,
                    file_writer_channel,
                );
            } else {
                return;
            }

            if more_frames {
                info!("returning because more frames");
                return;
            }

            if let Ok(route_event) = guard.routes_states().build_route_event(
                conn_id.as_str(),
                &scid,
                stream_id,
                EventType::OnFinished,
            ) {
                chunk_dispatch_channel.insert_new_channel(stream_id, &scid);
                let header_sender =
                    chunk_dispatch_channel.get_high_priority_sender(stream_id, &scid);
                let body_sender = chunk_dispatch_channel.get_low_priority_sender(stream_id, &scid);
                let is_end = route_event.is_end();
                if let Some((route_form, _)) =
                    guard.get_routes_from_path_and_method(route_event.path(), route_event.method())
                {
                    match route_form.method() {
                        H3Method::GET => {
                            /*stream shutdown send response*/

                            let mut response =
                                if let Some(event_subscriber) = route_form.event_subscriber() {
                                    event_subscriber.on_event(route_event)
                                } else {
                                    error!("no event subscribre");
                                    None
                                };

                            if let Some(resp) = &mut response {
                                resp.attach_chunk_sender(body_sender.unwrap());
                            }
                            /*
                                                        client
                                                            .conn()
                                                            .stream_shutdown(stream_id, quiche::Shutdown::Read, 0)
                                                            .unwrap();
                            */
                            if let Ok((headers, body)) = route_form.build_response(
                                stream_id,
                                &scid,
                                conn_id.as_str(),
                                response,
                            ) {
                                match body {
                                    Some(body) => {
                                        let (recv_send_confirmation, header_req) =
                                            HeaderRequest::new(
                                                stream_id,
                                                &scid,
                                                headers.clone(),
                                                false,
                                                Some(body),
                                                header_sender,
                                                crate::request_response::HeaderPriority::SendHeader,
                                            );
                                        if let Err(_) = chunk_dispatch_channel
                                            .send_to_high_priority_queue(
                                                stream_id,
                                                &scid,
                                                QueuedRequest::new_header(header_req),
                                            )
                                        {
                                            error!("Failed to send header_req")
                                        }
                                        info!("sending post request");

                                        /*
                                                                                if let Ok(n) = quiche_http3_server::send_header(
                                                                                    client, socket, stream_id, headers, false,
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
                                        */
                                    }
                                    None => {
                                        let (recv_send_confirmation, header_req) =
                                            HeaderRequest::new(
                                                stream_id,
                                                &scid,
                                                headers.clone(),
                                                is_end,
                                                None,
                                                header_sender,
                                                crate::request_response::HeaderPriority::SendHeader,
                                            );
                                        if let Err(_) = chunk_dispatch_channel
                                            .send_to_high_priority_queue(
                                                stream_id,
                                                &scid,
                                                QueuedRequest::new_header(header_req),
                                            )
                                        {
                                            error!("Failed to send header_req")
                                        }

                                        /*
                                                                                quiche_http3_server::send_response(
                                                                                    client,
                                                                                    stream_id,
                                                                                    headers,
                                                                                    vec![],
                                                                                    is_end,
                                                                                );
                                        */
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
            cb: impl FnOnce(Option<&RouteForm<S>>),
        ) {
            self.get_routes_from_path(path, |request_coll: Option<&Vec<RouteForm<S>>>| {
                if request_coll.is_none() {
                    cb(None)
                } else {
                    let coll: &Vec<RouteForm<S>> = request_coll.unwrap();
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
        ) -> Option<(&RouteForm<S>, Option<Vec<&'b str>>)> {
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
    impl RequestResponse {
        ///________________________________________
        ///Attach a channel sender to the body that corresponds to the associated client.
        ///Sender can be build with :
        ///client.get_response_sender()
        fn attach_chunk_sender(&mut self, sndr: ChunkSender) {
            match self.body_as_mut() {
                crate::request_response::BodyType::Data {
                    stream_id,
                    scid,
                    conn_id,
                    data,
                    sender,
                    conn_stats,
                } => {
                    *sender = Some(sndr);
                }
                crate::request_response::BodyType::FilePath {
                    stream_id,
                    scid,
                    conn_id,
                    file_path,
                    sender,
                    conn_stats,
                } => {
                    *sender = Some(sndr);
                }
                crate::request_response::BodyType::None => {}
            }
        }
    }
}
