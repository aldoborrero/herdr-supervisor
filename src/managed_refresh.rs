//! Bounded background reads for the managed Projects popup. Never submits decisions.
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::managed::Binding;

const MAX_READS: usize = 4;
const REFRESH: Duration = Duration::from_secs(2);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Projection {
    Detail,
    Overview,
}
type Key = (String, Projection);
type Fetch = dyn Fn(&Binding, Projection) -> Result<Value, String> + Send + Sync;

#[derive(Clone, Debug)]
pub enum View {
    Loading,
    Current(Value),
    Stale(Value, String),
    Failed(String),
}

struct Entry {
    binding: Binding,
    generation: String,
    value: Option<Value>,
    error: Option<String>,
    completed: Option<Instant>,
    pending: bool,
}

struct Read {
    key: Key,
    generation: String,
    binding: Binding,
}

struct Reply {
    read: Read,
    result: Result<Value, String>,
}

pub struct Cache {
    entries: BTreeMap<Key, Entry>,
    queue: VecDeque<Read>,
    running: usize,
    sender: mpsc::Sender<Reply>,
    receiver: mpsc::Receiver<Reply>,
    fetch: Arc<Fetch>,
}

impl Default for Cache {
    fn default() -> Self {
        Self::with_fetch(Arc::new(|binding, projection| {
            match projection {
                Projection::Detail => binding.context(),
                Projection::Overview => binding.overview(),
            }
            .map_err(|error| error.to_string())
        }))
    }
}

