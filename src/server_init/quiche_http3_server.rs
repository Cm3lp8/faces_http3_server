use crate::conn_statistics::ConnStats;
use crate::request_response::{BodyRequest, QueuedRequest, ResponseQueue};
use crate::stream_sessions::StreamManagement;

pub use self::quiche_implementation::{
    send_body, send_header, send_more_header, send_response, send_response_when_finished,
};
pub use fifo_body::{BodyReqQueue, QueueTrackableItem};
use mio::Token;
use quiche::h3::{self, NameValue};
use quiche::Connection;
use ring::rand::*;
use std::borrow::BorrowMut;
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::hash_map::ValuesMut;
use std::collections::vec_deque::Iter;
use std::collections::{HashMap, VecDeque};
use std::net;
use std::rc::Rc;
mod fifo_body;

pub use Client as QClient;
const MAX_DATAGRAM_SIZE: usize = 1350;
const WAKER_TOKEN: Token = Token(1);
struct PartialResponse {
    headers: Option<Vec<quiche::h3::Header>>,
    body: Vec<u8>,
    written: usize,
}
pub struct Client {
    conn: quiche::Connection,
    http3_conn: Option<quiche::h3::Connection>,
    partial_responses: HashMap<u64, PartialResponse>,
    progress_status_requests: HashMap<u64, u64>,
    headers_sending_tracker: HashMap<u64, bool>,
    body_sending_tracker: HashMap<u64, (usize, usize)>,
    pending_body_queue: BodyReqQueue<QueuedRequest>,
    conn_stats: ConnStats,
}
impl Client {
    pub fn conn_ref(&self) -> &Connection {
        &self.conn
    }
    pub fn conn(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub fn h3_conn(&mut self) -> &mut h3::Connection {
        self.http3_conn.as_mut().unwrap()
    }
    pub fn set_headers_send(&mut self, stream_id: u64, value: bool) {
        self.headers_sending_tracker
            .entry(stream_id)
            .insert_entry(value);
    }
    pub fn headers_send(&self, stream_id: u64) -> bool {
        if let Some(entry) = self.headers_sending_tracker.get(&stream_id) {
            *entry
        } else {
            false
        }
    }
    pub fn check_writable_packet_for_stream(&self, stream_id: u64) -> bool {
        if let Some(_entry) = self.partial_responses.get(&stream_id) {
            true
        } else {
            false
        }
    }
    pub fn set_body_size_to_body_sending_tracker(&mut self, stream_id: u64, body_size: usize) {
        self.body_sending_tracker
            .entry(stream_id)
            .insert_entry((0, body_size));
    }
    pub fn set_written_to_body_sending_tracker(&mut self, stream_id: u64, written: usize) {
        self.body_sending_tracker
            .entry(stream_id)
            .and_modify(|e| e.0 += written);
    }
    pub fn get_written(&self, stream_id: u64) -> (usize, usize) {
        if let Some(entry) = self.body_sending_tracker.get(&stream_id) {
            *entry
        } else {
            (0, 0)
        }
    }

    pub fn is_body_totally_written(&self, stream_id: u64) -> bool {
        if let Some(entry) = self.body_sending_tracker.get(&stream_id) {
            if entry.0 == entry.1 {
                true
            } else {
                false
            }
        } else {
            false
        }
    }
    pub fn get_max_pending_queue_size(&self) -> Option<(u64, usize)> {
        self.pending_body_queue.max_pending_queue()
    }
    pub fn clean_client(&mut self, stream_id: u64) {
        self.headers_sending_tracker.remove(&stream_id);
        self.partial_responses.remove(&stream_id);
        self.progress_status_requests.remove(&stream_id);
    }
}
type ClientMap = HashMap<quiche::ConnectionId<'static>, Client>;

type Scid = Vec<u8>;
type StreamId = u64;
trait PriorityUpdate {
    fn get_pending_buffers_usages(&self) -> HashMap<Scid, HashMap<StreamId, usize>>;
    fn priority_value_mut(&mut self, round: usize) -> Vec<&mut Client> {
        let vec = Vec::new();
        vec
    }
    fn has_too_much_pending(&self, thres: usize) -> bool {
        true
    }
}

impl PriorityUpdate for ClientMap {
    fn get_pending_buffers_usages(&self) -> HashMap<Scid, HashMap<StreamId, usize>> {
        let mut o = HashMap::new();

        for client in self.values() {
            if client.conn_ref().is_closed() {
                continue;
            }
            let scid = client.conn.source_id().to_vec();
            let buffer_size = client.pending_body_queue.buffer_size_per_stream();
            o.insert(scid, buffer_size);
        }
        o
    }
    fn has_too_much_pending(&self, thres: usize) -> bool {
        for (id, client) in self.iter() {
            if let Some((stream_id, queue_size)) = client.get_max_pending_queue_size() {
                if queue_size >= thres {
                    return true;
                }
            }
        }
        false
    }
    fn priority_value_mut(&mut self, round: usize) -> Vec<&mut Client> {
        let mut priority_queue: VecDeque<&mut Client> = VecDeque::new();

        let mut size_track: Vec<(&[u8], usize, &mut Client)> = vec![]; //conn_id, queue_size,
                                                                       //index in vec

        for (id, client) in self.iter_mut() {
            if let Some((stream_id, queue_size)) = client.get_max_pending_queue_size() {
                size_track.push((id, queue_size, client))
            }
        }

        size_track.sort_by_key(|it| Reverse(it.1));

        let v = size_track.into_iter().map(|it| it.2).collect();

        v
    }
}

pub use quiche_implementation::run;
mod quiche_implementation {
    use core::{error, panic};
    use std::{
        alloc::GlobalAlloc,
        borrow::Borrow,
        fs::File,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    use mio::{net::UdpSocket, Waker};
    use quiche::{h3::Priority, ConnectionId};
    use uuid::timestamp::context;

    use crate::{
        file_writer::{self, FileWriter, FileWriterChannel, WritableItem},
        header_queue_processing::{self, HeaderProcessing},
        request_response::{
            BodyRequest, ChunkingStation, ChunksDispatchChannel, HeaderPriority, ResponseQueue,
        },
        response_queue_processing::{
            self, ResponseInjection, ResponseInjectionBuffer, ResponsePoolProcessing,
        },
        route_events::{self, EventType},
        route_handler::{self, response_preparation_with_route_handler},
        server_config::{self, RouteHandler},
        server_init::quiche_http3_server,
        stream_sessions::UserSessions,
        DataManagement, HeadersColl, ResponseBuilderSender, RouteEventListener, RouteManager,
        ServerConfig,
    };

    use super::*;

    pub fn run<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>>(
        server_config: Arc<ServerConfig>,
        route_manager: RouteManager<S, T>,
    ) {
        let mut buf = [0; 65535];
        let mut out = [0; MAX_DATAGRAM_SIZE];

        let mut last_send_instant: Option<Instant> = None;
        let a_socket_time = Arc::new(Mutex::new(Duration::from_micros(100)));
        // Setup the event loop.
        let mut poll = mio::Poll::new().unwrap();
        let mut events = mio::Events::with_capacity(8192 * 16);
        // Create the UDP listening socket, and register it with the event loop.
        let mut socket = mio::net::UdpSocket::bind(*server_config.server_address()).unwrap();
        info!("socket [{:?}]", socket);
        poll.registry()
            .register(
                &mut socket,
                mio::Token(0),
                mio::Interest::READABLE | mio::Interest::WRITABLE,
            )
            .unwrap();

        let waker = Arc::new(Waker::new(poll.registry(), WAKER_TOKEN).unwrap());
        let waker_clone = waker.clone();
        // init the request Response queue with mio waker
        let response_queue = ResponseQueue::<QueuedRequest>::new(waker_clone.clone());
        let file_writer_worker = FileWriter::<WritableItem<File>>::new();
        let file_writer_channel = file_writer_worker.get_file_writer_sender();

        let mut last_time_spend = Arc::new(Mutex::new(Duration::from_micros(100)));
        let mut socket_time = Duration::from_micros(33);
        let chunking_station = ChunkingStation::new(waker_clone.clone(), last_time_spend.clone());

        if let Some(stream_sessions) = route_manager.stream_sessions() {
            stream_sessions.set_chunking_station(&chunking_station);
        }
        let chunk_dispatch_channel = chunking_station.get_chunking_dispatch_channel();

        let response_injection_buffer = ResponseInjectionBuffer::new(
            route_manager.routes_handler(),
            &server_config,
            file_writer_channel.clone(),
            chunking_station.clone(),
            &waker_clone,
        );
        let response_pool_processing = ResponsePoolProcessing::new(
            route_manager.routes_handler(),
            server_config.clone(),
            chunking_station.clone(),
            waker.clone(),
            file_writer_channel.clone(),
            route_manager.routes_handler().app_state(),
            &response_injection_buffer,
        );

        let response_injection = response_pool_processing.get_response_pool_processing_sender();

        let header_queue_processing = HeaderProcessing::new(
            route_manager.routes_handler(),
            server_config.clone(),
            chunking_station.clone(),
            waker.clone(),
            file_writer_channel.clone(),
            route_manager.routes_handler().app_state(),
            response_pool_processing.get_response_pool_processing_sender(),
            response_injection_buffer.get_signal_sender(),
        );

        header_queue_processing.run();
        response_pool_processing.run(move |response_injection, response_injection_buffer| {
            response_injection_buffer.register(response_injection);
        });

        // Create the configuration for the QUIC connections.
        let mut config = quiche::Config::new(quiche::PROTOCOL_VERSION).unwrap();

        config
            .load_cert_chain_from_pem_file(server_config.cert_path())
            .unwrap();
        config
            .load_priv_key_from_pem_file(server_config.key_path())
            .unwrap();
        config
            .set_application_protos(quiche::h3::APPLICATION_PROTOCOL)
            .unwrap();
        config.set_max_idle_timeout(20_000);
        config.set_max_recv_udp_payload_size(MAX_DATAGRAM_SIZE);
        config.set_max_send_udp_payload_size(MAX_DATAGRAM_SIZE);
        config.set_initial_max_data(100_000_000);
        config.set_initial_max_stream_data_bidi_local(200_000_000);
        config.set_initial_max_stream_data_bidi_remote(200_000_000);
        config.set_initial_max_stream_data_uni(100_000_000);
        config.set_initial_max_streams_bidi(100);
        config.set_initial_max_streams_uni(100);
        config.set_disable_active_migration(true);
        config.set_cc_algorithm(quiche::CongestionControlAlgorithm::BBR2);
        config.enable_early_data();
        let h3_config = quiche::h3::Config::new().unwrap();
        let rng = SystemRandom::new();
        let conn_id_seed = ring::hmac::Key::generate(ring::hmac::HMAC_SHA256, &rng).unwrap();
        let mut clients = ClientMap::new();
        let local_addr = socket.local_addr().unwrap();
        let mut total = 0;
        let mut send_packet_time = Instant::now();
        let mut round = 0;
        //let mut last_instant: Option<Instant> = None;
        loop {
            round += 1;
            // Find the shorter timeout from all the active connections.
            //
            // TODO: use event loop that properly supports timers
            let timeout = clients.values().filter_map(|c| c.conn.timeout()).min();
            for client in clients.values() {
                for stream_id in chunk_dispatch_channel.streams(client.conn.source_id().as_ref()) {
                    let written = client.get_written(stream_id);
                    if written.0 < written.1 {
                        let _ = waker_clone.wake();
                        continue;
                    }
                }
            }
            poll.poll(&mut events, timeout).unwrap();
            // Read incoming UDP packets from the socket and feed them to quiche,
            // until there are no more packets to read.

            let mut read_loop_count = 0;
            'read: loop {
                read_loop_count += 1;
                // If the event loop reported no events, it means that the timeout
                // has expired, so handle it without attempting to read packets. We
                // will then proceed with the send loop.
                if events.is_empty() {
                    clients.values_mut().for_each(|c| c.conn.on_timeout());
                    break 'read;
                }
                let (len, from) = match socket.recv_from(&mut buf) {
                    Ok(v) => v,
                    Err(e) => {
                        // There are no more UDP packets to read, so end the read
                        // loop.
                        if e.kind() == std::io::ErrorKind::WouldBlock {
                            debug!("recv() would block");
                            break 'read;
                        }
                        panic!("recv() failed: {:?}", e);
                    }
                };
                debug!("got {} bytes", len);
                let pkt_buf = &mut buf[..len];
                // Parse the QUIC packet's header.
                let hdr = match quiche::Header::from_slice(pkt_buf, quiche::MAX_CONN_ID_LEN) {
                    Ok(v) => v,
                    Err(e) => {
                        error!("Parsing packet header failed: {:?}", e);
                        continue 'read;
                    }
                };
                trace!("got packet {:?}", hdr);
                let conn_id = ring::hmac::sign(&conn_id_seed, &hdr.dcid);
                let conn_id = &conn_id.as_ref()[..quiche::MAX_CONN_ID_LEN];
                let conn_id = conn_id.to_vec().into();
                // Lookup a connection based on the packet's connection ID. If there
                // is no connection matching, create a new one.
                let client = if !clients.contains_key(&hdr.dcid) && !clients.contains_key(&conn_id)
                {
                    if hdr.ty != quiche::Type::Initial {
                        error!("Packet is not Initial");
                        continue 'read;
                    }
                    if !quiche::version_is_supported(hdr.version) {
                        debug!("Doing version negotiation");
                        let len =
                            quiche::negotiate_version(&hdr.scid, &hdr.dcid, &mut out).unwrap();
                        let out = &out[..len];
                        if let Err(e) = socket.send_to(out, from) {
                            if e.kind() == std::io::ErrorKind::WouldBlock {
                                debug!("send() would block");
                                break;
                            }
                            panic!("send() failed: {:?}", e);
                        }
                        continue 'read;
                    }
                    let mut scid = [0; quiche::MAX_CONN_ID_LEN];
                    scid.copy_from_slice(&conn_id);
                    let scid = quiche::ConnectionId::from_ref(&scid);
                    // Token is always present in Initial packets.
                    let token = hdr.token.as_ref().unwrap();
                    // Do stateless retry if the client didn't send a token.
                    if token.is_empty() {
                        debug!("Doing stateless retry");
                        let new_token = mint_token(&hdr, &from);
                        let len = quiche::retry(
                            &hdr.scid,
                            &hdr.dcid,
                            &scid,
                            &new_token,
                            hdr.version,
                            &mut out,
                        )
                        .unwrap();
                        let out = &out[..len];
                        if let Err(e) = socket.send_to(out, from) {
                            if e.kind() == std::io::ErrorKind::WouldBlock {
                                debug!("send() would block");
                                break;
                            }
                            panic!("send() failed: {:?}", e);
                        }
                        continue 'read;
                    }
                    let odcid = validate_token(&from, token);
                    // The token was not valid, meaning the retry failed, so
                    // drop the packet.
                    if odcid.is_none() {
                        error!("Invalid address validation token");
                        continue 'read;
                    }
                    if scid.len() != hdr.dcid.len() {
                        error!("Invalid destination connection ID");
                        continue 'read;
                    }
                    // Reuse the source connection ID we sent in the Retry packet,
                    // instead of changing it again.
                    let scid = hdr.dcid.clone();
                    //debug!("New connection: dcid={:?} scid={:?}", hdr.dcid, scid);
                    debug!("New connection: dcid={:?} scid={:?}", hdr.dcid, scid);
                    let conn = quiche::accept(&scid, odcid.as_ref(), local_addr, from, &mut config)
                        .unwrap();
                    let client = Client {
                        conn,
                        http3_conn: None,
                        partial_responses: HashMap::new(),
                        progress_status_requests: HashMap::new(),
                        headers_sending_tracker: HashMap::new(),
                        body_sending_tracker: HashMap::new(),
                        pending_body_queue: BodyReqQueue::<QueuedRequest>::new(scid.as_ref()),
                        conn_stats: ConnStats::new(),
                    };
                    clients.insert(scid.clone(), client);
                    clients.get_mut(&scid).unwrap()
                } else {
                    match clients.get_mut(&hdr.dcid) {
                        Some(v) => v,
                        None => clients.get_mut(&conn_id).unwrap(),
                    }
                };
                let recv_info = quiche::RecvInfo {
                    to: socket.local_addr().unwrap(),
                    from,
                };
                // Process potentially coalesced packets.
                let read = match client.conn.recv(pkt_buf, recv_info) {
                    Ok(v) => v,
                    Err(e) => {
                        error!("{} recv failed: {:?}", client.conn.trace_id(), e);
                        continue 'read;
                    }
                };
                debug!("{} processed {} bytes", client.conn.trace_id(), read);
                // Create a new HTTP/3 connection as soon as the QUIC connection
                // is established.
                if (client.conn.is_in_early_data() || client.conn.is_established())
                    && client.http3_conn.is_none()
                {
                    debug!(
                        "{} QUIC handshake completed, now trying HTTP/3",
                        client.conn.trace_id()
                    );

                    debug!(
                        "{} QUIC handshake completed, now trying HTTP/3",
                        client.conn.trace_id()
                    );
                    let h3_conn = match quiche::h3::Connection::with_transport(
                        &mut client.conn,
                        &h3_config,
                    ) {
                        Ok(v) => v,
                        Err(e) => {
                            error!("failed to create HTTP/3 connection: {}", e);
                            continue 'read;
                        }
                    };
                    // TODO: sanity check h3 connection before adding to map
                    client.http3_conn = Some(h3_conn);
                }
                if client.http3_conn.is_some() {
                    // Process HTTP/3 events.
                    'h3_read: loop {
                        let http3_conn = client.http3_conn.as_mut().unwrap();
                        match http3_conn.poll(&mut client.conn) {
                            Ok((stream_id, quiche::h3::Event::Headers { list, more_frames })) => {
                                let scid = client.conn.source_id().as_ref().to_vec();
                                let conn_id = client.conn.trace_id().to_string();
                                header_queue_processing.process_header(
                                    stream_id,
                                    scid,
                                    conn_id,
                                    list.clone(),
                                    more_frames,
                                );
                            }
                            Ok((stream_id, quiche::h3::Event::Data)) => {
                                while let Ok(read) =
                                    http3_conn.recv_body(&mut client.conn, stream_id, &mut out)
                                {
                                    let trace_id = client.conn.trace_id().to_string();
                                    let scid = client.conn.source_id().to_vec();
                                    if !route_manager
                                        .is_request_set_in_table(stream_id, trace_id.as_str())
                                    {
                                        route_manager.set_request_in_table(
                                            stream_id,
                                            trace_id.as_str(),
                                            &scid,
                                            &server_config,
                                            &file_writer_channel,
                                        );
                                    }
                                    let scid = client.conn.source_id().as_ref().to_vec();

                                    if let Ok(res) = String::from_utf8(out[..read].to_vec()) {
                                        info!("[{}][{:?}]", stream_id, res);
                                    }
                                    if let Err(e) =
                                        route_manager.routes_handler().write_body_packet(
                                            stream_id,
                                            &scid,
                                            client.conn.trace_id(),
                                            &out[..read],
                                            false,
                                        )
                                    {
                                        error!(
                                            "Failed writing body packet on stream_id [{stream_id}] [{:?}]", e
                                        );
                                    }
                                    total += read;
                                }
                                let trace_id = client.conn.trace_id().to_string();

                                if let Err(_) =
                                    route_manager.routes_handler().send_reception_status(
                                        client,
                                        response_queue.get_high_priority_sender(),
                                        response_queue.get_low_priority_sender(),
                                        stream_id,
                                        trace_id.as_str(),
                                        &chunk_dispatch_channel,
                                    )
                                {
                                    error!("Failed to send progress response status")
                                }
                            }
                            Ok((stream_id, quiche::h3::Event::Finished)) => {
                                let trace_id = client.conn.trace_id().to_owned();
                                let scid = client.conn.source_id().as_ref().to_vec();

                                route_manager.routes_handler().handle_finished_stream(
                                    trace_id.as_str(),
                                    &scid,
                                    stream_id,
                                    &waker_clone,
                                    &response_injection,
                                );
                                ()
                            }
                            Ok((_stream_id, quiche::h3::Event::Reset { .. })) => (),
                            Ok((_prioritized_element_id, quiche::h3::Event::PriorityUpdate)) => {
                                debug!("prioritytupdate");
                                ()
                            }
                            Ok((_goaway_id, quiche::h3::Event::GoAway)) => {
                                debug!("GoAway");
                                ()
                            }
                            Err(quiche::h3::Error::Done) => {
                                break;
                            }
                            Err(e) => {
                                error!("{} HTTP/3 error {:?}", client.conn.trace_id(), e);
                                break;
                            }
                        }
                        // }
                    }
                }
            }

            // Generate outgoing QUIC packets for all active connections and send
            // them on the UDP socket, until quiche reports that there are no more
            // packets to be sent.

            {
                std::thread::sleep(Duration::from_nanos(10));
                for client in clients.values_mut() {
                    for stream_id in client.conn.writable() {
                        // Don't try to pop new chunk from the chunk queue if there are still something
                        // in the pending queue for this stream.

                        while client.conn.stream_writable(stream_id, 512).unwrap() {
                            if client.partial_responses.get(&stream_id).is_some() {
                                if let Some(_val) = handle_writable(client, stream_id) {}
                                if let Err(_e) = waker_clone.wake() {
                                    error!("failed to wake poll");
                                }
                            } else {
                                break;
                            }
                        }
                    }
                    let scid = client.conn.source_id().as_ref().to_vec();
                    let mut writtable = 0;
                    for stream_id in client.conn.writable() {
                        writtable += 1;

                        // Don't try to pop new chunk from the chunk queue if there are still something
                        // in the pending queue for this stream.

                        if !client.pending_body_queue.is_stream_queue_empty(stream_id) {
                            continue;
                        }
                        if client.partial_responses.get(&stream_id).is_some() {
                            continue;
                        }
                        if let Ok(mut request_chunk) =
                            chunk_dispatch_channel.try_pop(stream_id, &scid)
                        {
                            let p_id = request_chunk.packet_id();
                            match request_chunk {
                                QueuedRequest::Body(ref mut content) => {
                                    if let Err(_e) = send_body_response(
                                        client,
                                        content.stream_id(),
                                        content.take_data(),
                                        content.is_end(),
                                    ) {
                                        client.pending_body_queue.push_item_on_front(request_chunk);
                                        info!("pushed [[{}]]", p_id);

                                        if let Err(_e) = waker_clone.wake() {
                                            error!("failed to wake poll");
                                        }
                                    } else {
                                    };
                                    let is_end = client.is_body_totally_written(stream_id);

                                    if is_end {
                                        if let Err(e) =
                                            send_body_response(client, stream_id, vec![], is_end)
                                        {
                                            error!("Failed send end ! [{:?}]", e);
                                        }
                                        client.pending_body_queue.remove_stream_queue(stream_id);
                                    }
                                    if let Err(_e) = waker_clone.wake() {}
                                }
                                QueuedRequest::BodyProgression(ref mut content) => {
                                    let b = content.take_data();
                                    let p = b.clone();
                                    if let Err(_e) = send_body_response_progression(
                                        client,
                                        content.stream_id(),
                                        b,
                                        false,
                                    ) {
                                        client.pending_body_queue.push_item_on_front(request_chunk);
                                        info!("pushed [[{}]]", p_id);
                                    }
                                }
                                QueuedRequest::Header(ref mut content) => {
                                    let headers = content.get_headers().to_vec();
                                    let is_end = content.is_end();
                                    let attached_body_len = content.attached_body_len();

                                    match content.priority_mode() {
                                        HeaderPriority::SendHeader => {
                                            if let Ok(_n) = quiche_http3_server::send_header(
                                                client,
                                                stream_id,
                                                headers.to_vec(),
                                                is_end,
                                            ) {
                                                client.set_headers_send(stream_id, true);
                                                if let Some(body_len) = attached_body_len {
                                                    client.set_body_size_to_body_sending_tracker(
                                                        stream_id, body_len,
                                                    );
                                                    let conn_stats = client.conn_stats.clone();
                                                    content.send_body_to_chunking_station(
                                                        &chunking_station,
                                                        &last_time_spend,
                                                        conn_stats,
                                                    );
                                                }
                                                if let Err(e) = waker.wake() {
                                                    error!("Failed to wake poll [{:?}]", e);
                                                };
                                            } else {
                                                client
                                                    .pending_body_queue
                                                    .push_item_on_front(request_chunk);
                                            }
                                        }
                                        HeaderPriority::SendHeader100 => {
                                            if let Ok(n) = quiche_http3_server::send_header(
                                                client,
                                                stream_id,
                                                headers.to_vec(),
                                                is_end,
                                            ) {
                                                route_manager
                                                    .routes_handler()
                                                    .set_intermediate_headers_send(
                                                        stream_id, client,
                                                    );

                                                /*
                                                if let Err(e) = waker.wake() {
                                                    error!("Failed to wake poll [{:?}]", e);
                                                };*/
                                            } else {
                                                client
                                                    .pending_body_queue
                                                    .push_item_on_front(request_chunk);
                                            }
                                        }
                                        HeaderPriority::SendAdditionnalHeader => {
                                            if let Ok(n) = quiche_http3_server::send_more_header(
                                                client,
                                                stream_id,
                                                headers.clone(),
                                                content.is_end(),
                                            ) {
                                                if let Some(body_len) = attached_body_len {
                                                    client.set_body_size_to_body_sending_tracker(
                                                        stream_id, body_len,
                                                    );
                                                    client.set_headers_send(stream_id, true);

                                                    let conn_stats = client.conn_stats.clone();
                                                    content.send_body_to_chunking_station(
                                                        &chunking_station,
                                                        &last_time_spend,
                                                        conn_stats,
                                                    ); /*
                                                       if let Err(e) = waker.wake() {
                                                           error!(
                                                               "Failed to wake poll [{:?}]",
                                                               e
                                                           );
                                                       };*/
                                                }
                                            } else {
                                                client
                                                    .pending_body_queue
                                                    .push_item_on_front(request_chunk);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }

                    {
                        for stream_id in
                            chunk_dispatch_channel.streams(client.conn.source_id().as_ref())
                        {
                            let written = client.get_written(stream_id);
                            if written.0 < written.1 {
                                let _ = waker_clone.wake();
                                continue;
                            }
                        }
                    }
                }

                for client in clients.values_mut() {
                    'out: loop {
                        if let Some(last_instant) = last_send_instant {
                            while Instant::now() <= last_instant {
                                std::thread::yield_now();
                                continue 'out;
                            }
                        }
                        let (write, send_info) = match client.conn.send(&mut out) {
                            Ok(v) => v,
                            Err(quiche::Error::Done) => {
                                break;
                            }
                            Err(e) => {
                                error!("{} send failed: {:?}", client.conn.trace_id(), e);
                                client.conn.close(false, 0x1, b"fail").ok();
                                break;
                            }
                        };
                        let path_stats = client.conn.path_stats().next().unwrap();
                        client
                            .conn_stats
                            .set_conn_stats(path_stats.rtt, path_stats.cwnd);
                        last_send_instant = Some(send_info.at);

                        if let Err(e) = socket.send_to(&out[..write], send_info.to) {
                            if e.kind() == std::io::ErrorKind::WouldBlock {
                                debug!("send() would block, [{:?}]", e);

                                break;
                            }
                            panic!("send() failed: {:?}", e);
                        }
                        //if let Some(last_instant) = &mut last_instant {
                        debug!("\n{} written {} bytes", client.conn.trace_id(), write);
                        //  println!("{} written {} bytes", client.conn.trace_id(), write);
                    }
                }
            }
            /*
            socket_time = if has_blocked {
                // send_duration.elapsed()
            } else {
                socket_time
            };*/
            *last_time_spend.lock().unwrap() = send_packet_time.elapsed();
            send_packet_time = Instant::now();
            // Garbage collect closed connections.
            clients.retain(|_, ref mut c| {
                if c.conn.is_closed() {
                    debug!(
                        "{} connection collected {:?}",
                        c.conn.trace_id(),
                        c.conn.stats()
                    );
                    if let Some(stream_sessions) = route_manager.stream_sessions() {
                        stream_sessions
                            .clean_closed_connexions(c.conn.source_id().to_vec().as_slice());
                    }
                }
                !c.conn.is_closed()
            });
        }
    }
    /// Generate a stateless retry token.
    ///
    /// The token includes the static string `"quiche"` followed by the IP address
    /// of the client and by the original destination connection ID generated by the
    /// client.
    ///
    /// Note that this function is only an example and doesn't do any cryptographic
    /// authenticate of the token. *It should not be used in production system*.
    fn mint_token(hdr: &quiche::Header, src: &net::SocketAddr) -> Vec<u8> {
        let mut token = Vec::new();
        token.extend_from_slice(b"quiche");
        let addr = match src.ip() {
            std::net::IpAddr::V4(a) => a.octets().to_vec(),
            std::net::IpAddr::V6(a) => a.octets().to_vec(),
        };
        token.extend_from_slice(&addr);
        token.extend_from_slice(&hdr.dcid);
        token
    }
    /// Validates a stateless retry token.
    ///
    /// This checks that the ticket includes the `"quiche"` static string, and that
    /// the client IP address matches the address stored in the ticket.
    ///
    /// Note that this function is only an example and doesn't do any cryptographic
    /// authenticate of the token. *It should not be used in production system*.
    fn validate_token<'a>(
        src: &net::SocketAddr,
        token: &'a [u8],
    ) -> Option<quiche::ConnectionId<'a>> {
        if token.len() < 6 {
            return None;
        }
        if &token[..6] != b"quiche" {
            return None;
        }
        let token = &token[6..];
        let addr = match src.ip() {
            std::net::IpAddr::V4(a) => a.octets().to_vec(),
            std::net::IpAddr::V6(a) => a.octets().to_vec(),
        };
        if token.len() < addr.len() || &token[..addr.len()] != addr.as_slice() {
            return None;
        }
        Some(quiche::ConnectionId::from_ref(&token[addr.len()..]))
    }
    /// Handles incoming HTTP/3 requests.
    pub fn send_header(
        client: &mut Client,
        stream_id: u64,
        headers: Vec<quiche::h3::Header>,
        is_end: bool,
    ) -> Result<usize, ()> {
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        let conn = &mut client.conn;
        if let Err(e) = http3_conn.send_response_with_priority(
            conn,
            stream_id,
            &headers,
            &Priority::new(7, true),
            is_end,
        ) {
            info!(
                "Failed send intermediate 200 response, [{:?}] [{:?}]",
                headers, e
            );
            return Err(());
        }
        Ok(1)
    }
    pub fn send_more_header(
        client: &mut Client,
        stream_id: u64,
        headers: Vec<quiche::h3::Header>,
        is_end: bool,
    ) -> Result<usize, ()> {
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        let conn = &mut client.conn;
        if let Err(e) = http3_conn.send_additional_headers(conn, stream_id, &headers, false, is_end)
        {
        } else {
            return Ok(1);
        }
        Err(())
    }
    pub fn send_body(
        client: &mut Client,
        stream_id: u64,
        body: Vec<u8>,
        is_end: bool,
    ) -> Result<usize, ()> {
        if body.len() == 0 {
            return Ok(0);
        }
        debug!("sending body [{}] bytes", body.len());
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        let conn = &mut client.conn;
        let written = match http3_conn.send_body(conn, stream_id, &body, is_end) {
            Ok(v) => v,
            Err(quiche::h3::Error::Done) => 0,
            Err(e) => {
                error!(
                    "{}-[{}] DStream send failed {:?}",
                    conn.trace_id(),
                    stream_id,
                    e
                );
                return Err(());
            }
        };
        if written < body.len() {
            let response = PartialResponse {
                headers: None,
                body,
                written,
            };
            client.partial_responses.insert(stream_id, response);
        }
        return Ok(written);
        /*
         *
         * Send reponse to the client
         *
         * */
        Err(())
    }
    pub fn send_response_when_finished(
        client: &mut Client,
        stream_id: u64,
        headers: Vec<quiche::h3::Header>,
        body: Vec<u8>,
        is_end: bool,
    ) -> Result<usize, ()> {
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        let conn = &mut client.conn;
        match http3_conn.send_additional_headers(
            conn,
            stream_id,
            &headers,
            false,
            if body.len() == 0 { true } else { false },
        ) {
            Ok(v) => v,
            Err(quiche::h3::Error::StreamBlocked) => {
                let response = PartialResponse {
                    headers: Some(headers),
                    body,
                    written: 0,
                };
                client.partial_responses.insert(stream_id, response);
                return Ok(0);
            }
            Err(e) => {
                error!(
                    "{}, headers [{:#?}] Estream send failed {:?}",
                    conn.trace_id(),
                    headers,
                    e
                );
                return Err(());
            }
        }
        if body.len() == 0 {
            return Ok(0);
        }
        debug!("sending body [{}] bytes", body.len());
        let written = match http3_conn.send_body(conn, stream_id, &body, true) {
            Ok(v) => v,
            Err(quiche::h3::Error::Done) => 0,
            Err(e) => {
                error!("{} Bstream send failed {:?}", conn.trace_id(), e);
                return Err(());
            }
        };
        if written < body.len() {
            let response = PartialResponse {
                headers: None,
                body,
                written,
            };
            client.partial_responses.insert(stream_id, response);
        }
        /*
         *
         * Send reponse to the client
         *
         * */
        Ok(written)
    }
    pub fn send_response(
        client: &mut Client,
        stream_id: u64,
        headers: Vec<quiche::h3::Header>,
        body: Vec<u8>,
        is_end: bool,
    ) -> Result<usize, ()> {
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        let conn = &mut client.conn;
        match http3_conn.send_response_with_priority(
            conn,
            stream_id,
            &headers,
            &Priority::default(),
            is_end,
        ) {
            Ok(v) => v,
            Err(quiche::h3::Error::StreamBlocked) => {
                let response = PartialResponse {
                    headers: Some(headers),
                    body,
                    written: 0,
                };
                client.partial_responses.insert(stream_id, response);
                return Err(());
            }
            Err(e) => {
                error!(
                    "{}, headers [{:#?}] stream send failed {:?}",
                    conn.trace_id(),
                    headers,
                    e
                );
                return Err(());
            }
        }
        if body.len() == 0 {
            return Ok(0);
        }
        debug!("sending body [{}] bytes", body.len());
        let written = match http3_conn.send_body(conn, stream_id, &body, true) {
            Ok(v) => v,
            Err(quiche::h3::Error::Done) => 0,
            Err(e) => {
                error!("{} [{}] Esend failed {:?}", conn.trace_id(), stream_id, e);
                return Err(());
            }
        };
        if written < body.len() {
            let response = PartialResponse {
                headers: None,
                body,
                written,
            };
            client.partial_responses.insert(stream_id, response);
        }
        /*
         *
         * Send reponse to the client
         *
         * */
        Ok(written)
    }
    fn send_body_response_progression(
        client: &mut Client,
        stream_id: u64,
        data: Vec<u8>,
        is_end: bool,
    ) -> Result<usize, ()> {
        debug!("sending body [{}] bytes", data.len());
        let (already_written, total_len) = client.get_written(stream_id);
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        let conn = &mut client.conn;

        let written = match http3_conn.send_body(conn, stream_id, &data, false) {
            Ok(v) => v,
            Err(quiche::h3::Error::Done) => 0,
            Err(e) => {
                error!(
                    " stramcap [{:?} ]is_end [{:?}]  ...{} [{}] Dsend failed {:?}",
                    conn.stream_capacity(stream_id),
                    is_end,
                    conn.trace_id(),
                    stream_id,
                    e
                );
                return Err(());
            }
        };
        if written < data.len() {
            let response = PartialResponse {
                headers: None,
                body: data,
                written,
            };
            client.partial_responses.insert(stream_id, response);
        }
        client.set_written_to_body_sending_tracker(stream_id, written);
        /*
         *
         * Send reponse to the client
         *
         * */
        Ok(written)
    }
    fn send_body_response(
        client: &mut Client,
        stream_id: u64,
        data: Vec<u8>,
        is_end: bool,
    ) -> Result<usize, ()> {
        debug!("sending body [{}] bytes", data.len());
        let (already_written, total_len) = client.get_written(stream_id);
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        let conn = &mut client.conn;

        let written =
            match http3_conn.send_body(conn, stream_id, &data, already_written == total_len) {
                Ok(v) => v,
                Err(quiche::h3::Error::Done) => 0,
                Err(e) => {
                    error!(
                        " stramcap [{:?} ]is_end [{:?}]  ...{} [{}] Dsend failed {:?}",
                        conn.stream_capacity(stream_id),
                        is_end,
                        conn.trace_id(),
                        stream_id,
                        e
                    );
                    return Err(());
                }
            };
        if written < data.len() {
            let response = PartialResponse {
                headers: None,
                body: data,
                written,
            };
            client.partial_responses.insert(stream_id, response);
        }
        client.set_written_to_body_sending_tracker(stream_id, written);
        /*
         *
         * Send reponse to the client
         *
         * */
        Ok(written)
    }
    fn handle_request<S: Send + Sync + 'static, T: UserSessions<Output = T>>(
        client: &mut Client,
        stream_id: u64,
        headers: &[quiche::h3::Header],
        route_handler: &RouteHandler<S, T>,
    ) {
        let conn = &mut client.conn;
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        debug!(
            "{} got request {:?} on stream id {}",
            conn.trace_id(),
            hdrs_to_strings(headers),
            stream_id
        );
        // We decide the response based on headers alone, so stop reading the
        // request stream so that any body is ignored and pointless Data events
        // are not generated.
        //conn.stream_shutdown(stream_id, quiche::Shutdown::Read, 0)
        //   .unwrap();
        let (headers, body) = build_response("", headers, route_handler);
        match http3_conn.send_response(conn, stream_id, &headers, false) {
            Ok(v) => v,
            Err(quiche::h3::Error::StreamBlocked) => {
                let response = PartialResponse {
                    headers: Some(headers),
                    body,
                    written: 0,
                };
                client.partial_responses.insert(stream_id, response);
                return;
            }
            Err(e) => {
                error!("ici stream send failed {:?}", e);
                return;
            }
        }
        let written = match http3_conn.send_body(conn, stream_id, &body, true) {
            Ok(v) => v,
            Err(quiche::h3::Error::Done) => 0,
            Err(e) => {
                error!("{}aa stream send failed {:?}", conn.trace_id(), e);
                return;
            }
        };
        if written < body.len() {
            let response = PartialResponse {
                headers: None,
                body,
                written,
            };
            client.partial_responses.insert(stream_id, response);
        }
    }
    /// Builds an HTTP/3 response given a request.
    fn build_response<S: Send + Sync + 'static, T: UserSessions<Output = T>>(
        root: &str,
        request: &[quiche::h3::Header],
        _request_handler: &RouteHandler<S, T>,
    ) -> (Vec<quiche::h3::Header>, Vec<u8>) {
        let mut file_path = std::path::PathBuf::from(root);
        let mut path = std::path::Path::new("");
        let mut method = None;
        // Look for the request's path and method.
        for hdr in request {
            match hdr.name() {
                b":path" => path = std::path::Path::new(std::str::from_utf8(hdr.value()).unwrap()),
                b":method" => method = Some(hdr.value()),
                _ => (),
            }
        }

        let (status, body) = match method {
            Some(b"GET") => {
                for c in path.components() {
                    if let std::path::Component::Normal(v) = c {
                        file_path.push(v)
                    }
                }
                match std::fs::read(file_path.as_path()) {
                    Ok(data) => (200, data),
                    Err(_) => (404, b"\nHello Error world\n".to_vec()),
                }
            }
            _ => (405, Vec::new()),
        };
        let headers = vec![
            quiche::h3::Header::new(b":status", status.to_string().as_bytes()),
            quiche::h3::Header::new(b"server", b"quiche"),
            quiche::h3::Header::new(b"content-length", body.len().to_string().as_bytes()),
        ];
        (headers, body)
    }
    /// Handles newly writable streams.
    fn handle_writable(client: &mut Client, stream_id: u64) -> Option<bool> {
        let is_end = client.is_body_totally_written(stream_id);
        let amount_written = client.get_written(stream_id);
        let total_to_write = client.get_written(stream_id).1;
        let conn = &mut client.conn;
        let http3_conn = &mut client.http3_conn.as_mut().unwrap();
        debug!("{} stream {} is writable", conn.trace_id(), stream_id);
        if !client.partial_responses.contains_key(&stream_id) {
            warn!("no key");
            return None;
        }
        let resp = client.partial_responses.get_mut(&stream_id).unwrap();
        if let Some(ref headers) = resp.headers {
            match http3_conn.send_response(conn, stream_id, headers, false) {
                Ok(_) => (),
                Err(quiche::h3::Error::StreamBlocked) => {
                    return None;
                }
                Err(e) => {
                    error!(
                        "{} handle writabel stream send failed {:?}",
                        conn.trace_id(),
                        e
                    );
                    return None;
                }
            }
        }
        resp.headers = None;

        let body = if !is_end {
            &resp.body[resp.written..]
        } else {
            &resp.body[resp.written..]
        };
        let written = match http3_conn.send_body(conn, stream_id, body, false) {
            Ok(v) => v,
            Err(quiche::h3::Error::Done) => 0,
            Err(e) => {
                client.partial_responses.remove(&stream_id);
                error!("{} stream send failed {:?}", conn.trace_id(), e);
                return None;
            }
        };
        resp.written += written;
        if resp.written == resp.body.len() {
            {
                debug!("{}/{}", written + amount_written.0, total_to_write);
                if written + amount_written.0 == total_to_write {
                    let _ = match http3_conn.send_body(conn, stream_id, &[], true) {
                        Ok(v) => v,
                        Err(quiche::h3::Error::Done) => 0,
                        Err(e) => {
                            error!("{} stream send failed {:?}", conn.trace_id(), e);
                            return None;
                        }
                    };
                }
            }
            client.set_written_to_body_sending_tracker(stream_id, written);
            client.partial_responses.remove(&stream_id);
            debug!("{:?}", client.get_written(stream_id));
            return Some(true);
        }

        if written > 0 {
            client.set_written_to_body_sending_tracker(stream_id, written);
            return Some(false);
        }

        let o = if resp.written + amount_written.0 == amount_written.1 {
            let _ = match http3_conn.send_body(conn, stream_id, &[], true) {
                Ok(v) => v,
                Err(quiche::h3::Error::Done) => 0,
                Err(e) => {
                    client.partial_responses.remove(&stream_id);
                    error!("{} stream send failed {:?}", conn.trace_id(), e);
                    return None;
                }
            };

            Some(true)
        } else {
            Some(false)
        };
        client.set_written_to_body_sending_tracker(stream_id, written);

        Some(false)
    }
    pub fn hdrs_to_strings(hdrs: &[quiche::h3::Header]) -> Vec<(String, String)> {
        hdrs.iter()
            .map(|h| {
                let name = String::from_utf8_lossy(h.name()).to_string();
                let value = String::from_utf8_lossy(h.value()).to_string();
                (name, value)
            })
            .collect()
    }
    pub fn response_prep<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>>(
        response_injection: ResponseInjection,
        response_injection_buffer: &ResponseInjectionBuffer<S, T>,
    ) {
        response_injection_buffer.register(response_injection);
    }
}
