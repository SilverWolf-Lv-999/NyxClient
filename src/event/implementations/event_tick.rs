use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickSource {
    System,
    Client,
    MinecraftClient,
}

#[derive(Debug, Clone)]
pub struct EventTick {
    pub source: TickSource,
    pub frame: u64,
    pub delta: Duration,
    pub timestamp: SystemTime,
    pub instant: Instant,
}

impl EventTick {
    pub fn system(frame: u64, delta: Duration) -> Self {
        Self::new(TickSource::System, frame, delta)
    }

    pub fn client(frame: u64, delta: Duration) -> Self {
        Self::new(TickSource::Client, frame, delta)
    }

    pub fn minecraft_client(frame: u64, delta: Duration) -> Self {
        Self::new(TickSource::MinecraftClient, frame, delta)
    }

    pub fn new(source: TickSource, frame: u64, delta: Duration) -> Self {
        Self {
            source,
            frame,
            delta,
            timestamp: SystemTime::now(),
            instant: Instant::now(),
        }
    }
}
