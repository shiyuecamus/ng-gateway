use ng_gateway_error::{NGError, NGResult};
use std::sync::{Arc, Mutex};
use tracing::{subscriber::set_global_default, Level};
use tracing_appender::{non_blocking::WorkerGuard, rolling};
use tracing_subscriber::{
    filter::DynFilterFn,
    fmt::{self},
    layer::SubscriberExt,
    Layer, Registry,
};

pub struct Logger {
    level: Arc<Mutex<Level>>,
    _file_guard: Option<WorkerGuard>,
}

#[allow(unused)]
impl Logger {
    pub fn new(level: Option<Level>) -> Self {
        Logger {
            level: Arc::new(Mutex::new(level.unwrap_or(Level::INFO))),
            _file_guard: None,
        }
    }

    #[inline]
    /// Sets the new logging level.
    ///
    /// # Parameters
    ///
    /// * `new_level`: The new logging level to be set.
    ///
    /// # Important Code Block
    ///
    /// Acquires a mutable lock on the current logging level and updates it to the new level.
    ///
    pub fn set_level(&self, new_level: Level) {
        let mut level = self.level.lock().unwrap();
        *level = new_level;
    }

    #[inline]
    /// Retrieves the current log level.
    ///
    /// This function returns a `Level` value representing the current log level.
    ///
    /// # Returns
    /// - `Level`: The current log level.
    ///
    /// # Panics
    /// - This function will panic if the lock on `self.level` is poisoned.
    pub fn get_level(&self) -> Level {
        *self.level.lock().unwrap()
    }

    #[inline]
    /// Initializes the logger
    ///
    /// This function sets up logging output to both the console and a rolling log file,
    /// with filtering based on log levels.
    pub fn initialize(&mut self) -> NGResult<()> {
        // Create a daily rolling file appender for log files
        let file_appender = rolling::daily("logs", "ng.log");
        // Convert the file appender into a non-blocking writer
        let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
        self._file_guard = Some(_guard);

        // Define a filter for console output based on the log level
        let console_filter = {
            let level = Arc::clone(&self.level);
            DynFilterFn::new(move |metadata, _| metadata.level() <= &*level.lock().unwrap())
        };

        // Define a filter for file output based on the log level
        let file_filter = {
            let level = Arc::clone(&self.level);
            DynFilterFn::new(move |metadata, _| metadata.level() <= &*level.lock().unwrap())
        };

        // Create a console layer with specific formatting options and the console filter
        let console_layer = {
            #[cfg(debug_assertions)]
            let mut layer = fmt::layer().pretty().with_writer(std::io::stdout);

            #[cfg(not(debug_assertions))]
            let mut layer = fmt::layer().with_writer(std::io::stdout);

            #[cfg(debug_assertions)]
            {
                layer = layer.with_file(true).with_line_number(true);
            }

            #[cfg(not(debug_assertions))]
            {
                layer = layer.with_file(false).with_line_number(false);
            }

            layer.with_filter(console_filter)
        };

        // Create a file layer with specific formatting options and the file filter
        let file_layer = {
            #[cfg(debug_assertions)]
            let mut layer = fmt::layer()
                .pretty()
                .with_writer(non_blocking)
                .with_ansi(false);

            #[cfg(not(debug_assertions))]
            let mut layer = fmt::layer().with_writer(non_blocking).with_ansi(false);

            #[cfg(debug_assertions)]
            {
                layer = layer.with_file(true).with_line_number(true);
            }

            #[cfg(not(debug_assertions))]
            {
                layer = layer.with_file(false).with_line_number(false);
            }

            layer.with_filter(file_filter)
        };

        // Combine the console and file layers into a single subscriber
        let subscriber = Registry::default().with(console_layer).with(file_layer);

        // Set the combined subscriber as the global default subscriber
        set_global_default(subscriber).map_err(|_| NGError::from("Failed to set logger"))?;
        Ok(())
    }
}
