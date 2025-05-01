pub use chunk_dispatch_channel::{ChunkReceiver, ChunkSender, ChunksDispatchChannel};
pub use request_reponse_builder::{BodyType, RequestResponse};
pub use response_elements::ContentType;
pub use response_elements::Status;
pub use response_queue::{BodyRequest, HeaderPriority, HeaderRequest, QueuedRequest};
pub use response_queue::{ChunkingStation, ResponseQueue};

mod chunk_dispatch_channel;

mod chunking_implementation {
    use core::panic;
    use std::{
        backtrace,
        collections::{HashMap, VecDeque},
        fs::{self, metadata, Metadata},
        io::{BufRead, BufReader, Cursor},
        path::Path,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
        usize,
    };

    use mio::Waker;
    use ring::test::File;

    use crate::conn_statistics::{self, ConnStats};

    use self::chunk_dispatch_channel::ChunkSender;

    use super::*;

    pub struct ChunkableBody {
        stream_id: u64,
        scid: Vec<u8>,
        conn_id: String,
        reader: Box<dyn BufRead + Send + Sync + 'static>,
        sender: ChunkSender,
        body_len: usize,
        packet_id: usize,
        bytes_written: usize,
        conn_stats: Option<ConnStats>,
    }

    impl ChunkableBody {
        pub fn new_data(
            sender: ChunkSender,
            stream_id: u64,
            scid: &[u8],
            conn_id: String,
            data: Vec<u8>,
            conn_stats: Option<ConnStats>,
        ) -> Result<Self, ()> {
            let body_len = data.len();
            Ok(Self {
                stream_id,
                scid: scid.to_vec(),
                conn_id,
                reader: Box::new(BufReader::new(Cursor::new(data))),
                sender,
                body_len,
                packet_id: 0,
                bytes_written: 0,
                conn_stats,
            })
        }
        pub fn new_file(
            sender: ChunkSender,
            scid: &[u8],
            stream_id: u64,
            conn_id: String,
            path: impl AsRef<Path>,
            conn_stats: Option<ConnStats>,
        ) -> Result<Self, ()> {
            if let Ok(file) = std::fs::File::open(path) {
                let body_len = if let Ok(metadata) = file.metadata() {
                    metadata.len() as usize
                } else {
                    0
                };
                Ok(Self {
                    stream_id,
                    conn_id,
                    scid: scid.to_vec(),
                    reader: Box::new(BufReader::new(file)),
                    sender,
                    body_len,
                    packet_id: 0,
                    bytes_written: 0,
                    conn_stats,
                })
            } else {
                Err(())
            }
        }
    }

    const CHUNK_SIZE: usize = 8192;

    type Scid = Vec<u8>;
    type StreamId = u64;

