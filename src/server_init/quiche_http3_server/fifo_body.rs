#![allow(warnings)]

pub use queue_implementation::{BodyReqQueue, QueueTrackableItem};

mod queue_implementation {
    use std::collections::{
        hash_map::{Iter, Values},
        HashMap, VecDeque,
    };

    use crate::request_response::BodyRequest;

    type StreamIDs = (String, u64);
    type LastSendedIndex = usize;

    ///____________________________
    ///Fifo queue. Push back and pop front, keep track of packet index / conn, stream_id
    pub struct BodyReqQueue<T> {
        conn: Vec<u8>,
        streams: VecDeque<u64>,
        current_stream_index: usize,
        queue: HashMap<u64, VecDeque<T>>,
    }
    impl<T: QueueTrackableItem> BodyReqQueue<T> {
        pub fn new(conn_id: &[u8]) -> Self {
            Self {
                conn: conn_id.to_vec(),
                streams: VecDeque::new(),
                current_stream_index: 0,
                queue: HashMap::new(),
            }
        }
        pub fn max_pending_queue(&self) -> Option<(u64, usize)> {
            if let Some(item) = self.queue.iter().max_by_key(|item| item.1.len()) {
                Some((*item.0, item.1.len()))
            } else {
                None
            }
        }
        ///_______________________________
        ///Insert a new element in the fifo in the corresponding stream queue
        ///if stream is new, stream Vedeque is updated to keep track of futures iterations.
        pub fn push_item(&mut self, item: T) {
            let stream_id = item.stream_id();

            if let Some(entry) = self.queue.get_mut(&stream_id) {
                entry.push_back(item);
            } else {
                if !self.streams.contains(&stream_id) {
                    self.streams.push_back(stream_id);
                }
                let mut queue = VecDeque::new();
                queue.push_back(item);
                self.queue.insert(stream_id, queue);
            }
        }
        ///_______________________________
        ///Push front the element in the fifo in the corresponding stream queue
        ///if stream is none, stream Vedeque is updated to keep track of futures iterations.
        pub fn push_item_on_front(&mut self, item: T) {
            let stream_id = item.stream_id();

            if let Some(entry) = self.queue.get_mut(&stream_id) {
                entry.push_front(item);
            } else {
                if !self.streams.contains(&stream_id) {
                    self.streams.push_back(stream_id);
                }
                let mut queue = VecDeque::new();
                queue.push_front(item);
                self.queue.insert(stream_id, queue);
            }
        }
        pub fn get_queue_mut(&mut self, stream_id: u64) -> Option<&mut VecDeque<T>> {
            self.queue.get_mut(&stream_id)
        }

        ///___________________________________________________
        ///This iterates on registered streams that have pending bodies. Returns None when all
        ///streams have been visited.
        pub fn next_stream(&mut self) -> Option<(u64, Option<&mut VecDeque<T>>)> {
            if self.streams.is_empty() {
                return None;
            };

            let len = self.streams.len();

            if self.current_stream_index < len {
                let stream_index = self.streams[self.current_stream_index];
                self.current_stream_index += 1;
                Some((stream_index, self.queue.get_mut(&stream_index)))
            } else {
                self.current_stream_index = 0;
                None
            }
        }
        pub fn stream_vec(&self) -> &VecDeque<u64> {
            &self.streams
        }
        pub fn for_stream(&self) -> Option<Iter<'_, u64, VecDeque<T>>> {
            if self.streams.is_empty() {
                return None;
            };

            Some(self.queue.iter())
        }
        pub fn remove(&mut self, stream_id: u64) {
            if let Some(pos) = self.streams.iter().position(|it| *it == stream_id) {
                self.streams.remove(pos);
                self.current_stream_index = 0;
            }
            self.queue.remove(&stream_id);
        }

        pub fn push_item_on_stream(&mut self, item: T, stream_id: u64) {
            if let Some(entry) = self.queue.get_mut(&stream_id) {
                entry.push_back(item);
            } else {
                if !self.streams.contains(&stream_id) {
                    self.streams.push_back(stream_id);
                }
                let mut queue = VecDeque::new();
                queue.push_back(item);
                self.queue.insert(stream_id, queue);
            }
        }

        pub fn is_stream_queue_empty(&self, stream_id: u64) -> bool {
            if let Some(entry) = self.queue.get(&stream_id) {
                if entry.len() > 0 {
                    false
                } else {
                    true
                }
            } else {
                true
            }
        }
    }

    pub trait QueueTrackableItem {
        fn stream_id(&self) -> u64;
        fn is_last_item(&self) -> bool;
        fn scid(&self) -> Vec<u8>;
        fn len(&self) -> usize;
    }
}
