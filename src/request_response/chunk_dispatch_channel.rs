pub use dispatcher::ChunksDispatchChannel;
pub use dispatcher::{ChunkReceiver, ChunkSender};
mod dispatcher {
    use std::{
        ascii::AsciiExt,
        collections::HashMap,
        error::Error,
        sync::{Arc, Mutex},
    };

    use crossbeam_channel::SendError;

    use crate::request_response::QueuedRequest;

    pub struct ChunksDispatchChannel {
        inner: Arc<Mutex<ChunksDispatchChannelInner>>,
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
                inner: Arc::new(Mutex::new(ChunksDispatchChannelInner::new())),
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
            let guard = &*self.inner.lock().unwrap();

            if let Some(entry) = guard.map.get(&(stream_id, scid.to_vec())) {
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
            let guard = &*self.inner.lock().unwrap();

            if let Some(entry) = guard.map.get(&(stream_id, scid.to_vec())) {
                if let Err(e) = entry.1.sender.send(msg) {
                    Err(e.0)
                } else {
                    Ok(())
                }
            } else {
                Err(msg)
            }
        }

        pub fn streams(&self, client_scid: &[u8]) -> Vec<u64> {
            let guard = &*self.inner.lock().unwrap();

            let coll = guard
                .map
                .keys()
                .filter(|k| &k.1 == client_scid)
                .map(|k| k.0)
                .collect();
            coll
        }
        pub fn try_pop(&self, stream_id: u64, scid: &[u8]) -> Result<QueuedRequest, ()> {
            let guard = &*self.inner.lock().unwrap();
            if let Some((_, _, lower_priority, higher_priority)) =
                guard.map.get(&(stream_id, scid.to_vec()))
            {
                if let Ok(i) = higher_priority.try_recv() {
                    warn!("\nhdr [{:?}]\n", i);
                    return Ok(i);
                }
                match lower_priority.try_recv() {
                    Ok(i) => Ok(i),
                    Err(e) if e.is_disconnected() => {
                        warn!("is_disctonneted");
                        Err(())
                    }
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
            let guard = &mut *self.inner.lock().unwrap();
            if !guard.already_in_map(stream_id, scid) {
                let chann_0 = crossbeam_channel::unbounded::<QueuedRequest>();
                let chann_1 = crossbeam_channel::unbounded::<QueuedRequest>();

                let sender_0: ChunkSender = ChunkSender::new(chann_0.0);
                let receiver_0: ChunkReceiver = ChunkReceiver::new(chann_0.1);
                let sender_1: ChunkSender = ChunkSender::new(chann_1.0);
                let receiver_1: ChunkReceiver = ChunkReceiver::new(chann_1.1);

                guard.insert(
                    (stream_id, scid.to_vec()),
                    (sender_0, sender_1, receiver_0, receiver_1),
                );
            }
        }
        pub fn get_high_priority_sender(&self, stream_id: u64, scid: &[u8]) -> Option<ChunkSender> {
            let guard = &mut *self.inner.lock().unwrap();
            if let Some(entry) = guard.map.get(&(stream_id, scid.to_vec())) {
                Some(entry.1.clone())
            } else {
                None
            }
        }
        pub fn get_low_priority_sender(&self, stream_id: u64, scid: &[u8]) -> Option<ChunkSender> {
            let guard = &mut *self.inner.lock().unwrap();
            if let Some(entry) = guard.map.get(&(stream_id, scid.to_vec())) {
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
        map: HashMap<Ids, (ChunkSender, ChunkSender, ChunkReceiver, ChunkReceiver)>,
    }
    impl ChunksDispatchChannelInner {
        fn new() -> Self {
            Self {
                map: HashMap::new(),
            }
        }
        fn insert(
            &mut self,
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
    }
    impl ChunkSender {
        fn new(sender: crossbeam_channel::Sender<QueuedRequest>) -> Self {
            Self { sender }
        }
        pub fn send(
            &self,
            msg: QueuedRequest,
        ) -> Result<(), crossbeam_channel::SendError<QueuedRequest>> {
            self.sender.send(msg)
        }
    }

    pub struct ChunkReceiver {
        receiver: crossbeam_channel::Receiver<QueuedRequest>,
    }
    impl ChunkReceiver {
        fn new(receiver: crossbeam_channel::Receiver<QueuedRequest>) -> Self {
            Self { receiver }
        }
        fn try_recv(&self) -> Result<QueuedRequest, crossbeam_channel::TryRecvError> {
            self.receiver.try_recv()
        }
    }
}
