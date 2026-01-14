use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use dashmap::{DashMap, DashSet};

#[derive(Eq, PartialEq, Hash, Clone, Debug)]
struct ConnStreamK {
    stream_id: u64,
    conn_id: Vec<u8>,
}

impl ConnStreamK {
    pub fn new(stream_id: u64, conn_id: Vec<u8>) -> Self {
        Self { stream_id, conn_id }
    }
}
#[derive(Eq, PartialEq, Hash, Clone, Debug)]
struct ConnPathK {
    path: String,
    conn_id: Vec<u8>,
}

impl ConnPathK {
    pub fn new(path: String, conn_id: Vec<u8>) -> Self {
        Self { path, conn_id }
    }
}

pub struct InFlightStreamsPathVerifier {
    accepted_route_pathes: HashSet<&'static str>,
    accepted_route_stream_pathes: Option<HashSet<String>>,
    stream_map: Arc<Mutex<HashSet<ConnStreamK>>>,
}

impl InFlightStreamsPathVerifier {
    pub fn new(
        route_format: HashSet<&'static str>,
        stream_pathes_set: Option<HashSet<String>>,
    ) -> Self {
        Self {
            accepted_route_pathes: route_format,
            accepted_route_stream_pathes: stream_pathes_set,
            stream_map: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    #[inline]
    pub fn insert_stream_path_for_conn(
        &self,
        conn_id: Vec<u8>,
        stream_id: u64,
        path: &str,
    ) -> bool {
        if let Some(stream_path_set) = self.accepted_route_stream_pathes.as_ref() {
            if !stream_path_set.contains(path) && !self.accepted_route_pathes.contains(path) {
                return false;
            }
        } else {
            if !self.accepted_route_pathes.contains(path) {
                return false;
            };
        }

        let k_0 = ConnStreamK::new(stream_id, conn_id.clone());
        let guard_stream_map = &mut *self.stream_map.lock().unwrap();
        guard_stream_map.insert(k_0.clone());
        true
    }

    #[inline]
    pub fn is_finished_request_a_valid_path(&self, conn_id: Vec<u8>, stream_id: u64) -> bool {
        let guard_stream_map = &mut *self.stream_map.lock().unwrap();
        let k = ConnStreamK::new(stream_id, conn_id);

        guard_stream_map.remove(&k)
    }
}
