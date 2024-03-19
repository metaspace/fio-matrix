use anyhow::Result;
use log4rs::append::console::ConsoleAppender;
use log4rs::append::file::FileAppender;
use log4rs::config::runtime::ConfigBuilder;
use log4rs::config::Appender;
use log4rs::config::Config;
use log4rs::config::Root;
use log4rs::encode::pattern::PatternEncoder;
use log4rs::encode::writer::simple::SimpleWriter;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

pub(crate) fn init_log() -> Result<log4rs::Handle> {
    let config_builder = configure_stdout_log(Config::builder());

    let log_config = config_builder.build(
        Root::builder()
            .appender("console")
            .build(log::LevelFilter::Info),
    )?;

    Ok(log4rs::init_config(log_config)?)
}

fn configure_stdout_log(config_builder: ConfigBuilder) -> ConfigBuilder {
    let console = ConsoleAppender::builder().build();
    config_builder.appender(Appender::builder().build("console", Box::new(console)))
}

fn configure_file_log(config_builder: ConfigBuilder, output_dir: &str) -> Result<ConfigBuilder> {
    let mut logfile_path = PathBuf::from(output_dir);
    logfile_path.push(format!(
        "log-{}.log",
        chrono::Local::now().format("%Y-%m-%d-%H%M-%f")
    ));
    println!("Log file path: {logfile_path:?}");

    let logfile = FileAppender::builder().build(logfile_path)?;
    Ok(config_builder.appender(Appender::builder().build("logfile", Box::new(logfile))))
}

pub(crate) fn setup_log(
    handle: log4rs::Handle,
    output_dir: Option<&str>,
    stdout_log: bool,
    memory_log: bool,
) -> Result<Option<Arc<MemoryAppender>>> {
    let mut log_config_builder = Config::builder();
    let mut root_builder = Root::builder();

    match output_dir {
        Some(output_dir) => {
            log_config_builder = configure_file_log(log_config_builder, output_dir)?;
            root_builder = root_builder.appender("logfile");
        }
        None => (),
    }

    if !std::io::stdout().is_terminal() && stdout_log {
        log_config_builder = configure_stdout_log(log_config_builder);
        root_builder = root_builder.appender("console");
    }

    let memory_log_handle = if memory_log {
        let handle = Arc::new(MemoryAppender::new());
        log_config_builder = log_config_builder
            .appender(Appender::builder().build("memory", Box::new(handle.clone())));
        root_builder = root_builder.appender("memory");
        Some(handle)
    } else {
        None
    };

    let log_config = log_config_builder.build(root_builder.build(log::LevelFilter::Info))?;

    handle.set_config(log_config);
    Ok(memory_log_handle)
}

#[derive(Debug)]
pub(crate) struct MemoryAppender {
    buffer: Mutex<SimpleWriter<Vec<u8>>>,
    encoder: Box<dyn log4rs::encode::Encode>,
}

impl MemoryAppender {
    fn new() -> Self {
        Self {
            buffer: Mutex::new(SimpleWriter(Vec::new())),
            encoder: Box::<PatternEncoder>::default(),
        }
    }

    pub(crate) fn data(&self) -> Vec<u8> {
        let mut buffer = self.buffer.lock().unwrap();
        let mut new_buffer = Vec::new();
        std::mem::swap(&mut buffer.0, &mut new_buffer);
        new_buffer
    }
}

impl log::Log for MemoryAppender {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        use std::ops::DerefMut;
        let mut buffer = self.buffer.lock().unwrap();
        self.encoder.encode(buffer.deref_mut(), record).unwrap();
    }

    fn flush(&self) {}
}

// impl log4rs::append::Append for MemoryAppender {
//     fn append(&self, record: &log::Record) -> anyhow::Result<()> {
//         use std::ops::DerefMut;
//         let mut buffer = self.buffer.lock().unwrap();
//         self.encoder.encode(buffer.deref_mut(), record)?;
//         Ok(())
//     }

//     fn flush(&self) {}
// }
