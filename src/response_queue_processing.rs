pub use response_buffer_table::{ResponseInjectionBuffer, SignalNewRequest};
pub use response_pool_processing::{
    ResponseInjection, ResponsePoolProcessing, ResponsePoolProcessingSender,
};
mod response_buffer_table;
mod response_worker;
mod response_pool_processing {
    use std::sync::Arc;

    use mio::Waker;
    use quiche::h3;
    use rusqlite::ToSql;

    use crate::{
        file_writer::{FileWriter, FileWriterChannel},
        request_response::ChunkingStation,
        H3Method, RouteHandler, ServerConfig,
    };

    use super::{response_worker::ResponseThreadPool, ResponseInjectionBuffer};

    pub struct ResponseInjection {
        stream_id: u64,
        scid: Vec<u8>,
        conn_id: String,
        has_more_frames: bool,
        mio_waker: Arc<mio::Waker>,
    }
    impl ResponseInjection {
        pub fn new(
            stream_id: u64,
            scid: &[u8],
            conn_id: &str,
            has_more_frames: bool,
            mio_waker: Arc<mio::Waker>,
        ) -> Self {
            Self {
                stream_id,
                scid: scid.to_vec(),
                conn_id: conn_id.to_string(),
                has_more_frames,
                mio_waker,
            }
        }
        pub fn req_id(&self) -> (u64, String) {
            (self.stream_id, self.conn_id.to_string())
        }
        pub fn wake(&self) {
            let _ = self.mio_waker.wake();
        }
        /*
        pub fn content_length(&self) -> Option<usize> {
            self.content_length
        }
        pub fn headers(&self) -> &[h3::Header] {
            &self.headers
        }*/
        pub fn stream_id(&self) -> u64 {
            self.stream_id
        }
        pub fn conn_id(&self) -> String {
            self.conn_id.clone()
        }
        pub fn scid(&self) -> Vec<u8> {
            self.scid.to_vec()
        }
        /*
        pub fn path(&self) -> String {
            self.path.to_string()
        }
        pub fn method(&self) -> H3Method {
            self.method
        }*/
        pub fn has_more_frames(&self) -> bool {
            self.has_more_frames
        }
    }
    pub struct ResponsePoolProcessing<S: Sync + Send + 'static> {
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
        response_injection_table: ResponseInjectionBuffer<S>,
    }
    impl<S: Send + Sync + Clone + 'static> ResponsePoolProcessing<S> {
        pub fn new(
            route_handler: RouteHandler<S>,
            server_config: Arc<ServerConfig>,
            chunking_station: ChunkingStation,
            waker: Arc<Waker>,
            file_writer_channel: FileWriterChannel,
            app_state: S,
            response_injection_buffer: &ResponseInjectionBuffer<S>,
        ) -> Self {
            Self {
                injection_channel: crossbeam_channel::unbounded(),
                route_handler,
                server_config,
                chunking_station,
                waker,
                file_writer_channel,
                app_state,
                response_injection_table: response_injection_buffer.clone(),
            }
        }
        pub fn run(
            &self,
            worker_cb: impl Fn(ResponseInjection, &ResponseInjectionBuffer<S>) + Send + Sync + 'static,
        ) {
            let worker_cb = Arc::new(worker_cb);
            let _ = ResponseThreadPool::new(
                8,
                self.injection_channel.clone(),
                self.app_state.clone(),
                worker_cb,
                &self.response_injection_table,
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
            stream_id: u64,
            scid: &[u8],
            conn_id: &str,
            has_more_frames: bool,
            mio_waker: &Arc<mio::Waker>,
        ) -> Result<(), crossbeam_channel::SendError<ResponseInjection>> {
            let mio_waker = mio_waker.clone();
            let response_injection =
                ResponseInjection::new(stream_id, scid, conn_id, has_more_frames, mio_waker);

            self.sender.send(response_injection)
        }
    }
}
