pub use response_pool_processing::{ResponsePoolProcessing, ResponsePoolProcessingSender};
mod response_worker;
mod response_pool_processing {
    use std::sync::Arc;

    use mio::Waker;
    use quiche::h3;

    use crate::{
        file_writer::{FileWriter, FileWriterChannel},
        request_response::ChunkingStation,
        H3Method, RouteHandler, ServerConfig,
    };

    use super::response_worker::ResponseThreadPool;

    pub struct ResponseInjection {
        path: String,
        method: H3Method,
        headers: Vec<h3::Header>,
        stream_id: u64,
        scid: Vec<u8>,
    }
    impl ResponseInjection {
        pub fn new(
            path: &str,
            method: H3Method,
            headers: &mut [h3::Header],
            stream_id: u64,
            scid: &[u8],
        ) -> Self {
            Self {
                path: path.to_string(),
                method,
                headers: headers.to_vec(),
                stream_id,
                scid: scid.to_vec(),
            }
        }
    }
    pub struct ResponsePoolProcessing<S> {
        injection_channel: (
            crossbeam_channel::Sender<ResponseInjection>,
            crossbeam_channel::Receiver<ResponseInjection>,
        ),
        route_handler: RouteHandler<S>,
        server_config: Arc<ServerConfig>,
        chunking_station: ChunkingStation,
        waker: Arc<Waker>,
        file_writer_channel: FileWriterChannel,
        app_state: S,
    }
    impl<S: Send + Sync + Clone + 'static> ResponsePoolProcessing<S> {
        pub fn new(
            route_handler: RouteHandler<S>,
            server_config: Arc<ServerConfig>,
            chunking_station: ChunkingStation,
            waker: Arc<Waker>,
            file_writer_channel: FileWriterChannel,
            app_state: S,
        ) -> Self {
            Self {
                injection_channel: crossbeam_channel::unbounded(),
                route_handler,
                server_config,
                chunking_station,
                waker,
                file_writer_channel,
                app_state,
            }
        }
        pub fn run(&self, worker_cb: impl Fn(ResponseInjection) + Send + Sync + 'static) {
            let worker_cb = Arc::new(worker_cb);
            let _ = ResponseThreadPool::new(
                8,
                self.injection_channel.clone(),
                self.app_state.clone(),
                worker_cb,
            );
        }

        pub fn get_response_pool_processing_sender(&self) -> ResponsePoolProcessingSender {
            ResponsePoolProcessingSender {
                sender: self.injection_channel.0.clone(),
            }
        }
    }

    #[derive(Clone)]
    pub struct ResponsePoolProcessingSender {
        sender: crossbeam_channel::Sender<ResponseInjection>,
    }
    impl ResponsePoolProcessingSender {
        pub fn send(
            &self,
            path: &str,
            method: H3Method,
            headers: &mut [h3::Header],
            stream_id: u64,
            scid: &[u8],
        ) {
        }
    }
}