    pub fn run_worker(
        last_time_spend: &Arc<Mutex<Duration>>,
        waker: &Arc<Waker>,
        receiver: crossbeam_channel::Receiver<ChunkableBody>,
        resender: crossbeam_channel::Sender<ChunkableBody>,
        pending_buffer_usage_map: Arc<Mutex<HashMap<Scid, HashMap<StreamId, usize>>>>,
        chunk_station: ChunksDispatchChannel,
    ) {
        let waker = waker.clone();
        let waker_clone = waker.clone();
        let last_time_spend = last_time_spend.clone();
        let mut sended = 0usize;
        type Ids = (u64, Vec<u8>);
        let pending_items: Arc<Mutex<HashMap<Ids, VecDeque<(ChunkSender, QueuedRequest)>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_items_clone = pending_items.clone();
        std::thread::spawn(move || {
            let mut buf_read_high = vec![0; CHUNK_SIZE];
            let mut buf_read_low = vec![0; CHUNK_SIZE];
            let mut low = false;
            let mut buf_read = &mut buf_read_high;

            'read: loop {
                while let Ok(mut chunkable) = receiver.recv() {
                    let start = Instant::now();
                    let last_duration = *last_time_spend.lock().unwrap();

                    if chunkable.sender.is_occupied() {
                        if let Err(_) = resender.send(chunkable) {
                            error!("Failed to resend chunkable body")
                        }
                        std::thread::sleep(Duration::from_micros(20));
                        if let Err(e) = waker.wake() {
                            panic!("error f waking [{:?}]", e)
                        }
                        continue;
                    }
                    if let Some(conn_stats) = &chunkable.conn_stats {
                        if conn_stats.stats().0 > Duration::from_millis(3) {
                            buf_read = &mut buf_read_low;
                        }
                    }
                    if let Ok(n) = chunkable.reader.read(buf_read) {
                        let is_end = if n + chunkable.bytes_written == chunkable.body_len {
                            true
                        } else {
                            false
                        };

                        let queueable_item = QueuedRequest::new_body(BodyRequest::new(
                            chunkable.stream_id,
                            chunkable.conn_id.as_str(),
                            &chunkable.scid,
                            chunkable.packet_id,
                            buf_read[..n].to_vec(),
                            is_end,
                        ));
                        if let Err(e) = chunkable.sender.send(queueable_item) {
                            error!("faield to send queued request")
                        }
                        std::thread::sleep(Duration::from_micros(2));
                        if let Err(e) = waker.wake() {
                            panic!("error f waking [{:?}]", e)
                        }

                        sended += 1;
                        chunkable.packet_id += 1;
                        chunkable.bytes_written += n;
                        if sended % 1000 == 0 {
                            sended = 0;
                        }
                        //repush the unfinished read in the channel
                        if !is_end {
                            if let Err(_) = resender.send(chunkable) {
                                error!("Failed to resend chunkable body")
                            }
                        } else {
                            if let Err(e) = waker.wake() {
                                panic!("error f waking [{:?}]", e)
                            }
                            debug!(
                                "stream [{}]  in queue[{}]SUCCESS all data has been chunked  [{}/{}] !! ",
                                chunkable.stream_id, chunkable.sender.in_queue(),chunkable.bytes_written, chunkable.body_len
                            )
                        }
                    }
                }
            }
        });
    }
}
mod response_queue {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use mio::Waker;
    use quiche::h3;

    use crate::{conn_statistics::ConnStats, server_init::quiche_http3_server::QueueTrackableItem};

    use super::{
        chunk_dispatch_channel::ChunkSender,
        chunking_implementation::{self, ChunkableBody},
        BodyType, ChunksDispatchChannel,
    };

    type Scid = Vec<u8>;
    type StreamId = u64;

    ///____________________________________
    ///Channel to send a body to be chunked before sending to peer.
    #[derive(Clone)]
    pub struct ChunkingStation {
        station_channel: (
            crossbeam_channel::Sender<ChunkableBody>,
            crossbeam_channel::Receiver<ChunkableBody>,
        ),
        waker: Arc<Waker>,
        pending_buffer_usage_map: Arc<Mutex<HashMap<Scid, HashMap<StreamId, usize>>>>,
        chunk_dispatch_channel: ChunksDispatchChannel,
    }

    impl ChunkingStation {
        pub fn new(waker: Arc<Waker>, last_time_spend: Arc<Mutex<Duration>>) -> Self {
            let pending_buffer_usage_map = Arc::new(Mutex::new(HashMap::new()));

            let chunk_dispatch_channel = ChunksDispatchChannel::new();
            let chunking_station = Self {
                station_channel: crossbeam_channel::unbounded(),
                waker,
                pending_buffer_usage_map: pending_buffer_usage_map.clone(),
                chunk_dispatch_channel,
            };

            chunking_implementation::run_worker(
                &last_time_spend,
                &chunking_station.waker,
                chunking_station.station_channel.1.clone(),
                chunking_station.station_channel.0.clone(),
                pending_buffer_usage_map,
                chunking_station.chunk_dispatch_channel.clone(),
            );

            chunking_station
        }

        pub fn get_chunking_dispatch_channel(&self) -> ChunksDispatchChannel {
            self.chunk_dispatch_channel.clone()
        }

