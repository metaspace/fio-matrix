//!
//! # TODO
//!
//! - Capture modprobe in/out

use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use indicatif::ProgressBar;
use logging::MemoryAppender;
use std::io::IsTerminal;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::rc::Rc;
use std::sync::Arc;
use std::{fs::File, path::PathBuf};
use tap::Pipe;
use tap::Tap;

mod command;
mod config;
mod logging;

use crate::command::CheckExitCode;
use crate::command::Command;
use crate::command::SpawnRetry;

fn main() -> Result<()> {
    let log_handle = logging::init_log()?;
    log::info!("Starting test runner");

    let config = config::Config::parse()?;

    let status = Rc::new(run_test(&config, log_handle));

    if let Some(target) = config.remote {
        shutdown(target, status.clone())?;
    }

    Rc::try_unwrap(status).or(Err(anyhow!("Failed to get status")))?
}

fn run_test(config: &config::Config, log_handle: log4rs::Handle) -> Result<()> {
    let output_dir = match config.capture {
        true => Some(get_batch_dir(&config)?),
        false => None,
    };

    let mem_log = if config.capture {
        logging::setup_log(log_handle, Some(output_dir.as_ref().unwrap()), true, true)?
    } else {
        None
    };

    log::info!("Configuration: {:#?}", config);

    let push_log = move || -> Result<()> {
        if let Some(target) = &config.remote {
            push_log(target, mem_log.clone().unwrap())?;
        }
        Ok(())
    };

    print_uname()?;
    let status = run_workloads(output_dir.as_deref(), &config, &push_log);

    // Print the error to log before compressing
    if let Err(e) = &status {
        log::error!("Test failed: {e:?}");
    }
    else {
        log::info!("Test succeeded");
    }

    push_log()?;

    if config.capture && config.compress {
        compress(output_dir.as_ref().unwrap())?;

        if let Some(target) = &config.remote {
            upload(target, &format!("{}.tgz", output_dir.as_ref().unwrap()))?;
        }
    }

    status
}

fn print_uname() -> Result<()> {
    let uname_output = Command::new("uname")
        .arg("-a")
        .stdout(Stdio::piped())
        .spawn()?
        .wait_with_output()?;
    uname_output
        .status
        .check_status()
        .context("`uname` failed")?;
    log::info!(
        "Uname: {}",
        String::from_utf8(uname_output.stdout).context("Failed to convert uname to utf-8")?
    );
    Ok(())
}

fn compress(output_dir: &str) -> Result<()> {
    let outfile_path = format!("{output_dir}.tgz");
    log::info!("Compressing to {outfile_path}");
    let outfile = File::create(outfile_path)?;
    let encoder = libflate::gzip::Encoder::new(outfile)?;
    let mut tarball = tar::Builder::new(encoder);

    for file in walkdir::WalkDir::new(output_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.clone().into_path().is_file())
    {
        tarball.append_path(file.into_path())?;
    }

    tarball.into_inner()?.finish().into_result()?;

    Ok(())
}

fn push_log(target: &url::Url, log: Arc<MemoryAppender>) -> Result<()> {
    let client = reqwest::blocking::Client::new();
    let buffer = log.data();

    client
        .put(target.join("log/")?)
        .body(buffer)
        .send()?
        .error_for_status()?;
    Ok(())
}

fn upload(target: &url::Url, filename: &str) -> Result<()> {
    let file = std::fs::File::open(filename)?;
    let client = reqwest::blocking::Client::new();
    client
        .put(target.join("upload/")?.join(filename)?)
        .body(file)
        .send()?
        .error_for_status()?;
    Ok(())
}

fn shutdown(target: url::Url, status: Rc<Result<()>>) -> Result<()> {
    let code = match *status {
        Ok(_) => 0,
        Err(_) => 1,
    };
    let client = reqwest::blocking::Client::new();
    client
        .put(target.join("shutdown/")?.join(&format!("{code}"))?)
        .send()?
        .error_for_status()?;
    Ok(())
}

