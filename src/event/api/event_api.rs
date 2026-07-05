use std::{
    any::TypeId,
    collections::HashMap,
    sync::{Arc, Mutex},
};

use super::{
    event::Event,
    event_handler::{EventCallback, EventHandler, HandlerId},
};

pub struct EventBus {
    next_id: HandlerId,
    handlers: HashMap<TypeId, Vec<EventHandler>>,
}

pub type SharedEventBus = Arc<Mutex<EventBus>>;

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn shared() -> SharedEventBus {
        Arc::new(Mutex::new(Self::new()))
    }

    pub fn subscribe<E, F>(&mut self, priority: i32, handler: F) -> HandlerId
    where
        E: Event + 'static,
        F: FnMut(&mut E) + Send + 'static,
    {
        let id = self.next_id;
        self.next_id += 1;

        let mut handler = handler;
        let callback: EventCallback = Box::new(move |event| {
            if let Some(event) = event.as_any_mut().downcast_mut::<E>() {
                handler(event);
            }
        });

        let handlers = self.handlers.entry(TypeId::of::<E>()).or_default();
        handlers.push(EventHandler::new(id, priority, callback));
        handlers.sort_by(|left, right| right.priority().cmp(&left.priority()));
        id
    }

    pub fn unsubscribe(&mut self, id: HandlerId) -> bool {
        let mut removed = false;

        for handlers in self.handlers.values_mut() {
            let before = handlers.len();
            handlers.retain(|handler| handler.id() != id);
            removed |= handlers.len() != before;
        }

        removed
    }

    pub fn dispatch<E>(&mut self, event: &mut E)
    where
        E: Event + 'static,
    {
        if let Some(handlers) = self.handlers.get_mut(&TypeId::of::<E>()) {
            for handler in handlers {
                handler.handle(event);
            }
        }
    }

    pub fn dispatch_shared<E>(event_bus: &SharedEventBus, event: &mut E)
    where
        E: Event + 'static,
    {
        if let Ok(mut event_bus) = event_bus.lock() {
            event_bus.dispatch(event);
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self {
            next_id: 1,
            handlers: HashMap::new(),
        }
    }
}
