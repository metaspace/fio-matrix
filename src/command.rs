use anyhow::{anyhow, Result};
use std::process::{self, Stdio};
use std::ffi::OsStr;

pub(crate) struct Command {
    command: process::Command,
}

impl Command {
    pub(crate) fn new(cmd: impl AsRef<OsStr>) -> Self {
        Self {
            command: process::Command::new(cmd.as_ref()),
        }
    }

    pub(crate) fn spawn(&mut self) -> std::io::Result<process::Child> {
        log::info!("Running command: {:?}", &self.command);
        self.command.spawn()
    }

    pub(crate) fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Self {
        self.command.arg(arg);
        self
    }

    pub(crate) fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command.args(args);
        self
    }

    pub(crate) fn stdout<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.command.stdout(cfg);
        self
    }

    pub(crate) fn stderr<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.command.stdout(cfg);
        self
    }
}

pub(crate) trait SpawnRetry {
    fn spawn_retry(
        &mut self,
        retry_max: u32,
        retry_delay: std::time::Duration,
    ) -> Result<()> ;
}

impl SpawnRetry for process::Command {
    fn spawn_retry(
        &mut self,
        retry_max: u32,
        retry_delay: std::time::Duration,
    ) -> Result<()> {
        if retry_max == 0 {
            return Err(anyhow!("Invalid retry count value"));
        }

        let mut retry_cnt: u32 = 0;

        while retry_cnt < retry_max {
            log::info!("Running command: {:?}", self);
            match self.spawn()?.wait()?.check_status() {
                Ok(v) => {
                    log::info!("Command succeeded: {:?}", self);
                    return Ok(v)},
                Err(e) => {
                    log::warn!("Command retry count: {retry_cnt}");
                    log::warn!("Command failed: {:?}", self);
                    retry_cnt += 1;
                    if retry_cnt == retry_max {
                        return Err(e);
                    }
                    std::thread::sleep(retry_delay);
                },
            }
        }

        unreachable!()
    }

}

pub(crate) trait CheckExitCode {
    fn check_status(&self) -> Result<()>;
}

impl CheckExitCode for process::ExitStatus {
    fn check_status(&self) -> Result<()> {
        if self.success() {
            Ok(())
        } else {
            Err(anyhow!("Proccess failed: {}", self.code().unwrap()))
        }
    }
}

impl std::ops::Deref for Command {
    type Target = process::Command;

    fn deref(&self) -> &Self::Target {
        &self.command
    }
}

impl std::ops::DerefMut for Command {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.command
    }
}