fn get_batch_dir(config: &config::Config) -> Result<String> {
    let mut output_path = PathBuf::new();
    if let Some(path) = &config.output_path {
        output_path.push(path);
    }

    let mut filename = String::new();
    filename.push_str("output");
    if let Some(tag) = &config.tag {
        filename.push_str(&format!("-{tag}"));
    }

    let name = names::Generator::default()
        .next()
        .ok_or(anyhow!("Failed to generate name"))
        .context("Failed to generate name")?;

    filename.push_str(&format!("-{name}"));
    filename.push_str(&format!(
        "-{}",
        chrono::Local::now().format("%Y-%m-%d-%H%M")
    ));

    output_path.push(filename);

    std::fs::create_dir(&output_path).context("failed to create batch dir")?;

    Ok(output_path
        .to_str()
        .ok_or(anyhow!("failed to convert path to string"))?
        .into())
}

fn get_run_dir(prefix_dir: &str) -> Result<PathBuf> {
    let mut run_dir = PathBuf::new();
    run_dir.push(prefix_dir);
    run_dir.push(format!(
        "{}",
        chrono::Local::now().format("%Y-%m-%d-%H%M-%f")
    ));
    std::fs::create_dir(&run_dir).context("failed to create run dir")?;
    Ok(run_dir)
}

fn new_bar(enable: bool, total_configs: u64) -> Result<ProgressBar> {
    Ok(if std::io::stdout().is_terminal() && enable {
        let bar = ProgressBar::new(total_configs);
        bar.set_style(indicatif::ProgressStyle::with_template("{msg} {wide_bar} {pos}/{len} [elapsed: {elapsed} / estimated: {duration} / remaining: {eta}]")?);
        bar
    } else {
        ProgressBar::hidden()
    })
}

fn run_workloads(
    output_dir: Option<&str>,
    config: &config::Config,
    mut push_log: impl FnMut() -> Result<()>,
) -> Result<()> {
    log::info!("Starting test loop");
    use itertools::Itertools;
    let configs = config
        .block_sizes
        .clone()
        .into_iter()
        .cartesian_product(config.jobcounts.clone().into_iter())
        .cartesian_product(config.workloads.clone().into_iter())
        .cartesian_product(config.queue_depths.clone().into_iter())
        .collect::<Vec<_>>();

    if config.device == "nullb0" {
        let _ = teardown_cnull();
    }
    let _ = unload_module(&config);

    if let config::ModuleReloadPolicy::Once = config.module_reload_policy {
        load_module(config).context("Load module once")?;
    }

    if config.cpufreq {
        set_governor().context("failed to set cpu frequency governor")?;
    }

    let total_configs = config.samples as u64 * configs.len() as u64;
    let bar = new_bar(config.capture, total_configs).context("Failed to set up progress bar")?;
    bar.set_message("Measuring:");
    log::info!("Starting measurements, total configs: {total_configs}");
    bar.println(format!(
        "[+] Starting measurements, total configs: {total_configs}"
    ));

    for i in 0..config.samples {
        log::info!("Starting sample #{i}");
        bar.println(format!("[+] Starting sample #{i}"));
        let run_dir = output_dir
            .map(|output_dir| get_run_dir(output_dir))
            .transpose()
            .context("Failed to get run dir")?;
        for (((block_size, jobcount), workload), queue_depth) in configs.clone() {
            log::info!(
                "Starting test qd:{queue_depth} bs:{block_size} jobs:{jobcount} wl:{workload}"
            );
            bar.println(format!(
                "[+] Starting test qd:{queue_depth} bs:{block_size} jobs:{jobcount} wl:{workload}"
            ));
            setup(&config).context("Failed to set up module")?;
            run_single_workload(
                &config,
                run_dir.as_ref().map(|v| v.as_path()),
                queue_depth,
                &block_size,
                jobcount,
                &workload,
            )
            .context("Failed to run test")?;
            teardown(&config).context("Failed to tear down module")?;
            bar.inc(1);
            push_log()?;
        }
    }

    bar.println("[+] All done!");
    log::info!("Test loop done");
    Ok(())
}

