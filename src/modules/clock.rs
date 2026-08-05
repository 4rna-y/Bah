use chrono::Local;

/// A cached clock string, updated by the bar's one-second task.
pub struct Clock {
    value: String,
}

impl Clock {
    pub fn new() -> Self {
        let mut clock = Self {
            value: String::new(),
        };
        clock.tick();
        clock
    }

    pub fn tick(&mut self) {
        self.value = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    }

    pub fn value(&self) -> &str {
        &self.value
    }
}