        pub fn set_pending_buffers_usage(&self, map: HashMap<Vec<u8>, HashMap<u64, usize>>) {
            *self.pending_buffer_usage_map.lock().unwrap() = map;
        }
        pub fn send_response(&self, msg: BodyType) {
            match msg {
                BodyType::Data {
                    stream_id,
                    scid,
                    conn_id,
                    data,
                    sender,
                    conn_stats,
                } => {
                    if let Ok(c_b) = ChunkableBody::new_data(
                        sender.expect("No sender attached"),
                        stream_id,
                        &scid,
                        conn_id,
                        data,
                        conn_stats,
                    ) {
                        self.station_channel.0.send(c_b);
                    };
                }
                BodyType::FilePath {
                    stream_id,
                    scid,
                    conn_id,
                    file_path,
                    sender,
                    conn_stats,
                } => {
                    if let Ok(c_b) = ChunkableBody::new_file(
                        sender.expect("no sender attached"),
                        &scid,
                        stream_id,
                        conn_id,
                        file_path,
                        conn_stats,
                    ) {
                        if let Err(e) = self.station_channel.0.send(c_b) {
                            error!("failed sending filereader to chunking station [{:?}]", e);
                        }
                    };
                }
                BodyType::None => {}
            }
        }
    }
    pub struct ResponseQueue<T: QueueTrackableItem> {
        low_priority_channel: (crossbeam_channel::Sender<T>, crossbeam_channel::Receiver<T>),
        high_priority_channel: (crossbeam_channel::Sender<T>, crossbeam_channel::Receiver<T>),
        waker: Arc<Waker>,
    }
    impl<T: QueueTrackableItem> Clone for ResponseQueue<T> {
        fn clone(&self) -> Self {
            Self {
                low_priority_channel: self.low_priority_channel.clone(),
                high_priority_channel: self.high_priority_channel.clone(),
                waker: self.waker.clone(),
            }
        }
    }

    impl<T: QueueTrackableItem> ResponseQueue<T> {
        pub fn new(waker: Arc<Waker>) -> Self {
            let low_priority_channel = crossbeam_channel::unbounded::<T>();
            let high_priority_channel = crossbeam_channel::unbounded::<T>();

            Self {
                low_priority_channel,
                high_priority_channel,
                waker,
            }
        }

        pub fn get_low_priority_sender(&self) -> crossbeam_channel::Sender<T> {
            self.low_priority_channel.0.clone()
        }
        pub fn get_high_priority_sender(&self) -> crossbeam_channel::Sender<T> {
            self.high_priority_channel.0.clone()
        }

        pub fn pop_request(&self) -> Result<T, crossbeam_channel::TryRecvError> {
            if let Ok(r) = self.high_priority_channel.1.try_recv() {
                return Ok(r);
            }

            if let Ok(l) = self.low_priority_channel.1.try_recv() {
                return Ok(l);
            }
            Err(crossbeam_channel::TryRecvError::Empty)
        }
    }
    impl QueueTrackableItem for QueuedRequest {
        fn stream_id(&self) -> u64 {
            match self {
                QueuedRequest::Header(content) => content.stream_id,
                QueuedRequest::Body(content) => content.stream_id,
                QueuedRequest::BodyProgression(content) => content.stream_id,
            }
        }
        fn is_last_item(&self) -> bool {
            match self {
                QueuedRequest::Header(content) => content.is_end,
                QueuedRequest::Body(content) => content.is_end,
                QueuedRequest::BodyProgression(content) => content.is_end,
            }
        }
        fn scid(&self) -> Vec<u8> {
            match self {
                QueuedRequest::Header(content) => content.scid.to_vec(),
                QueuedRequest::Body(content) => content.scid.to_vec(),
                QueuedRequest::BodyProgression(content) => content.scid.to_vec(),
            }
        }
        fn len(&self) -> usize {
            match self {
                QueuedRequest::Header(content) => content.get_headers_len(),
                QueuedRequest::Body(content) => content.len(),
                QueuedRequest::BodyProgression(content) => content.len(),
            }
        }
    }

