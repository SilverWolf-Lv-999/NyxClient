use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecyclePhase {
    Startup,
    Running,
    Closing,
    Closed,
}

#[derive(Debug, Clone)]
pub struct EventLifecycle {
    pub phase: LifecyclePhase,
    pub timestamp: SystemTime,
    pub uptime: Option<Duration>,
    pub frame: Option<u64>,
}

impl EventLifecycle {
    pub fn startup() -> Self {
        Self::new(LifecyclePhase::Startup, None, None)
    }

    pub fn running(frame: u64, started_at: Instant) -> Self {
        Self::new(
            LifecyclePhase::Running,
            Some(started_at.elapsed()),
            Some(frame),
        )
    }

    pub fn closing(started_at: Instant) -> Self {
        Self::new(LifecyclePhase::Closing, Some(started_at.elapsed()), None)
    }

    pub fn closed(started_at: Instant) -> Self {
        Self::new(LifecyclePhase::Closed, Some(started_at.elapsed()), None)
    }

    pub fn new(phase: LifecyclePhase, uptime: Option<Duration>, frame: Option<u64>) -> Self {
        Self {
            phase,
            timestamp: SystemTime::now(),
            uptime,
            frame,
        }
    }
}
