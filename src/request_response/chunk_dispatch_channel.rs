pub use dispatcher::ChunksDispatchChannel;
pub use dispatcher::{ChunkReceiver, ChunkSender};
mod dispatcher {
    use std::{
        ascii::AsciiExt,
        collections::HashMap,
        error::Error,
        sync::{atomic::AtomicUsize, Arc, Mutex},
    };

    use crossbeam_channel::SendError;
    use dashmap::DashMap;

    use crate::request_response::QueuedRequest;

    pub struct ChunksDispatchChannel {
        inner: Arc<ChunksDispatchChannelInner>,
    }
    impl Clone for ChunksDispatchChannel {
        fn clone(&self) -> Self {
            Self {
                inner: self.inner.clone(),
            }
        }
    }

    impl ChunksDispatchChannel {
        pub fn new() -> Self {
            Self {
                inner: Arc::new(ChunksDispatchChannelInner::new()),
            }
        }
        ///__________________________________
        ///Direct send to the queue
        pub fn send_to_queue(
            &self,
            stream_id: u64,
            scid: &[u8],
            msg: QueuedRequest,
        ) -> Result<(), QueuedRequest> {
            let inner = &*self.inner;

            if let Some(entry) = inner.map.get(&(stream_id, scid.to_vec())) {
                if let Err(e) = entry.0.sender.send(msg) {
                    Err(e.0)
                } else {
                    Ok(())
                }
            } else {
                Err(msg)
            }
        }
        pub fn send_to_high_priority_queue(
            &self,
            stream_id: u64,
            scid: &[u8],
            msg: QueuedRequest,
        ) -> Result<(), QueuedRequest> {
            let inner = &*self.inner;

            if let Some(entry) = inner.map.get(&(stream_id, scid.to_vec())) {
                if let Err(e) = entry.1.sender.send(msg) {
                    warn!("errorStatus 100 seding");
                    Err(e.0)
                } else {
                    Ok(())
                }
            } else {
                Err(msg)
            }
        }

        pub fn streams(&self, client_scid: &[u8]) -> Vec<u64> {
            let inner = &*self.inner;

            let coll = inner
                .map
                .iter()
                .filter(|k| &k.key().1 == client_scid)
                .map(|k| k.key().0)
                .collect();
            coll
        }
        pub fn in_queue(&self, stream_id: u64, scid: &[u8]) -> Result<usize, ()> {
            let inner = &*self.inner;
            if let Some(entry) = inner.map.get(&(stream_id, scid.to_vec())) {
                Ok(entry.2.in_queue())
            } else {
                Err(())
            }
        }
        pub fn try_pop(&self, stream_id: u64, scid: &[u8]) -> Result<QueuedRequest, ()> {
            let inner = &*self.inner;
            if let Some(entry) = inner.map.get(&(stream_id, scid.to_vec())) {
                if let Ok(i) = entry.3.try_recv() {
                    return Ok(i);
                }
                match entry.2.try_recv() {
                    Ok(i) => Ok(i),
                    Err(e) => Err(()),
                }
            } else {
                Err(())
            }
        }

