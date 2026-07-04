use super::event::Event;

pub type HandlerId = u64;
pub type EventCallback = Box<dyn FnMut(&mut dyn Event) + Send>;

pub struct EventHandler {
    id: HandlerId,
    priority: i32,
    callback: EventCallback,
}

impl EventHandler {
    pub fn new(id: HandlerId, priority: i32, callback: EventCallback) -> Self {
        Self {
            id,
            priority,
            callback,
        }
    }

    pub const fn id(&self) -> HandlerId {
        self.id
    }

    pub const fn priority(&self) -> i32 {
        self.priority
    }

    pub(crate) fn handle(&mut self, event: &mut dyn Event) {
        (self.callback)(event);
    }
}
