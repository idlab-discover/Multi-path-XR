use crate::error::AppError;
use axum::http::StatusCode;
use bytes::Bytes;
use std::sync::Arc;
use std::sync::RwLock;
use tokio::sync::watch;

/// Response headers/status published once the origin responds (before body completes).
#[derive(Debug, Clone)]
pub struct InflightHead {
    pub status: StatusCode,
    pub headers: Vec<(String, String)>,
}

/// Progress snapshot for consumers and observability.
#[derive(Debug, Clone, Default)]
pub struct InflightProgress {
    pub available_bytes: u64,
    pub done: bool,
    pub error: Option<Arc<str>>,
}

#[derive(Debug, Default)]
struct InflightState {
    chunks: Vec<Bytes>,
    available_bytes: u64,
    done: bool,
    error: Option<Arc<str>>,
}

/// Snapshot returned to readers of a single inflight object.
#[derive(Debug, Clone)]
pub enum InflightRead {
    Chunk(Bytes),
    Pending,
    Done,
    Error(Arc<str>),
}

/// Shared inflight state for a single cache key.
#[derive(Debug)]
pub struct Inflight {
    head_tx: watch::Sender<Option<InflightHead>>,
    head_rx: watch::Receiver<Option<InflightHead>>,
    prog_tx: watch::Sender<InflightProgress>,
    prog_rx: watch::Receiver<InflightProgress>,
    state: RwLock<InflightState>,
}

impl Inflight {
    /// Creates a new inflight entry.
    pub fn new() -> Self {
        let (head_tx, head_rx) = watch::channel::<Option<InflightHead>>(None);
        let (prog_tx, prog_rx) = watch::channel::<InflightProgress>(InflightProgress::default());
        Self {
            head_tx,
            head_rx,
            prog_tx,
            prog_rx,
            state: RwLock::new(InflightState::default()),
        }
    }

    /// Subscribes to head updates.
    pub fn subscribe_head(&self) -> watch::Receiver<Option<InflightHead>> {
        self.head_rx.clone()
    }

    /// Subscribes to progress updates.
    pub fn subscribe_progress(&self) -> watch::Receiver<InflightProgress> {
        self.prog_rx.clone()
    }

    /// Publishes head (status+headers) once origin responded.
    pub fn publish_head(&self, head: InflightHead) {
        let _ = self.head_tx.send_replace(Some(head));
    }

    /// Appends a new body chunk and wakes subscribers.
    pub fn push_chunk(&self, chunk: Bytes) {
        let mut state = self.state.write().expect("inflight state poisoned");
        state.available_bytes = state.available_bytes.saturating_add(chunk.len() as u64);
        state.chunks.push(chunk);

        let mut p = (*self.prog_tx.borrow()).clone();
        p.available_bytes = state.available_bytes;
        let _ = self.prog_tx.send_replace(p);
    }

    /// Returns the chunk at the given index, or the current terminal/pending state.
    pub fn read_at(&self, index: usize) -> InflightRead {
        let state = self.state.read().expect("inflight state poisoned");
        if let Some(chunk) = state.chunks.get(index) {
            return InflightRead::Chunk(chunk.clone());
        }
        if let Some(err) = &state.error {
            return InflightRead::Error(err.clone());
        }
        if state.done {
            return InflightRead::Done;
        }
        InflightRead::Pending
    }

    /// Marks completion.
    pub fn publish_done(&self) {
        let mut state = self.state.write().expect("inflight state poisoned");
        state.done = true;
        let mut p = (*self.prog_tx.borrow()).clone();
        p.done = true;
        let _ = self.prog_tx.send_replace(p);
    }

    /// Marks error and completion.
    pub fn publish_error(&self, err: AppError) {
        let err = Arc::<str>::from(err.to_string());
        let mut state = self.state.write().expect("inflight state poisoned");
        state.done = true;
        state.error = Some(err.clone());
        let mut p = (*self.prog_tx.borrow()).clone();
        p.done = true;
        p.error = Some(err);
        let _ = self.prog_tx.send_replace(p);
    }
}