fn run_single_workload(
    config: &config::Config,
    output_dir_path: Option<&Path>,
    queue_depth: u32,
    block_size: &str,
    jobcount: u32,
    workload: &str,
) -> Result<()> {
    let run_output_id = format!(
        "j{jobcount}-r{runtime}-w{workload}-bs{block_size}-qd{queue_depth}",
        runtime = config.runtime,
    );

    log::info!("Setting up workload: {run_output_id}");

    let run_file_path = |name: &str| -> Option<PathBuf> {
        let path = output_dir_path.map(|v| {
            let mut p = PathBuf::from(v);
            p.push(format!("{run_output_id}{name}"));
            p
        });
        path
    };

    if config.prep {
        let prep_stdout_path = run_file_path("-prep.stdout");
        let prep_stderr_path = run_file_path("-prep.stderr");

        let mut command = Command::new(&config.fio);
        command
            .arg("--name=prep")
            .arg("--rw=write")
            .arg("--direct=1")
            .arg("--bs=4k")
            .arg(format!("--filename=/dev/{}", config.device));

        if config.capture {
            command
                .stdout(File::create(prep_stdout_path.unwrap())?)
                .stderr(File::create(prep_stderr_path.unwrap())?);
        }

        log::info!("Running prep command");

        let mut prep = || -> Result<()> { command.spawn()?.wait()?.check_status() };
        prep().context("Prep work failed")?;
    }

    let output_path = run_file_path(".json");
    let stdout_path = run_file_path(".stdout");
    let stderr_path = run_file_path(".stderr");

    let mut args = vec![
        String::from("--group_reporting"),
        String::from("--name=default"),
        format!("--filename=/dev/{}", config.device),
        String::from("--time_based=1"),
        format!("--runtime={}", config.runtime),
        String::from("--gtod_reduce=1"),
        String::from("--clocksource=cpu"),
        format!("--readwrite={}", workload),
        format!("--blocksize={}", block_size),
        String::from("--direct=1"),
        String::from("--cpus_allowed_policy=split"),
        format!("--cpus_allowed=0-{}", jobcount - 1),
        format!("--numjobs={}", jobcount),
        String::from("--ioengine=io_uring"),
        format!("--iodepth={}", queue_depth),
        String::from("--fixedbufs=1"),
        String::from("--registerfiles=1"),
        String::from("--nonvectored=1"),
        //"--iodepth_batch_submit=4"
        //"--iodepth_batch_complete=4",
    ];

    if config.ramp != 0 {
        args.push(format!("--ramp={}", config.ramp));
    }

    if config.verify {
        args.push(format!("--do_verify=1"));
        args.push(format!("--verify=md5"));
    } else {
        args.push(String::from("--norandommap"));
        args.push(String::from("--random_generator=lfsr"));
    }

    if config.capture {
        args.push(String::from("--output-format=json+"));
        args.push(format!(
            "--output={}",
            output_path
                .unwrap()
                .to_str()
                .ok_or(anyhow!("path conversion error"))?
        ));
    }

    if config.hipri {
        args.push(String::from("--hipri=1"));
    }

    let mut command = Command::new(&config.fio);

    command.args(args);

    if config.capture {
        command
            .stdout(File::create(stdout_path.unwrap())?)
            .stderr(File::create(stderr_path.unwrap())?);
    }

    log::info!("Running workload command");

    let mut run = || command.spawn()?.wait()?.check_status();
    run().context("Fio workload failed")
}

fn setup(config: &config::Config) -> Result<()> {
    if let config::ModuleReloadPolicy::Always = config.module_reload_policy {
        load_module(config).context("Load module always")?;
    }

    if config.configure_c_nullblk {
        setup_cnull(&config.device).context("setup cnull")?;
    }

    set_block_scheduler(&config.device).context("Set block scheduler")?;
    disable_iostats(&config.device).context("Disable iostats")?;

    Ok(())
}

fn teardown(config: &config::Config) -> Result<()> {
    if config.configure_c_nullblk {
        teardown_cnull()?;
    }

    if let config::ModuleReloadPolicy::Always = config.module_reload_policy {
        unload_module(config)?;
    }

    Ok(())
}

