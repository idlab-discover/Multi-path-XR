use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tracing::error;

pub type ThreadPool = Arc<Mutex<Vec<JoinHandle<()>>>>;

pub fn clean_threads(thread_pool: &ThreadPool) {
    match thread_pool.lock() {
        Ok(mut pool) => {
            pool.retain(|handle| !handle.is_finished());
        }
        Err(e) => {
            error!("Failed to acquire lock on thread_pool: {}", e);
        }
    }
}

pub fn join_all_threads(thread_pool: &ThreadPool) {
    let handles = match thread_pool.lock() {
        Ok(mut guard) => guard.drain(..).collect::<Vec<_>>(),
        Err(e) => {
            error!(
                "Failed to acquire lock on thread_pool during shutdown: {}",
                e
            );
            return;
        }
    };

    for handle in handles {
        if let Err(e) = handle.join() {
            error!("A managed thread panicked during shutdown: {:?}", e);
        }
    }
}
