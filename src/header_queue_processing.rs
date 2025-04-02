#![allow(warnings)]
pub use header_reception::{HeaderMessage, HeaderProcessing};
pub use middleware_worker::MiddleWareJob;

mod middleware_worker;
mod header_reception {
    use std::{sync::Arc, task::Wake};

    use mio::Waker;
    use quiche::h3;

    use crate::{
        file_writer::FileWriterChannel,
        request_response::{ChunkingStation, ChunksDispatchChannel},
        route_handler, RouteHandler, ServerConfig,
    };

    use super::{
        middleware_worker::ThreadPool,
        workers::{self, run_prime_processor},
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
    pub struct HeaderProcessing<S> {
        route_handler: RouteHandler<S>,
        server_config: Arc<ServerConfig>,
        chunking_station: ChunkingStation,
        waker: Arc<Waker>,
        incoming_header_channel: (
            crossbeam_channel::Sender<HeaderMessage>,
            crossbeam_channel::Receiver<HeaderMessage>,
        ),
        file_writer_channel: FileWriterChannel,
        workers: Arc<ThreadPool>,
    }

    impl<S: Send + Sync + 'static> HeaderProcessing<S> {
        pub fn new(
            route_handler: RouteHandler<S>,
            server_config: Arc<ServerConfig>,
            chunking_station: ChunkingStation,
            waker: Arc<Waker>,
            file_writer_channel: FileWriterChannel,
        ) -> Self {
            let workers = Arc::new(ThreadPool::new(8));
            Self {
                server_config,
                route_handler,
                incoming_header_channel: crossbeam_channel::unbounded(),
                file_writer_channel,
                chunking_station,
                waker,
                workers,
            }
        }
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
            run_prime_processor(
                self.incoming_header_channel.1.clone(),
                &self.route_handler,
                &self.workers,
            );
        }
    }
    /*
    impl<S: Send + Sync + 'static> Clone for HeaderProcessing<S> {
        fn clone(&self) -> Self {
            Self {
                server_config: self.server_config.clone(),
                route_handler: self.route_handler.clone(),
                incoming_header_channel: self.incoming_header_channel.clone(),
                file_writer_channel: self.file_writer_channel.clone(),
                chunking_station: self.chunking_station.clone(),
                waker: self.waker.clone(),
                workers: self.workers.clone(),
            }
        }
    }*/
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
    use std::{sync::Arc, time::Duration};

    use quiche::h3::{self, NameValue};

    use crate::{
        header_queue_processing::middleware_worker::MiddleWareJob, route_handler, H3Method,
        RouteHandler,
    };

    use super::{
        middleware_process::{ConfirmationRoom, MiddleWareProcess},
        middleware_worker::ThreadPool,
        HeaderMessage,
    };

    pub fn run_prime_processor<S: Send + Sync + 'static>(
        receiver: crossbeam_channel::Receiver<HeaderMessage>,
        route_handler: &RouteHandler<S>,
        header_workers_pool: &Arc<ThreadPool>,
    ) {
        let waker = crossbeam_channel::unbounded::<()>();
        let waker_clone = waker.clone();
        let confirmation_room = ConfirmationRoom::new(waker.0.clone());
        let confirmation_room_clone = confirmation_room.clone();
        let route_handler = route_handler.clone();
        let header_workers_pool = header_workers_pool.clone();
        std::thread::spawn(move || {
            let middleware_process = MiddleWareProcess::new();

            while let Ok(header_msg) = receiver.recv() {
                warn!("new header in the zone !");

                let (method, path, content_length) =
                    extract_method_path_content_length(header_msg.headers());
                if method.is_none() || path.is_none() {
                    continue;
                }

                if let Some(middleware_job) = route_handler.send_header_work(header_msg.clone()) {
                    header_workers_pool.execute(middleware_job);
                }

                let confirmation = middleware_process.extract_header(header_msg);
                confirmation_room.push_back(confirmation);
            }
        });

        std::thread::spawn(move || {
            while let Ok(()) = waker_clone.1.recv() {
                confirmation_room_clone.check_for_completes(|res| {
                    std::thread::sleep(Duration::from_micros(200));
                    /*
                     * */
                });
            }
        });
    }

    fn extract_method_path_content_length(
        headers: &[h3::Header],
    ) -> (Option<Vec<u8>>, Option<String>, Option<usize>) {
        let mut method: Option<Vec<u8>> = None;
        let mut path: Option<String> = None;
        let mut content_length: Option<usize> = None;

        {
            for hdr in headers {
                match hdr.name() {
                    b":method" => method = Some(hdr.value().to_vec()),
                    b":path" => path = Some(String::from_utf8(hdr.value().to_vec()).unwrap()),
                    b"content-length" => {
                        if let Some(method) = &method {
                            if let Ok(method_parsed) = H3Method::parse(&method) {
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
        }
        (method, path, content_length)
    }
}
