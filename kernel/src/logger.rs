use log::{Level, LevelFilter, Log, Metadata, Record};
use spin::Once;

use crate::drivers::vga::VgaTextWriter;

static LOGGER: KernelLogger = KernelLogger::new();
static LOGGER_ONCE: Once<()> = Once::new();

pub struct KernelLogger {
    writer: VgaTextWriter,
}

impl KernelLogger {
    pub const fn new() -> Self {
        Self {
            writer: VgaTextWriter::new(),
        }
    }
}

impl Log for KernelLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Trace
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let _ = self.writer.lock().write_fmt(format_args!(
            "[{:<5}] {}\n",
            record.level(),
            record.args()
        ));
    }

    fn flush(&self) {}
}

pub fn init_once() {
    LOGGER_ONCE.call_once(|| {
        log::set_logger(&LOGGER)
            .map(|()| log::set_max_level(LevelFilter::Trace))
            .expect("logger already configured");
    });
}

pub fn console() -> &'static VgaTextWriter {
    &LOGGER.writer
}