    #[derive(Debug)]
    pub enum QueuedRequest {
        Header(HeaderRequest),
        Body(BodyRequest),
        BodyProgression(BodyRequest),
    }
    impl QueuedRequest {
        pub fn new_body(req: BodyRequest) -> Self {
            Self::Body(req)
        }
        pub fn new_header(req: HeaderRequest) -> Self {
            Self::Header(req)
        }
        pub fn packet_id(&self) -> usize {
            match self {
                Self::Header(header) => 0,
                Self::Body(body) => body.packet_id,
                Self::BodyProgression(body) => body.packet_id,
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    pub enum HeaderPriority {
        SendHeader,
        SendHeader100,
        SendAdditionnalHeader,
        SendWithPriority,
    }
    #[derive(Debug)]
    pub struct HeaderRequest {
        stream_id: u64,
        scid: Vec<u8>,
        headers: Vec<h3::Header>,
        headers_size: usize,
        is_end: bool,
        priority_mode: HeaderPriority,
        send_response: crossbeam_channel::Sender<Result<usize, ()>>,
        chunk_sender: Option<ChunkSender>,
        attached_body: Option<BodyType>,
    }
    impl HeaderRequest {
        const AVERAGE_HEADERS_SIZE: usize = 700;
        ///
        ///____________________________________
        ///Create a new HeaderRequest to send to queue,
        ///attach a body (BodyType) if there is something
        ///you want to send to peer once, once header
        ///send is confirmed.
        ///
        ///
        pub fn new(
            stream_id: u64,
            scid: &[u8],
            headers: Vec<h3::Header>,
            is_end: bool,
            body: Option<BodyType>,
            chunk_sender: Option<ChunkSender>,
            priority_mode: HeaderPriority,
        ) -> (crossbeam_channel::Receiver<Result<usize, ()>>, Self) {
            let channel = crossbeam_channel::bounded::<Result<usize, ()>>(1);
            (
                channel.1.clone(),
                HeaderRequest {
                    stream_id,
                    scid: scid.to_vec(),
                    headers,
                    headers_size: Self::AVERAGE_HEADERS_SIZE,
                    is_end,
                    send_response: channel.0.clone(),
                    chunk_sender,
                    priority_mode,
                    attached_body: body,
                },
            )
        }
        pub fn get_headers(&self) -> &[h3::Header] {
            &self.headers
        }
        pub fn is_end(&self) -> bool {
            self.is_end
        }
        pub fn attached_body_len(&self) -> Option<usize> {
            if let Some(attached_body) = self.attached_body.as_ref() {
                Some(attached_body.bytes_len())
            } else {
                None
            }
        }

        pub fn priority_mode(&self) -> HeaderPriority {
            self.priority_mode
        }
        pub fn send_body_to_chunking_station(
            &mut self,
            chunking_station: &ChunkingStation,
            last_time_spend: &Arc<Mutex<Duration>>,
            conn_stats: ConnStats,
        ) {
            let body_option = self.attached_body.take();
            if let Some(mut body) = body_option {
                body.attach_conn_stats(conn_stats);
                chunking_station.send_response(body);
            }
        }
        pub fn get_headers_len(&self) -> usize {
            self.headers_size
        }
    }
    #[derive(Debug)]
    pub struct BodyRequest {
        packet_id: usize,
        stream_id: u64,
        scid: Vec<u8>,
        conn_id: String,
        packet: Vec<u8>,
        is_end: bool,
    }

    impl BodyRequest {
        pub fn new(
            stream_id: u64,
            conn_id: &str,
            scid: &[u8],
            packet_id: usize,
            packet: Vec<u8>,
            is_end: bool,
        ) -> Self {
            Self {
                stream_id,
                conn_id: conn_id.to_string(),
                scid: scid.to_vec(),
                packet_id,
                packet,
                is_end,
            }
        }
        pub fn stream_id(&self) -> u64 {
            self.stream_id
        }
        pub fn conn_id(&self) -> &str {
            self.conn_id.as_str()
        }
        pub fn scid(&self) -> &[u8] {
            &self.scid
        }
        pub fn take_data(&mut self) -> Vec<u8> {
            std::mem::replace(&mut self.packet, vec![])
        }
        pub fn is_end(&self) -> bool {
            self.is_end
        }
        pub fn packet_id(&self) -> usize {
            self.packet_id
        }
        pub fn len(&self) -> usize {
            self.packet.len()
        }
    }
    impl QueueTrackableItem for BodyRequest {
        fn stream_id(&self) -> u64 {
            self.stream_id
        }
        fn is_last_item(&self) -> bool {
            self.is_end
        }
        fn scid(&self) -> Vec<u8> {
            self.scid.to_vec()
        }
        fn len(&self) -> usize {
            self.packet.len()
        }
    }
}
mod request_reponse_builder {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use quiche::h3::{self, Header, NameValue};

    use crate::conn_statistics::ConnStats;

    use self::chunk_dispatch_channel::ChunkSender;

    use super::*;
    #[derive(Debug)]
    pub enum BodyTypeBuilder {
        Data,
        FilePath,
        None,
    }

    #[derive(Debug, Clone)]
    pub enum BodyType {
        Data {
            stream_id: u64,
            scid: Vec<u8>,
            conn_id: String,
            data: Vec<u8>,
            sender: Option<ChunkSender>,
            conn_stats: Option<ConnStats>,
        },
        FilePath {
            stream_id: u64,
            scid: Vec<u8>,
            conn_id: String,
            file_path: PathBuf,
            sender: Option<ChunkSender>,
            conn_stats: Option<ConnStats>,
        },
        None,
    }

    impl BodyType {
        pub fn new_data(stream_id: u64, scid: &[u8], conn_id: String, data: Vec<u8>) -> Self {
            Self::Data {
                stream_id,
                scid: scid.to_vec(),
                conn_id,
                data,
                sender: None,
                conn_stats: None,
            }
        }
        pub fn new_file(
            stream_id: u64,
            scid: &[u8],
            conn_id: String,
            file_path: impl AsRef<Path>,
        ) -> Self {
            Self::FilePath {
                stream_id,
                scid: scid.to_vec(),
                conn_id,
                file_path: file_path.as_ref().to_path_buf(),
                sender: None,
                conn_stats: None,
            }
        }
        pub fn attach_conn_stats(&mut self, conn_statis: ConnStats) {
            match self {
                Self::Data {
                    stream_id,
                    scid,
                    conn_id,
                    data,
                    sender,
                    conn_stats,
                } => *conn_stats = Some(conn_statis),
                Self::FilePath {
                    stream_id,
                    scid,
                    conn_id,
                    file_path,
                    sender,
                    conn_stats,
                } => {
                    *conn_stats = Some(conn_statis);
                }
                Self::None => {}
            }
        }
        pub fn bytes_len(&self) -> usize {
            match self {
                Self::Data {
                    stream_id,
                    scid,
                    conn_id,
                    data,
                    sender,
                    conn_stats,
                } => data.len(),
                Self::FilePath {
                    stream_id,
                    scid,
                    conn_id,
                    file_path,
                    sender,
                    conn_stats,
                } => std::fs::File::open(file_path)
                    .unwrap()
                    .metadata()
                    .unwrap()
                    .len() as usize,
                Self::None => 0,
            }
        }
    }
    #[derive(Debug)]
    pub struct RequestResponse {
        status: h3::Header,
        content_length: Option<h3::Header>,
        content_type: Option<h3::Header>,
        custom: Vec<h3::Header>,
        body: BodyType,
    }

    impl RequestResponse {
        pub fn new() -> ResponseBuilder {
            ResponseBuilder::new()
        }
        pub fn body_as_mut(&mut self) -> &mut BodyType {
            &mut self.body
        }
        pub fn new_ok_200(stream_id: u64, scid: &[u8], conn_id: &str) -> RequestResponse {
            ResponseBuilder::new()
                .set_stream_id(stream_id)
                .set_scid(scid)
                .set_conn_id(conn_id)
                .set_status(Status::Ok(200))
                .build()
                .unwrap()
        }
        pub fn header(mut self, name: &str, value: &str) -> Self {
            self.custom
                .push(h3::Header::new(name.as_bytes(), value.as_bytes()));
            self
        }

        ///
        /// Respond to client with a file from fs.
        ///
        pub fn new_200_with_file(
            stream_id: u64,
            scid: &[u8],
            conn_id: &str,
            path: impl AsRef<Path>,
        ) -> RequestResponse {
            ResponseBuilder::new()
                .set_stream_id(stream_id)
                .set_conn_id(conn_id)
                .set_scid(scid)
                .set_status(Status::Ok(200))
                .set_path(path)
                .build()
                .unwrap()
        }
        pub fn from_header_200(
            stream_id: u64,
            scid: &[u8],
            conn_id: &str,
            headers: Vec<h3::Header>,
        ) -> RequestResponse {
            ResponseBuilder::new()
                .set_stream_id(stream_id)
                .set_conn_id(conn_id)
                .set_scid(scid)
                .set_status(Status::Ok(200))
                .extends_headers(headers)
                .set_body(vec![1])
                .build()
                .unwrap()
        }
        ///
        /// Respond to client with in memory data
        ///
        pub fn new_200_with_data(
            stream_id: u64,
            scid: &[u8],
            conn_id: &str,
            data: Vec<u8>,
        ) -> RequestResponse {
            ResponseBuilder::new()
                .set_stream_id(stream_id)
                .set_scid(scid)
                .set_conn_id(conn_id)
                .set_status(Status::Ok(200))
                .set_body(data)
                .build()
                .unwrap()
        }
        ///
        /// Respond to client with in  json memory data
        ///
        pub fn new_200_with_json(
            stream_id: u64,
            scid: &[u8],
            conn_id: &str,
            data: Vec<u8>,
        ) -> RequestResponse {
            ResponseBuilder::new()
                .set_stream_id(stream_id)
                .set_scid(scid)
                .set_conn_id(conn_id)
                .set_status(Status::Ok(200))
                .extends_headers(vec![h3::Header::new(b"content-type", b"application/json")])
                .set_body(data)
                .build()
                .unwrap()
        }
        /// Conflict in database
        pub fn new_409_with_data(
            stream_id: u64,
            scid: &[u8],
            conn_id: &str,
            data: Vec<u8>,
        ) -> RequestResponse {
            ResponseBuilder::new()
                .set_stream_id(stream_id)
                .set_scid(scid)
                .set_conn_id(conn_id)
                .set_status(Status::Err(409))
                .extends_headers(vec![h3::Header::new(b"content-type", b"application/json")])
                .set_body(data)
                .build()
                .unwrap()
        }
        /// Service unavailable
        pub fn new_503_with_data(
            stream_id: u64,
            scid: &[u8],
            conn_id: &str,
            data: Vec<u8>,
        ) -> RequestResponse {
            ResponseBuilder::new()
                .set_stream_id(stream_id)
                .set_scid(scid)
                .set_conn_id(conn_id)
                .set_status(Status::Err(503))
                .extends_headers(vec![h3::Header::new(b"content-type", b"application/json")])
                .set_body(data)
                .build()
                .unwrap()
        }
        /// Unauthorized
        pub fn new_401_with_data(
            stream_id: u64,
            scid: &[u8],
            conn_id: &str,
            data: Vec<u8>,
        ) -> RequestResponse {
            ResponseBuilder::new()
                .set_stream_id(stream_id)
                .set_scid(scid)
                .set_conn_id(conn_id)
                .set_status(Status::Err(401))
                .extends_headers(vec![h3::Header::new(b"content-type", b"application/json")])
                .set_body(data)
                .build()
                .unwrap()
        }
        pub fn content_length(&self) -> String {
            if let Some(content_len) = &self.content_length {
                String::from_utf8_lossy(content_len.value()).to_string()
            } else {
                "0".to_owned()
            }
        }
        pub fn with_custom_headers(
            &self,
            custom_headers: Option<impl FnOnce() -> Vec<h3::Header>>,
        ) -> Vec<h3::Header> {
            let mut headers: Vec<h3::Header> = vec![self.status.clone()];
            if let Some(content_type) = &self.content_type {
                headers.push(content_type.clone());
            }

            if let Some(content_length) = &self.content_length {
                headers.push(content_length.clone());
            }

            if !self.custom.is_empty() {
                headers.extend(self.custom.clone());
            }

            if let Some(custom_hdr_cb) = custom_headers {
                headers.extend(custom_hdr_cb());
            }

            headers
        }
        pub fn get_headers(&self) -> Vec<h3::Header> {
            let mut headers: Vec<h3::Header> = vec![self.status.clone()];
            if let Some(content_type) = &self.content_type {
                headers.push(content_type.clone());
            }
            if !self.custom.is_empty() {
                headers.extend(self.custom.clone());
            }

            if let Some(content_length) = &self.content_length {
                headers.push(content_length.clone());
            }

            headers
        }

        pub fn take_body(&mut self) -> BodyType {
            std::mem::replace(&mut self.body, BodyType::None)
        }

        fn new_response(&self) -> ResponseBuilder {
            ResponseBuilder::new()
        }
    }

    pub struct ResponseBuilder {
        stream_id: Option<u64>,
        conn_id: Option<String>,
        scid: Option<Vec<u8>>,
        status: Option<Status>,
        content_length: Option<usize>,
        content_type: Option<ContentType>,
        custom: Vec<h3::Header>,
        body: Option<Vec<u8>>,
        path: Option<PathBuf>,
        body_type: BodyTypeBuilder,
    }
    impl ResponseBuilder {
        pub fn new() -> Self {
            Self {
                stream_id: None,
                conn_id: None,
                scid: None,
                status: None,
                content_length: None,
                content_type: None,
                custom: vec![],
                body: None,
                path: None,
                body_type: BodyTypeBuilder::None,
            }
        }
        pub fn extends_headers(&mut self, headers: Vec<h3::Header>) -> &mut Self {
            let (status, others): (Vec<Header>, Vec<Header>) = headers
                .clone()
                .into_iter()
                .partition(|hdr| hdr.name() == b"status");
            if status.len() > 0 {
                self.status = Some(Status::from_bytes(status[0].value()));
                self.custom.extend(others);
            } else {
                self.custom.extend(headers);
            }

            self
        }
        pub fn set_stream_id(&mut self, stream_id: u64) -> &mut Self {
            self.stream_id = Some(stream_id);
            self
        }
        pub fn set_scid(&mut self, scid: &[u8]) -> &mut Self {
            self.scid = Some(scid.to_vec());
            self
        }
        pub fn set_conn_id(&mut self, conn_id: &str) -> &mut Self {
            self.conn_id = Some(conn_id.to_owned());
            self
        }
        pub fn set_status(&mut self, status: Status) -> &mut Self {
            self.status = Some(status);
            self
        }

        pub fn set_path(&mut self, path: impl AsRef<Path>) -> &mut Self {
            self.path = Some(path.as_ref().to_path_buf());
            self.body_type = BodyTypeBuilder::FilePath;
            self
        }

        pub fn set_body(&mut self, body: Vec<u8>) -> &mut Self {
            self.content_length = Some(body.len());
            self.body = Some(body);
            self.body_type = BodyTypeBuilder::Data;
            self
        }
        pub fn set_content_type(&mut self, content_type: ContentType) -> &mut Self {
            self.content_type = Some(content_type);
            self
        }

        pub fn build(&mut self) -> Result<RequestResponse, ()> {
            if self.status.is_none() {
                return Err(());
            }

            let status = h3::Header::new(
                b":status",
                self.status
                    .take()
                    .unwrap()
                    .get_status_to_string()
                    .as_bytes(),
            );

            let content_type = if let Some(content_type) = &self.content_type {
                let content_type =
                    h3::Header::new(b"content-type", content_type.to_string().as_bytes());
                Some(content_type)
            } else {
                None
            };
            let mut content_length: Option<h3::Header> = None;
            if let Some(stream_id) = self.stream_id {
                if let Some(conn_id) = self.conn_id.take() {
                    if let Some(scid) = self.scid.take() {
                        let body = match self.body_type {
                            BodyTypeBuilder::Data => {
                                let body: Vec<u8> = if let Some(mut data) = self.body.as_mut() {
                                    std::mem::replace(&mut data, vec![])
                                } else {
                                    vec![]
                                };
                                content_length = Some(h3::Header::new(
                                    b"content-length",
                                    body.len().to_string().as_bytes(),
                                ));
                                BodyType::new_data(stream_id, &scid, conn_id, body)
                            }
                            BodyTypeBuilder::FilePath => {
                                let file_path: PathBuf = if let Some(file_path) = &mut self.path {
                                    std::mem::replace(file_path, PathBuf::new())
                                } else {
                                    PathBuf::new()
                                };
                                let file_len =
                                    if let Ok(meta_data) = fs::metadata(file_path.as_path()) {
                                        if meta_data.is_file() {
                                            meta_data.len()
                                        } else {
                                            error!("Not a file ");
                                            0
                                        }
                                    } else {
                                        0
                                    };

                                content_length = Some(h3::Header::new(
                                    b"content-length",
                                    file_len.to_string().as_bytes(),
                                ));
                                BodyType::new_file(stream_id, &scid, conn_id, file_path.as_path())
                            }
                            BodyTypeBuilder::None => BodyType::None,
                        };
                        Ok(RequestResponse {
                            status,
                            content_type,
                            content_length,
                            custom: std::mem::replace(&mut self.custom, vec![]),
                            body,
                        })
                    } else {
                        Err(())
                    }
                } else {
                    Err(())
                }
            } else {
                Err(())
            }
        }
    }
}

mod response_elements {
    use super::*;

    #[derive(Clone, Copy, Debug)]
    pub enum Status {
        Ok(usize),
        Err(usize),
    }

    impl Status {
        pub fn get_status_to_string(&self) -> String {
            let status = match self {
                Status::Ok(value) => value.to_string(),
                Status::Err(value) => value.to_string(),
            };

            status
        }
        pub fn from_bytes(bytes: &[u8]) -> Self {
            let code = String::from_utf8_lossy(bytes).parse::<usize>().unwrap();

            if code == 200 {
                Self::Ok(code)
            } else {
                Self::Err(code)
            }
        }
        pub fn get_status(self) -> Self {
            self
        }
    }

    ///
    ///
    /// Enum for Building the response. The tuple variants let the user specify a custom type
    /// definition that will be recognized on the client side.
    ///
    ///
    pub enum ContentType {
        Text,
        JSon,
        Audio(String),
        Custom(String),
    }
    impl ContentType {
        pub fn to_string(&self) -> String {
            match self {
                Self::Text => "text".to_string(),
                Self::JSon => "application/json".to_string(),
                ContentType::Audio(format) => format!("audio/{:?}", format),
                Self::Custom(format) => format!("custom/{:?}", format),
            }
        }
        pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
            match bytes {
                b"text" => Some(Self::Text),
                &_ => {
                    let val = String::from_utf8_lossy(bytes).to_string();

                    if let Some((pre, suf)) = val.split_once("/") {
                        if pre == "custom" {
                            return Some(Self::Custom(suf.to_string()));
                        }
                        return None;
                    };
                    None
                }
            };
            None
        }
    }
}

/*
*
*
                    let headers = vec![
                        h3::Header::new(b":status", b"200"),
                        h3::Header::new(b"alt-svc", b"h3=\":3000\""),
                        h3::Header::new(b"content-type", &content_type),
                        h3::Header::new(b"content-lenght", body.len().to_string().as_bytes()),
                    ];
*
* */