fn load_module(config: &config::Config) -> Result<()> {
    match &config.module {
        Some(module) => {
            log::info!("Inserting module: {}", module);
            if config.insmod {
                Command::new("insmod")
                    .arg(module)
                    .args(&config.module_args)
                    .spawn()?
                    .wait()?
                    .check_status()?;
            }

            if config.modprobe {
                Command::new("modprobe")
                    .arg(module)
                    .args(&config.module_args)
                    .spawn()?
                    .wait()?
                    .check_status()?;
            }
        }
        None => (),
    }

    Ok(())
}

fn unload_module(config: &config::Config) -> Result<()> {
    match &config.module {
        Some(module) => {
            log::info!("Unloading module: {}", module);
            if config.insmod {
                Command::new("rmmod")
                    .arg(module)
                    .spawn_retry(3, std::time::Duration::from_secs(1))?;
            }

            if config.modprobe {
                Command::new("modprobe")
                    .arg("-r")
                    .arg(module)
                    .spawn_retry(3, std::time::Duration::from_secs(1))?;
            }
        }
        None => (),
    }

    Ok(())
}

fn setup_cnull(name: &str) -> Result<()> {
    use std::fs::create_dir;
    let control_path = PathBuf::from("/sys/kernel/config/nullb").tap_mut(|p| p.push(name));

    log::info!("Configuring null block at {control_path:?}");

    control_path
        .clone()
        .pipe(create_dir)
        .context("create debugfs folder")?;

    let write_control_file = |name: &str, value: &str| -> Result<()> {
        control_path
            .clone()
            .tap_mut(|p| p.push(name))
            .pipe(|p| File::options().write(true).open(p))
            .context("open control file")?
            .write_all(value.as_bytes())
            .context("Failed to write control path")
    };

    write_control_file("blocksize", "4096").context("blocksize")?;
    write_control_file("completion_nsec", "0").context("completion_nsec")?;
    write_control_file("irqmode", "0").context("irqmode")?; // IRQ_NONE
    write_control_file("queue_mode", "2").context("queue_mode")?; // MQ
    write_control_file("hw_queue_depth", "256").context("hw_queue_depth")?;
    write_control_file("memory_backed", "1").context("memory_backed")?;
    write_control_file("size", "4096").context("size")?; // 4G
    write_control_file("poll_queues", "0").context("poll_queues")?;
    write_control_file("power", "1").context("power")?; // Instantiate device

    Ok(())
}

fn teardown_cnull() -> Result<()> {
    for entry in std::fs::read_dir("/sys/kernel/config/nullb")? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir(entry.path())?;
        }
    }
    Ok(())
}

fn set_block_scheduler(device: &str) -> Result<()> {
    log::info!("Setting block scheduler");
    PathBuf::from("/sys/block")
        .tap_mut(|p| p.push(device))
        .tap_mut(|p| p.push("queue"))
        .tap_mut(|p| p.push("scheduler"))
        .pipe(|p| File::options().write(true).open(p))
        .context("Failed to open scheduler file for write")?
        .write_all("none".as_bytes())
        .context("Failed to write control path")
}

fn disable_iostats(device: &str) -> Result<()> {
    log::info!("Disabling iostats");
    PathBuf::from("/sys/block")
        .tap_mut(|p| p.push(device))
        .tap_mut(|p| p.push("queue"))
        .tap_mut(|p| p.push("iostats"))
        .pipe(|p| File::options().write(true).open(p))
        .context("Failed to open scheduler file for write")?
        .write_all("0".as_bytes())
        .context("Failed to write control path")
}

fn set_governor() -> Result<()> {
    log::info!("Setting cpupower governor");
    Command::new("cpupower")
        .arg("frequency-set")
        .arg("-g")
        .arg("performance")
        .spawn()?
        .wait()?
        .check_status()
        .context("Failed to set cpu frequency governor")
}

// fn disable_turbo_intel() -> Result<()> {
//     log::info!("Disabling intel turbo");
//     PathBuf::from("/sys/devices/system/cpu/intel_pstate/no_turbo")
//         .pipe(|p| File::options().write(true).open(p))?
//         .write_all("1\n".as_bytes())
//         .context("Failed to disable turbo boost")
// }