        ///____________________
        ///Inserting a new channel for a given stream + scid.
        ///
        pub fn insert_new_channel(&self, stream_id: u64, scid: &[u8]) {
            let inner = &*self.inner;
            if !inner.already_in_map(stream_id, scid) {
                let chunk_counter_0 = ChunkCounter::new(150);
                let chunk_counter_1 = ChunkCounter::new(1010);
                let chann_0 = crossbeam_channel::unbounded::<QueuedRequest>();
                let chann_1 = crossbeam_channel::unbounded::<QueuedRequest>();

                let sender_0: ChunkSender = ChunkSender::new(chann_0.0, chunk_counter_0.clone());
                let receiver_0: ChunkReceiver = ChunkReceiver::new(chann_0.1, chunk_counter_0);
                let sender_1: ChunkSender = ChunkSender::new(chann_1.0, chunk_counter_1.clone());
                let receiver_1: ChunkReceiver = ChunkReceiver::new(chann_1.1, chunk_counter_1);

                inner.insert(
                    (stream_id, scid.to_vec()),
                    (sender_0, sender_1, receiver_0, receiver_1),
                );
            }
        }
        pub fn get_high_priority_sender(&self, stream_id: u64, scid: &[u8]) -> Option<ChunkSender> {
            let inner = &*self.inner;
            if let Some(entry) = inner.map.get(&(stream_id, scid.to_vec())) {
                Some(entry.1.clone())
            } else {
                None
            }
        }
        pub fn get_low_priority_sender(&self, stream_id: u64, scid: &[u8]) -> Option<ChunkSender> {
            let inner = &*self.inner;
            if let Some(entry) = inner.map.get(&(stream_id, scid.to_vec())) {
                Some(entry.0.clone())
            } else {
                None
            }
        }
    }
    type Ids = (u64, Vec<u8>);
    struct ChunksDispatchChannelInner {
        //2 sender and 2 receiver : one for header, the other for bodies (differents level of
        //  prioriyt)
        map: DashMap<Ids, (ChunkSender, ChunkSender, ChunkReceiver, ChunkReceiver)>,
    }
    impl ChunksDispatchChannelInner {
        fn new() -> Self {
            Self {
                map: DashMap::new(),
            }
        }
        fn insert(
            &self,
            key: Ids,
            value: (ChunkSender, ChunkSender, ChunkReceiver, ChunkReceiver),
        ) {
            if let None = self.map.get(&key) {
                self.map.insert(key, value);
            }
        }
        fn already_in_map(&self, stream_id: u64, scid: &[u8]) -> bool {
            if let None = self.map.get(&(stream_id, scid.to_vec())) {
                false
            } else {
                true
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ChunkSender {
        sender: crossbeam_channel::Sender<QueuedRequest>,
        counter: ChunkCounter,
    }
    impl ChunkSender {
        fn new(sender: crossbeam_channel::Sender<QueuedRequest>, counter: ChunkCounter) -> Self {
            Self { sender, counter }
        }

        ///____________________
        ///Return true if the counter limit of items present in the channel is not reached.
        pub fn is_occupied(&self) -> bool {
            self.counter.is_occupied()
        }
        pub fn in_queue(&self) -> usize {
            self.counter.in_queue()
        }
        pub fn send(
            &self,
            msg: QueuedRequest,
        ) -> Result<(), crossbeam_channel::SendError<QueuedRequest>> {
            self.counter.try_increment();
            self.sender.send(msg)
        }
    }

    pub struct ChunkReceiver {
        receiver: crossbeam_channel::Receiver<QueuedRequest>,
        counter: ChunkCounter,
    }
    impl ChunkReceiver {
        fn new(
            receiver: crossbeam_channel::Receiver<QueuedRequest>,
            counter: ChunkCounter,
        ) -> Self {
            Self { receiver, counter }
        }
        pub fn in_queue(&self) -> usize {
            self.counter.in_queue()
        }
        fn try_recv(&self) -> Result<QueuedRequest, ()> {
            if let Ok(r) = self.receiver.try_recv() {
                self.counter.decrement();
                Ok(r)
            } else {
                Err(())
            }
        }
    }
    #[derive(Debug)]
    pub struct ChunkCounter {
        counter_limit: usize,
        counter: Arc<AtomicUsize>,
    }
    impl ChunkCounter {
        pub fn new(counter_limit: usize) -> Self {
            Self {
                counter_limit,
                counter: Arc::new(AtomicUsize::new(0)),
            }
        }
        pub fn in_queue(&self) -> usize {
            self.counter.load(std::sync::atomic::Ordering::Acquire)
        }
        pub fn is_occupied(&self) -> bool {
            self.counter_limit <= self.counter.load(std::sync::atomic::Ordering::Acquire)
        }
        pub fn try_increment(&self) -> bool {
            if self.counter_limit == self.counter.load(std::sync::atomic::Ordering::Acquire) {
                false
            } else {
                self.counter
                    .fetch_add(1, std::sync::atomic::Ordering::Release);

                true
            }
        }
        pub fn decrement(&self) {
            if self.counter.load(std::sync::atomic::Ordering::Acquire) > 0 {
                let old = self
                    .counter
                    .fetch_sub(1, std::sync::atomic::Ordering::Release);
            }
        }
    }
    impl Clone for ChunkCounter {
        fn clone(&self) -> Self {
            Self {
                counter_limit: self.counter_limit,
                counter: self.counter.clone(),
            }
        }
    }
}
