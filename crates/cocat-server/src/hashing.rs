//! Password hashing and checking run on the blocking pool, never on an async worker, and only
//! a few at a time: an Argon2 run reserves 19 MiB, so an unbounded pool would let a burst of
//! logins ask for gigabytes.

use std::sync::{Arc, LazyLock};

use tokio::sync::Semaphore;
use tokio::task::JoinError;

/// One password run per core. The work is CPU-bound, so a larger pool would only queue runs
/// up while holding their memory.
static SLOTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(slots())));

fn slots() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
}

/// Runs `work` off the async workers, waiting for a slot first.
pub(crate) async fn off_worker<T, F>(work: F) -> Result<T, JoinError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // The slot moves into the run, so a request dropped mid-run (a client that disconnects)
    // still holds it until the hash ends. The semaphore is never closed, so a refused permit
    // cannot happen; if it did, the work would still run.
    let slot = Arc::clone(&SLOTS).acquire_owned().await.ok();
    tokio::task::spawn_blocking(move || {
        let _slot = slot;
        work()
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    use super::*;

    /// Both tests count the one process-wide semaphore, so they take turns.
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn only_one_password_work_runs_per_slot() {
        let _serial = SERIAL.lock().await;
        let permits = slots();
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..permits * 4 {
            let in_flight = Arc::clone(&in_flight);
            let peak = Arc::clone(&peak);
            tasks.push(tokio::spawn(async move {
                off_worker(move || {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(50));
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                })
                .await
                .expect("the password work answered");
            }));
        }
        for task in tasks {
            task.await.expect("the task finished");
        }
        assert!(
            peak.load(Ordering::SeqCst) <= permits,
            "{} password tasks ran at once, over the {permits} slots",
            peak.load(Ordering::SeqCst)
        );
    }

    /// A client that disconnects drops its request, but the blocking run it started goes on:
    /// the slot must stay taken until that run ends, or dropped requests bypass the bound.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dropped_request_keeps_its_slot_until_the_run_ends() {
        let _serial = SERIAL.lock().await;
        let (started, on_start) = std::sync::mpsc::channel();
        let (release, on_release) = std::sync::mpsc::channel::<()>();
        let before = SLOTS.available_permits();
        let request = tokio::spawn(off_worker(move || {
            started.send(()).unwrap();
            on_release.recv().ok();
        }));
        tokio::task::spawn_blocking(move || on_start.recv().unwrap())
            .await
            .unwrap();
        request.abort();
        let _ = request.await;
        assert_eq!(
            SLOTS.available_permits(),
            before - 1,
            "the slot came back early"
        );
        release.send(()).unwrap();
        for _ in 0..100 {
            if SLOTS.available_permits() == before {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the slot never came back after the run ended");
    }
}
