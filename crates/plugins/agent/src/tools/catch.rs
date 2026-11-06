use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

#[derive(Clone, Default)]
pub(super) struct Catch(Arc<Mutex<Vec<String>>>);

impl Catch {
    pub(super) fn has(&self, needle: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains(needle))
    }
}

struct Msg(String);

impl Visit for Msg {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl Subscriber for Catch {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= Level::WARN
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Id::from_u64(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut msg = Msg(String::new());
        event.record(&mut msg);
        self.0.lock().unwrap().push(msg.0);
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}