impl Cache {
    fn with_fetch(fetch: Arc<Fetch>) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            entries: BTreeMap::new(),
            queue: VecDeque::new(),
            running: 0,
            sender,
            receiver,
            fetch,
        }
    }

    /// A changed or corrupt marker invalidates both cached data and late replies.
    pub fn view(&mut self, slug: &str, binding: anyhow::Result<Binding>) -> View {
        self.project(slug, binding, Projection::Detail)
    }

    pub fn overview(&mut self, slug: &str, binding: anyhow::Result<Binding>) -> View {
        self.project(slug, binding, Projection::Overview)
    }

    fn project(
        &mut self,
        slug: &str,
        binding: anyhow::Result<Binding>,
        projection: Projection,
    ) -> View {
        let binding = match binding {
            Ok(binding) => binding,
            Err(error) => {
                self.entries.retain(|(name, _), _| name != slug);
                self.queue.retain(|read| read.key.0 != slug);
                return View::Failed(error.to_string());
            }
        };
        // A binding change in either projection invalidates both, including
        // queued and in-flight replies from the previous authority.
        if self
            .entries
            .iter()
            .any(|((name, _), entry)| name == slug && entry.binding != binding)
        {
            self.entries.retain(|(name, _), _| name != slug);
            self.queue.retain(|read| read.key.0 != slug);
        }
        let key = (slug.to_string(), projection);
        let entry = self.entries.entry(key.clone()).or_insert_with(|| Entry {
            binding,
            generation: uuid::Uuid::new_v4().to_string(),
            value: None,
            error: None,
            completed: None,
            pending: false,
        });
        if !entry.pending && entry.completed.is_none_or(|at| at.elapsed() >= REFRESH) {
            entry.pending = true;
            self.queue.push_back(Read {
                key,
                generation: entry.generation.clone(),
                binding: entry.binding.clone(),
            });
        }
        let view = match (&entry.value, &entry.error) {
            (Some(value), Some(error)) => View::Stale(value.clone(), error.clone()),
            (Some(value), None) => View::Current(value.clone()),
            (None, Some(error)) => View::Failed(error.clone()),
            (None, None) => View::Loading,
        };
        self.start();
        view
    }

    /// Returns whether a visible snapshot changed; never waits for a socket/thread.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(reply) = self.receiver.try_recv() {
            self.running -= 1;
            changed |= self.accept(reply);
        }
        self.start();
        changed
    }

    pub fn invalidate(&mut self) {
        self.entries.clear();
        self.queue.clear();
        // In-flight replies still occupy their bounded slot, but can no longer
        // restore data from before an operator decision.
    }

    fn accept(&mut self, reply: Reply) -> bool {
        let Some(entry) = self.entries.get_mut(&reply.read.key) else {
            return false;
        };
        if entry.generation != reply.read.generation || entry.binding != reply.read.binding {
            return false;
        }
        entry.pending = false;
        entry.completed = Some(Instant::now());
        match reply.result {
            Ok(value) if value["run"]["id"] == entry.binding.run => {
                entry.value = Some(value);
                entry.error = None;
            }
            Ok(_) => entry.error = Some("controller returned another run".into()),
            Err(error) => entry.error = Some(error),
        }
        true
    }

    fn start(&mut self) {
        while self.running < MAX_READS {
            let Some(read) = self.queue.pop_front() else {
                break;
            };
            let failed_key = read.key.clone();
            let fetch = self.fetch.clone();
            let sender = self.sender.clone();
            // No join on drop: leaving a popup must not wait for an unavailable
            // controller. Reads use the existing bounded socket timeouts.
            let started = std::thread::Builder::new()
                .name("managed-view".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        fetch(&read.binding, read.key.1)
                    }))
                    .unwrap_or_else(|_| Err("controller reader stopped unexpectedly".into()));
                    let _ = sender.send(Reply { read, result });
                });
            match started {
                Ok(_) => self.running += 1,
                Err(_) => {
                    if let Some(entry) = self.entries.get_mut(&failed_key) {
                        entry.pending = false;
                        entry.completed = Some(Instant::now());
                        entry.error = Some("could not start controller reader".into());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    type Answer = mpsc::Sender<Result<Value, String>>;

    fn controlled() -> (Cache, mpsc::Receiver<(String, Answer)>) {
        let (started, requests) = mpsc::channel();
        let cache = Cache::with_fetch(Arc::new(move |binding, _projection| {
            let (answer, response) = mpsc::channel();
            started.send((binding.run.clone(), answer)).unwrap();
            response
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|_| Err("test reader closed".into()))
        }));
        (cache, requests)
    }

    fn binding(run: &str) -> Binding {
        Binding {
            version: 1,
            controller: "/test/controller".into(),
            credential: "/test/private-token".into(),
            run: run.into(),
            launch_config: None,
        }
    }

    fn drain(cache: &mut Cache, remaining: usize) {
        let until = Instant::now() + Duration::from_secs(5);
        while cache.running > remaining {
            cache.poll();
            assert!(Instant::now() < until, "reader did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn slow_readers_are_bounded_coalesced_and_do_not_block_another_project_or_exit() {
        let (mut cache, requests) = controlled();
        for n in 0..6 {
            assert!(matches!(
                cache.view(&format!("p{n}"), Ok(binding(&format!("r{n}")))),
                View::Loading
            ));
        }
        let mut pending = Vec::new();
        for _ in 0..MAX_READS {
            pending.push(requests.recv_timeout(Duration::from_secs(5)).unwrap());
        }
        for _ in 0..50 {
            assert!(matches!(cache.view("p0", Ok(binding("r0"))), View::Loading));
        }
        assert_eq!(cache.running, MAX_READS);
        assert_eq!(cache.queue.len(), 2);
        assert!(requests.try_recv().is_err());
        let (run, answer) = pending.pop().unwrap();
        answer.send(Ok(json!({"run":{"id":run}}))).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while cache.queue.len() == 2 {
            cache.poll();
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(2));
        }
        let (run, answer) = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(run, "r4");
        pending.push((run, answer));
        // Drop returns before blocked readers are released. No join or socket
        // operation is part of popup exit.
        drop(cache);
        for (_, answer) in pending {
            let _ = answer.send(Err("closed".into()));
        }
    }

    #[test]
    fn authority_changes_and_corrupt_markers_discard_late_snapshots() {
        let (mut cache, requests) = controlled();
        cache.view("p", Ok(binding("old")));
        let (_, old) = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(cache.view("p", Ok(binding("new"))), View::Loading));
        let (_, new) = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        new.send(Ok(json!({"run":{"id":"new"}}))).unwrap();
        drain(&mut cache, 1);
        old.send(Ok(json!({"run":{"id":"old"}}))).unwrap();
        drain(&mut cache, 0);
        assert!(
            matches!(cache.view("p", Ok(binding("new"))), View::Current(v) if v["run"]["id"]=="new")
        );

        cache
            .entries
            .get_mut(&("p".into(), Projection::Detail))
            .unwrap()
            .completed = None;
        cache.view("p", Ok(binding("new")));
        let (_, late) = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            cache.view("p", Err(anyhow::anyhow!("corrupt marker"))),
            View::Failed(_)
        ));
        late.send(Ok(json!({"run":{"id":"new"}}))).unwrap();
        drain(&mut cache, 0);
        assert!(
            !cache
                .entries
                .contains_key(&("p".into(), Projection::Detail))
        );
        assert!(matches!(cache.view("p", Ok(binding("new"))), View::Loading));
        let (_, wrong) = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        wrong.send(Ok(json!({"run":{"id":"foreign"}}))).unwrap();
        drain(&mut cache, 0);
        assert!(
            matches!(cache.view("p", Ok(binding("new"))), View::Failed(error) if error.contains("another run"))
        );
    }

    #[test]
    fn unavailable_controller_retains_explicitly_stale_data_until_successful_retry() {
        let (mut cache, requests) = controlled();
        cache.view("p", Ok(binding("run")));
        let (_, answer) = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        answer
            .send(Ok(json!({"run":{"id":"run","revision":1}})))
            .unwrap();
        drain(&mut cache, 0);
        cache
            .entries
            .get_mut(&("p".into(), Projection::Detail))
            .unwrap()
            .completed = None;
        cache.view("p", Ok(binding("run")));
        let (_, answer) = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        answer.send(Err("offline".into())).unwrap();
        drain(&mut cache, 0);
        assert!(
            matches!(cache.view("p", Ok(binding("run"))), View::Stale(value,error) if value["run"]["revision"]==1 && error=="offline")
        );
        assert!(requests.try_recv().is_err());
        cache
            .entries
            .get_mut(&("p".into(), Projection::Detail))
            .unwrap()
            .completed = None;
        cache.view("p", Ok(binding("run")));
        let (_, answer) = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        answer
            .send(Ok(json!({"run":{"id":"run","revision":2}})))
            .unwrap();
        drain(&mut cache, 0);
        assert!(
            matches!(cache.view("p", Ok(binding("run"))), View::Current(value) if value["run"]["revision"]==2)
        );
    }
    #[test]
    fn summary_and_detail_share_capacity_but_never_share_payloads_or_old_authority() {
        let (started, requests) = mpsc::channel();
        let mut cache = Cache::with_fetch(Arc::new(move |binding, projection| {
            let (answer, response) = mpsc::channel();
            started
                .send((binding.run.clone(), projection, answer))
                .unwrap();
            response.recv_timeout(Duration::from_secs(5)).unwrap()
        }));
        let mut pending = BTreeMap::new();
        for slug in ["p", "q", "r"] {
            cache.view(slug, Ok(binding("old")));
            cache.overview(slug, Ok(binding("old")));
        }
        for _ in 0..MAX_READS {
            let (_, projection, answer) = requests.recv_timeout(Duration::from_secs(5)).unwrap();
            pending
                .entry(projection)
                .or_insert_with(Vec::new)
                .push(answer);
        }
        assert_eq!(cache.running, MAX_READS);
        assert_eq!(cache.queue.len(), 2);
        for _ in 0..5 {
            cache.overview("p", Ok(binding("old")));
            cache.view("p", Ok(binding("old")));
        }
        assert_eq!(cache.queue.len(), 2);
        assert!(requests.try_recv().is_err());
        // Respond to both projections of p/q; the two queued r reads then start.
        for (projection, answers) in pending {
            for answer in answers {
                answer
                    .send(Ok(
                        json!({"run":{"id":"old"},"projection":format!("{projection:?}")}),
                    ))
                    .unwrap();
            }
        }
        drain(&mut cache, 2);
        let (_, _, r1) = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        let (_, _, r2) = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            matches!(cache.view("p", Ok(binding("old"))), View::Current(value) if value["projection"]=="Detail")
        );
        assert!(
            matches!(cache.overview("p", Ok(binding("old"))), View::Current(value) if value["projection"]=="Overview")
        );
        // Rebinding through the summary invalidates the detail as well.
        cache.overview("r", Ok(binding("new")));
        let (run, projection, fresh) = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!((run.as_str(), projection), ("new", Projection::Overview));
        fresh.send(Ok(json!({"run":{"id":"new"}}))).unwrap();
        r1.send(Ok(json!({"run":{"id":"old"}}))).unwrap();
        r2.send(Ok(json!({"run":{"id":"old"}}))).unwrap();
        drain(&mut cache, 0);
        assert!(matches!(
            cache.overview("r", Ok(binding("new"))),
            View::Current(_)
        ));
        assert!(
            !cache
                .entries
                .contains_key(&("r".into(), Projection::Detail))
        );
        cache.overview("p", Err(anyhow::anyhow!("corrupt marker")));
        assert!(!cache.entries.keys().any(|(slug, _)| slug == "p"));
    }
}
