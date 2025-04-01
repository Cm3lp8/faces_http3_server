#![allow(warnings)]
pub use header_reception::{HeaderMessage, HeaderProcessing};
mod header_reception {
    use std::{sync::Arc, task::Wake};

    use mio::Waker;
    use quiche::h3;

    use crate::{
        file_writer::FileWriterChannel,
        request_response::{ChunkingStation, ChunksDispatchChannel},
        route_handler, RouteHandler, ServerConfig,
    };

    use super::workers::run_prime_processor;

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
    }

    impl<S: Send + Sync + 'static> HeaderProcessing<S> {
        pub fn new(
            route_handler: RouteHandler<S>,
            server_config: Arc<ServerConfig>,
            chunking_station: ChunkingStation,
            waker: Arc<Waker>,
            file_writer_channel: FileWriterChannel,
        ) -> Self {
            Self {
                server_config,
                route_handler,
                incoming_header_channel: crossbeam_channel::unbounded(),
                file_writer_channel,
                chunking_station,
                waker,
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
            run_prime_processor(self.incoming_header_channel.1.clone());
        }
    }
    impl<S: Send + Sync + 'static> Clone for HeaderProcessing<S> {
        fn clone(&self) -> Self {
            Self {
                server_config: self.server_config.clone(),
                route_handler: self.route_handler.clone(),
                incoming_header_channel: self.incoming_header_channel.clone(),
                file_writer_channel: self.file_writer_channel.clone(),
                chunking_station: self.chunking_station.clone(),
                waker: self.waker.clone(),
            }
        }
    }
}

mod workers {
    use super::HeaderMessage;

    pub fn run_prime_processor(receiver: crossbeam_channel::Receiver<HeaderMessage>) {
        std::thread::spawn(move || {
            while let Ok(header_msg) = receiver.recv() {
                warn!("new header in the zone !")
            }
        });
    }
}
