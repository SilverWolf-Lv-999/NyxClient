use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandLineStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandLineAction {
    Input {
        line: String,
    },
    Output {
        stream: CommandLineStream,
        text: String,
    },
    Execute {
        command: String,
        args: Vec<String>,
    },
    Completed {
        command: String,
        exit_code: Option<i32>,
        success: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventCommandLine {
    pub action: CommandLineAction,
    pub timestamp: SystemTime,
    cancelled: bool,
}

impl EventCommandLine {
    pub fn input(line: impl Into<String>) -> Self {
        Self::new(CommandLineAction::Input { line: line.into() })
    }

    pub fn stdout(text: impl Into<String>) -> Self {
        Self::output(CommandLineStream::Stdout, text)
    }

    pub fn stderr(text: impl Into<String>) -> Self {
        Self::output(CommandLineStream::Stderr, text)
    }

    pub fn output(stream: CommandLineStream, text: impl Into<String>) -> Self {
        Self::new(CommandLineAction::Output {
            stream,
            text: text.into(),
        })
    }

    pub fn execute(command: impl Into<String>, args: Vec<String>) -> Self {
        Self::new(CommandLineAction::Execute {
            command: command.into(),
            args,
        })
    }

    pub fn completed(command: impl Into<String>, exit_code: Option<i32>, success: bool) -> Self {
        Self::new(CommandLineAction::Completed {
            command: command.into(),
            exit_code,
            success,
        })
    }

    pub fn new(action: CommandLineAction) -> Self {
        Self {
            action,
            timestamp: SystemTime::now(),
            cancelled: false,
        }
    }

    pub const fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    pub fn set_cancelled(&mut self, cancelled: bool) {
        self.cancelled = cancelled;
    }
}
