#![allow(warnings)]
pub use header_reception::{HeaderMessage, HeaderProcessing};
pub use middleware_worker::{MiddleWareJob, RouteType};
pub use workers::extract_path_from_hdr;

mod middleware_worker;
mod header_reception {
    use std::{sync::Arc, task::Wake};

    use mio::Waker;
    use quiche::h3;

    use crate::{
        file_writer::{FileWriter, FileWriterChannel, WritableItem},
        request_response::{ChunkingStation, ChunksDispatchChannel},
        response_queue_processing::{ResponsePoolProcessingSender, SignalNewRequest},
        route_handler,
        stream_sessions::UserSessions,
        RouteHandler, ServerConfig,
    };

    use super::{
        middleware_worker::ThreadPool,
        workers::{self, run_header_processor},
    };

    #[derive(Clone)]
    pub struct HeaderMessage {
        stream_id: u64,
        scid: Vec<u8>,
        conn_id: String,
        more_frames: bool,
        headers: Vec<h3::Header>,
    }
    impl HeaderMessage {
        pub fn new(
            stream_id: u64,
            scid: Vec<u8>,
            conn_id: String,
            more_frames: bool,
            headers: Vec<h3::Header>,
        ) -> Self {
            Self {
                stream_id,
                scid,
                conn_id,
                more_frames,
                headers,
            }
        }
        pub fn stream_id(&self) -> u64 {
            self.stream_id
        }
        pub fn scid(&self) -> &[u8] {
            &self.scid
        }
        pub fn conn_id(&self) -> String {
            self.conn_id.clone()
        }
        pub fn more_frames(&self) -> bool {
            self.more_frames
        }
        pub fn headers(&self) -> &[h3::Header] {
            &self.headers
        }
    }

    /// Processing headers asyncronously
    pub struct HeaderProcessing<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>> {
        route_handler: RouteHandler<S, T>,
        server_config: Arc<ServerConfig>,
        chunking_station: ChunkingStation,
        waker: Arc<Waker>,
        incoming_header_channel: (
            crossbeam_channel::Sender<HeaderMessage>,
            crossbeam_channel::Receiver<HeaderMessage>,
        ),
        file_writer_manager: Arc<FileWriter>,
        workers: Arc<ThreadPool<T>>,
        response_processing_pool_injector: ResponsePoolProcessingSender,
        response_signal_sender: SignalNewRequest,
    }

    impl<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>> HeaderProcessing<S, T> {
        pub fn new(
            route_handler: RouteHandler<S, T>,
            server_config: Arc<ServerConfig>,
            chunking_station: ChunkingStation,
            waker: Arc<Waker>,
            file_writer_manager: Arc<FileWriter>,
            app_state: S,
            response_processing_pool_injector: ResponsePoolProcessingSender,
            response_signal_sender: SignalNewRequest,
        ) -> Self {
            let workers = Arc::new(ThreadPool::new(
                8,
                app_state,
                &route_handler,
                &server_config,
                &file_writer_manager,
                &response_signal_sender,
                &chunking_station,
                &waker,
            ));
            Self {
                server_config,
                route_handler,
                incoming_header_channel: crossbeam_channel::unbounded(),
                file_writer_manager,
                chunking_station,
                waker,
                workers,
                response_processing_pool_injector,
                response_signal_sender,
            }
        }
        #[inline]
        pub fn process_header(
            &self,
            stream_id: u64,
            scid: Vec<u8>,
            conn_id: String,
            header: Vec<h3::Header>,
            more_frames: bool,
        ) {
            let header_message = HeaderMessage::new(stream_id, scid, conn_id, more_frames, header);

            if let Err(e) = self.incoming_header_channel.0.send(header_message) {
                error!("Failed to send new incoming header");
            }
        }
        pub fn run(&self) {
            run_header_processor(
                self.incoming_header_channel.1.clone(),
                &self.route_handler,
                &self.workers,
                &self.chunking_station,
                &self.waker,
                &self.response_processing_pool_injector,
                &self.response_signal_sender,
            );
        }
    }
}

mod middleware_process {
    use std::{collections::VecDeque, time::Duration};

    use super::HeaderMessage;
    use std::sync::{Arc, Mutex};

    pub enum Projection {
        Pending,
        Done(()),
    }
    pub struct ProgressStatus {
        recv: crossbeam_channel::Receiver<Projection>,
        sender: crossbeam_channel::Sender<Projection>,
    }
    impl ProgressStatus {
        pub fn new(
            channel: (
                crossbeam_channel::Sender<Projection>,
                crossbeam_channel::Receiver<Projection>,
            ),
        ) -> ProgressStatus {
            Self {
                recv: channel.1,
                sender: channel.0,
            }
        }
    }

    pub struct MiddleWareProcess;

    impl MiddleWareProcess {
        pub fn new() -> Self {
            Self
        }
        pub fn extract_header(&self, header: HeaderMessage) -> ProgressStatus {
            let channel = crossbeam_channel::unbounded();
            let _ = channel.0.send(Projection::Pending);
            ProgressStatus::new(channel)
        }
    }

