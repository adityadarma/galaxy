use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock, mpsc};

const PARALLEL_THRESHOLD: usize = 64;

/// Process-wide CPU budget for workspace indexing. Language engines may run
/// concurrently, but together they never exceed the machine's logical CPUs.
pub struct IndexScheduler {
    capacity: usize,
    available: Mutex<usize>,
    ready: Condvar,
}

impl IndexScheduler {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            available: Mutex::new(capacity),
            ready: Condvar::new(),
        }
    }

    pub fn map_ordered<I, O, F>(&self, inputs: Vec<I>, operation: F) -> Vec<O>
    where
        I: Send,
        O: Send,
        F: Fn(I) -> O + Sync,
    {
        self.map_ordered_limited(inputs, usize::MAX, operation)
    }

    pub fn map_ordered_limited<I, O, F>(
        &self,
        inputs: Vec<I>,
        max_workers: usize,
        operation: F,
    ) -> Vec<O>
    where
        I: Send,
        O: Send,
        F: Fn(I) -> O + Sync,
    {
        self.map_ordered_with_progress_limited(inputs, max_workers, operation, |_, _| {})
    }

    pub fn run<O>(&self, operation: impl FnOnce() -> O) -> O {
        let permit = self.acquire(1);
        let output = operation();
        drop(permit);
        output
    }

    pub fn map_ordered_with_progress<I, O, F, P>(
        &self,
        inputs: Vec<I>,
        operation: F,
        progress: P,
    ) -> Vec<O>
    where
        I: Send,
        O: Send,
        F: Fn(I) -> O + Sync,
        P: FnMut(usize, usize),
    {
        self.map_ordered_with_progress_limited(inputs, usize::MAX, operation, progress)
    }

    pub fn map_ordered_with_progress_limited<I, O, F, P>(
        &self,
        inputs: Vec<I>,
        max_workers: usize,
        operation: F,
        mut progress: P,
    ) -> Vec<O>
    where
        I: Send,
        O: Send,
        F: Fn(I) -> O + Sync,
        P: FnMut(usize, usize),
    {
        let total = inputs.len();
        let max_workers = max_workers.max(1);
        if total < PARALLEL_THRESHOLD || self.capacity == 1 || max_workers == 1 {
            let permit = self.acquire(1);
            let outputs = inputs
                .into_iter()
                .enumerate()
                .map(|(index, input)| {
                    let output = operation(input);
                    progress(index + 1, total);
                    output
                })
                .collect();
            drop(permit);
            return outputs;
        }

        let permits = self.acquire(total.min(max_workers));
        let workers = permits.count;
        let queue = Mutex::new(inputs.into_iter().enumerate().collect::<VecDeque<_>>());
        let (sender, receiver) = mpsc::channel();
        let mut outputs = (0..total).map(|_| None).collect::<Vec<Option<O>>>();
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let sender = sender.clone();
                let operation = &operation;
                let queue = &queue;
                scope.spawn(move || {
                    loop {
                        let next = queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
                        let Some((index, input)) = next else {
                            break;
                        };
                        if sender.send((index, operation(input))).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(sender);
            for completed in 1..=total {
                let (index, output) = receiver
                    .recv()
                    .expect("index worker stopped without result");
                outputs[index] = Some(output);
                progress(completed, total);
            }
        });
        drop(permits);
        outputs
            .into_iter()
            .map(|output| output.expect("index worker result missing"))
            .collect()
    }

    fn acquire(&self, jobs: usize) -> Permits<'_> {
        let mut available = self.available.lock().unwrap_or_else(|e| e.into_inner());
        while *available == 0 {
            available = self
                .ready
                .wait(available)
                .unwrap_or_else(|e| e.into_inner());
        }
        let count = jobs.min(*available);
        *available -= count;
        Permits {
            scheduler: self,
            count,
        }
    }
}

struct Permits<'a> {
    scheduler: &'a IndexScheduler,
    count: usize,
}

impl Drop for Permits<'_> {
    fn drop(&mut self) {
        let mut available = self
            .scheduler
            .available
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *available += self.count;
        self.scheduler.ready.notify_all();
    }
}

pub fn shared_index_scheduler() -> &'static IndexScheduler {
    static SCHEDULER: OnceLock<IndexScheduler> = OnceLock::new();
    SCHEDULER.get_or_init(|| {
        IndexScheduler::new(std::thread::available_parallelism().map_or(1, |count| count.get()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn map_is_ordered_and_never_exceeds_capacity() {
        let scheduler = IndexScheduler::new(3);
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let output = scheduler.map_ordered((0..96).collect(), {
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            move |value| {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(1));
                active.fetch_sub(1, Ordering::SeqCst);
                value * 2
            }
        });
        assert_eq!(output, (0..96).map(|value| value * 2).collect::<Vec<_>>());
        assert!(peak.load(Ordering::SeqCst) <= 3);
    }

    #[test]
    fn concurrent_maps_share_one_capacity_limit() {
        let scheduler = Arc::new(IndexScheduler::new(3));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let scheduler = Arc::clone(&scheduler);
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                scope.spawn(move || {
                    scheduler.map_ordered((0..96).collect(), move |_| {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(1));
                        active.fetch_sub(1, Ordering::SeqCst);
                    });
                });
            }
        });
        assert!(peak.load(Ordering::SeqCst) <= 3);
    }

    #[test]
    fn limited_map_uses_at_most_requested_workers() {
        let scheduler = IndexScheduler::new(8);
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let output = scheduler.map_ordered_limited((0..96).collect(), 2, |value| {
            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(1));
            active.fetch_sub(1, Ordering::SeqCst);
            value
        });

        assert_eq!(output, (0..96).collect::<Vec<_>>());
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }
}
