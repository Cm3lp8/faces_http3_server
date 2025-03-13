#![allow(warnings)]

pub use queue_implementation::{BodyReqQueue, QueueTrackableItem};

mod queue_implementation {
    use std::collections::{HashMap, VecDeque};

    use crate::request_response::BodyRequest;

    type StreamIDs = (String, u64);
    type LastSendedIndex = usize;

    ///____________________________
    ///Fifo queue. Push back and pop front, keep track of packet index / conn, stream_id
    pub struct BodyReqQueue<T> {
        conn: Vec<u8>,
        queue: HashMap<u64, VecDeque<T>>,
    }
    impl<T: QueueTrackableItem> BodyReqQueue<T> {
        pub fn new(conn_id: &[u8]) -> Self {
            Self {
                conn: conn_id.to_vec(),
                queue: HashMap::new(),
            }
        }
        ///_______________________________
        ///Insert a new element in the fifo in the corresponding stream queue
        pub fn push_item(&mut self, item: T) {
            let item_index = item.item_index();
            let stream_id = item.stream_id();

            if let Some(entry) = self.queue.get_mut(&stream_id) {
                entry.push_back(item);
            } else {
                let mut queue = VecDeque::new();
                queue.push_back(item);
            }
        }
        ///___________________________________________________
        ///This iterates on registered streams that have pending bodies waiting in the queue.
        ///It calls the callback only if collection is > 0, and pop the next item. If item is not
        ///correctly send in the callback, it has to be repushed in front for next try.
        pub fn iter_mut_streams(
            &mut self,
        ) -> std::collections::hash_map::IterMut<'_, u64, VecDeque<T>> {
            self.queue.iter_mut()
            /*
            for q in self.queue.iter_mut() {
                if q.1.len() > 0 {
                    let res = cb(*q.0, q.1.pop_front().unwrap());

                    match res {
                        Ok(()) => {}
                        Err(item) => q.1.push_front(item),
                    }
                };
            }*/
        }

        pub fn push_item_on_stream(&mut self, item: T, stream_id: u64) {
            if let Some(entry) = self.queue.get_mut(&stream_id) {
                entry.push_back(item);
            } else {
                let mut queue = VecDeque::new();
                queue.push_back(item);
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
        fn item_index(&self) -> usize;
        fn conn_id(&self) -> String;
        fn stream_id(&self) -> u64;
        fn is_last_item(&self) -> bool;
    }
}