    impl Clone for MiddleWareProcess {
        fn clone(&self) -> Self {
            Self
        }
    }

    /// Queueing the status of the middleware collection process progress.
    #[derive(Clone)]
    pub struct ConfirmationRoom {
        queue: Arc<Mutex<VecDeque<ProgressStatus>>>,
        waker: crossbeam_channel::Sender<()>,
    }
    impl ConfirmationRoom {
        pub fn new(waker: crossbeam_channel::Sender<()>) -> Self {
            Self {
                queue: Arc::new(Mutex::new(VecDeque::new())),
                waker,
            }
        }
        pub fn push_back(&self, confirmation: ProgressStatus) {
            self.queue.lock().unwrap().push_back(confirmation);
            let _ = self.waker.send(());
        }
        pub fn check_for_completes(&self, cb: impl Fn(())) {
            let guard = &mut *self.queue.lock().unwrap();
            let mut index_to_remove: Vec<usize> = vec![];
            let mut waker_send_once = false;
            for (i, it) in guard.iter_mut().enumerate() {
                if let Ok(projection) = it.recv.recv() {
                    match projection {
                        Projection::Pending => {
                            let _ = it.sender.send(Projection::Pending);
                            if !waker_send_once {
                                let _ = self.waker.send(());
                                waker_send_once = true;
                            }
                        }
                        Projection::Done(()) => {
                            cb(());
                            index_to_remove.push(i);
                        }
                    }
                }
            }

            for i in index_to_remove {
                guard.remove(i);
            }
        }
    }
}

mod workers {
    use crate::stream_sessions::StreamManagement;
    use std::{sync::Arc, time::Duration};

    use quiche::h3::{self, NameValue};

    use crate::{
        header_queue_processing::middleware_worker::MiddleWareJob,
        request_response::{ChunkingStation, ChunksDispatchChannel, HeaderPriority},
        response_queue_processing::{self, ResponsePoolProcessingSender, SignalNewRequest},
        route_events::EventType,
        route_handler::{self, send_404, send_error},
        stream_sessions::UserSessions,
        H3Method, MiddleWareResult, RouteHandler,
    };

    use super::{
        middleware_process::{ConfirmationRoom, MiddleWareProcess},
        middleware_worker::ThreadPool,
        HeaderMessage, RouteType,
    };

    pub fn run_header_processor<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>>(
        receiver: crossbeam_channel::Receiver<HeaderMessage>,
        route_handler: &RouteHandler<S, T>,
        header_workers_pool: &Arc<ThreadPool<T>>,
        chunking_station: &ChunkingStation,
        mio_waker: &Arc<mio::Waker>,
        response_processing_pool_sender: &ResponsePoolProcessingSender,
        response_signal_sender: &SignalNewRequest,
    ) {
        let route_handler = route_handler.clone();
        let route_handler_clone = route_handler.clone();
        let header_workers_pool = header_workers_pool.clone();
        let middleware_result_chan = crossbeam_channel::unbounded::<MiddleWareResult>();
        let chunking_station = chunking_station.clone();
        let mio_waker = mio_waker.clone();
        let chunking_station_cl = chunking_station.clone();
        let mio_waker_cl = mio_waker.clone();
        let response_processing_pool_injector = response_processing_pool_sender.clone();
        let stream_sessions = route_handler.stream_sessions();
        let response_signal_sender = response_signal_sender.clone();
        std::thread::spawn(move || {
            while let Ok(header_msg) = receiver.recv() {
                let stream_id = header_msg.stream_id();
                let scid = header_msg.scid();
                let (method, path, content_length) =
                    extract_method_path_content_length(header_msg.headers());
                if method.is_none() || path.is_none() {
                    continue;
                }

                let method = if let Ok(method) = H3Method::parse(&method.unwrap()) {
                    method
                } else {
                    continue;
                };

                let path = path.unwrap();
                if let Some((route_form, _)) =
                    route_handler.get_routes_from_path_and_method(path.as_str(), method)
                {
                    let middleware_coll = (*route_form.clone()).to_middleware_coll();
                    if let Some(middleware_job) = route_handler.send_header_work(
                        path.as_str(),
                        RouteType::Regular,
                        method,
                        content_length,
                        middleware_coll,
                        header_msg.clone(),
                        middleware_result_chan.0.clone(),
                    ) {
                        header_workers_pool.execute(middleware_job);
                    }
                    continue;
                }

                if let Some(stream_sessions) = &stream_sessions {
                    if let Ok(_) = stream_sessions.get_stream_from_path(path.as_str(), |stream| {
                        let middleware_coll = stream.to_middleware_coll();
                        if let Some(middleware_job) = route_handler.send_header_work(
                            path.as_str(),
                            RouteType::Stream,
                            method,
                            content_length,
                            middleware_coll,
                            header_msg.clone(),
                            middleware_result_chan.0.clone(),
                        ) {
                            header_workers_pool.execute(middleware_job);
                        }
                    }) {
                        continue;
                    }
                }
                if response_signal_sender
                    .send_rejected((stream_id, header_msg.conn_id()))
                    .is_err()
                {
                    error!("Failed to signal unknown route rejection");
                }
                route_handler.inner(|guard| {
                    send_404(
                        "/404",
                        guard,
                        &mio_waker_cl,
                        &chunking_station_cl.get_chunking_dispatch_channel(),
                        &chunking_station_cl,
                        &scid,
                        stream_id,
                        EventType::OnFinished,
                        HeaderPriority::SendHeader,
                    );
                });

                //error
            }
        });

        // On  middleware handled
        std::thread::spawn(move || {
            let route_handler = route_handler_clone;
            let chunk_dispatch_channel = chunking_station.get_chunking_dispatch_channel();

            while let Ok(middleware_process_result) = middleware_result_chan.1.recv() {
                match middleware_process_result {
                    MiddleWareResult::Abort {
                        error_response,
                        stream_id,
                        scid,
                    } => {
                        route_handler.inner(|guard| {
                            log::error!("middleware error ... aborting");
                            send_error(
                                error_response,
                                guard,
                                &mio_waker,
                                &chunk_dispatch_channel,
                                &chunking_station,
                                &scid,
                                stream_id,
                                EventType::OnFinished,
                                HeaderPriority::SendHeader,
                            );
                        });
                    }
                    MiddleWareResult::Success {
                        path,
                        method,
                        mut headers,
                        stream_id,
                        scid,
                        conn_id,
                        has_more_frames,
                        content_length,
                    } => {

                        /*
                        response_processing_pool_injector.send(
                            stream_id,
                            &scid,
                            conn_id.as_str(),
                            has_more_frames,
                            &mio_waker,
                        );
                        */
                    }
                    _ => {}
                }
                /*
                 * */
            }
        });
    }

