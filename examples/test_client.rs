extern crate bits;
extern crate bits_client;
extern crate comedy;
extern crate failure;

use std::env;
use std::ffi::{OsStr, OsString};
use std::process;
use std::str::FromStr;

use bits::{BG_JOB_STATE_CONNECTING, BG_JOB_STATE_TRANSFERRING, BG_JOB_STATE_TRANSIENT_ERROR};
use comedy::guid::Guid;
use failure::{bail, Error};

#[cfg(feature = "external_task")]
use bits_client::task;
use bits_client::{BitsClient, BitsMonitorClient};

type Result = std::result::Result<(), Error>;

pub fn main() {
    if let Err(err) = entry() {
        eprintln!("{}", err);
        for cause in err.iter_causes() {
            eprintln!("caused by {}", cause);
        }

        process::exit(1);
    } else {
        println!("OK");
    }
}

const EXE_NAME: &'static str = "test_client";

fn usage() -> String {
    format!(
        concat!(
            "Usage {0} <command> [args...]\n",
            "Commands:\n",
            "  bits-start <URL> <local file>\n",
            "  bits-monitor <GUID> <millseconds delay>\n",
            "  bits-bg <GUID>\n",
            "  bits-fg <GUID>\n",
            "  bits-resume <GUID>\n",
            "  bits-complete <GUID>\n",
            "  bits-cancel <GUID>\n"
        ),
        EXE_NAME
    )
}

fn entry() -> Result {
    let args: Vec<_> = env::args_os().collect();

    if args.len() < 2 {
        eprintln!("{}", usage());
        bail!("not enough arguments");
    }

    let cmd = &*args[1].to_string_lossy();
    let cmd_args = &args[2..];

    // TODO: this should be able to do both
    #[cfg(feature = "external_task")]
    let mut client = BitsClient::connect_task(&task::task_name())?;

    #[cfg(not(feature = "external_task"))]
    let _com = comedy::com::InitCom::init_sta();
    #[cfg(not(feature = "external_task"))]
    let mut client = BitsClient::new();

    match cmd {
        // command line client for testing
        "bits-start" if cmd_args.len() == 2 => {
            bits_start(&mut client, cmd_args[0].clone(), cmd_args[1].clone())
        }
        "bits-monitor" if cmd_args.len() == 1 => bits_monitor(&mut client, &cmd_args[0]),
        // TODO: some way of testing set update interval
        "bits-bg" if cmd_args.len() == 1 => bits_bg(&mut client, &cmd_args[0]),
        "bits-fg" if cmd_args.len() == 1 => bits_fg(&mut client, &cmd_args[0]),
        "bits-resume" if cmd_args.len() == 1 => bits_resume(&mut client, &cmd_args[0]),
        "bits-complete" if cmd_args.len() == 1 => bits_complete(&mut client, &cmd_args[0]),
        "bits-cancel" if cmd_args.len() == 1 => bits_cancel(&mut client, &cmd_args[0]),

        _ => {
            eprintln!("{}", usage());
            bail!("usage error");
        }
    }
}

fn bits_start(client: &mut BitsClient, url: OsString, save_path: OsString) -> Result {
    let result = client.start_job(url, save_path, 1000)?;
    match result {
        Ok((r, monitor_client)) => {
            println!("start success, guid = {}", r.guid);
            monitor_loop(monitor_client, 1000)?;
            Ok(())
        }
        Err(e) => bail!("error from server {:?}", e),
    }
}

fn bits_monitor(client: &mut BitsClient, guid: &OsStr) -> Result {
    let guid = Guid::from_str(&guid.to_string_lossy())?;
    let result = client.monitor_job(guid, 1000)?;
    match result {
        Ok(monitor_client) => {
            println!("monitor success");
            monitor_loop(monitor_client, 1000)?;
            Ok(())
        }
        Err(e) => bail!("error from server {:?}", e),
    }
}

fn monitor_loop(mut monitor_client: BitsMonitorClient, wait_millis: u32) -> Result {
    loop {
        let status = monitor_client.get_status(wait_millis * 10)?;

        println!("{:?}", status);

        if !(status.state == BG_JOB_STATE_CONNECTING
            || status.state == BG_JOB_STATE_TRANSFERRING
            || status.state == BG_JOB_STATE_TRANSIENT_ERROR)
        {
            break;
        }
    }
    Ok(())
}

fn bits_bg(client: &mut BitsClient, guid: &OsStr) -> Result {
    bits_set_priority(client, guid, false)
}

fn bits_fg(client: &mut BitsClient, guid: &OsStr) -> Result {
    bits_set_priority(client, guid, true)
}

fn bits_set_priority(client: &mut BitsClient, guid: &OsStr, foreground: bool) -> Result {
    let guid = Guid::from_str(&guid.to_string_lossy())?;
    match client.set_job_priorty(guid, foreground)? {
        Ok(()) => Ok(()),
        Err(e) => bail!("error from server {:?}", e),
    }
}

fn bits_resume(client: &mut BitsClient, guid: &OsStr) -> Result {
    let guid = Guid::from_str(&guid.to_string_lossy())?;
    match client.resume_job(guid)? {
        Ok(()) => Ok(()),
        Err(e) => bail!("error from server {:?}", e),
    }
}

fn bits_complete(client: &mut BitsClient, guid: &OsStr) -> Result {
    let guid = Guid::from_str(&guid.to_string_lossy())?;
    match client.complete_job(guid)? {
        Ok(()) => Ok(()),
        Err(e) => bail!("error from server {:?}", e),
    }
}

fn bits_cancel(client: &mut BitsClient, guid: &OsStr) -> Result {
    let guid = Guid::from_str(&guid.to_string_lossy())?;
    match client.cancel_job(guid)? {
        Ok(()) => Ok(()),
        Err(e) => bail!("error from server {:?}", e),
    }
}