    /// Validate once before admitting a request to the middleware/response queues.
    pub fn extract_path_from_hdr(headers: &[h3::Header]) -> Option<String> {
        parse_request_headers(headers).map(|(_, path, _)| path)
    }

    fn parse_request_headers(headers: &[h3::Header]) -> Option<(Vec<u8>, String, Option<usize>)> {
        let mut method = None;
        let mut path = None;
        let mut content_length = None;
        for header in headers {
            match header.name() {
                b":method" => {
                    if method.is_some() || H3Method::parse(header.value()).is_err() {
                        return None;
                    }
                    method = Some(header.value().to_vec());
                }
                b":path" => {
                    if path.is_some() {
                        return None;
                    }
                    let value = std::str::from_utf8(header.value()).ok()?;
                    if value.is_empty() || value.bytes().any(|b| b <= b' ' || b == 127) {
                        return None;
                    }
                    path = Some(value.to_owned());
                }
                b"content-length" => {
                    if content_length.is_some()
                        || header.value().is_empty()
                        || !header.value().iter().all(u8::is_ascii_digit)
                    {
                        return None;
                    }
                    content_length = Some(std::str::from_utf8(header.value()).ok()?.parse().ok()?);
                }
                _ => {}
            }
        }
        Some((method?, path?, content_length))
    }

    fn extract_method_path_content_length(
        headers: &[h3::Header],
    ) -> (Option<Vec<u8>>, Option<String>, Option<usize>) {
        match parse_request_headers(headers) {
            Some((method, path, length)) => (Some(method), Some(path), length),
            None => (None, None, None),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_length_independently_of_header_order() {
            let headers = [
                h3::Header::new(b"content-length", b"12"),
                h3::Header::new(b":path", b"/upload"),
                h3::Header::new(b":method", b"POST"),
            ];
            assert_eq!(
                parse_request_headers(&headers),
                Some((b"POST".to_vec(), "/upload".into(), Some(12)))
            );
        }

        #[test]
        fn rejects_malformed_headers_without_panicking() {
            for value in [b"\xff".as_slice(), b"", b"/bad path", b"/bad\r\npath"] {
                let headers = [
                    h3::Header::new(b":path", value),
                    h3::Header::new(b":method", b"GET"),
                ];
                assert!(extract_path_from_hdr(&headers).is_none());
            }
            for value in [b"-1".as_slice(), b"+1", b"x", b"184467440737095516160", b""] {
                let headers = [
                    h3::Header::new(b":path", b"/"),
                    h3::Header::new(b":method", b"POST"),
                    h3::Header::new(b"content-length", value),
                ];
                assert!(extract_path_from_hdr(&headers).is_none());
            }
        }

        #[test]
        fn rejects_missing_or_duplicate_routing_headers() {
            assert!(extract_path_from_hdr(&[h3::Header::new(b":path", b"/")]).is_none());
            for duplicate in [
                h3::Header::new(b":path", b"/other"),
                h3::Header::new(b":method", b"POST"),
                h3::Header::new(b"content-length", b"0"),
            ] {
                let mut headers = vec![
                    h3::Header::new(b":path", b"/"),
                    h3::Header::new(b":method", b"GET"),
                    h3::Header::new(b"content-length", b"0"),
                ];
                headers.push(duplicate);
                assert!(extract_path_from_hdr(&headers).is_none());
            }
        }
    }
}
