/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// These are full integration tests that use the BITS service.

// TODO
// It may make sense to restrict how many tests can run at once. BITS is only supposed to support
// four simultaneous notifications per user, it is not impossible that this test suite could
// exceed that.
//
// TODO
// The timings used for these tests are too sensitive, timeouts should be much longer and the
// expected delay should be quite long.

#![cfg(test)]
extern crate bits;
extern crate lazy_static;
extern crate rand;
extern crate regex;
extern crate tempdir;

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::mem;
use std::net::{TcpListener, TcpStream};
use std::panic;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use self::{
    bits::BackgroundCopyManager,
    lazy_static::lazy_static,
    rand::{thread_rng, Rng},
    regex::bytes::Regex,
    tempdir::TempDir,
};
use super::{
    super::{BitsJobState, Error},
    BitsProxyUsage, InProcessClient, StartJobSuccess,
};

static SERVER_ADDRESS: &'static str = "127.0.0.1";

lazy_static! {
    static ref TEST_MUTEX: Mutex<()> = Mutex::new(());
}

fn format_server_url(port: u16, name: &str) -> OsString {
    format!("http://{}:{}/{}", SERVER_ADDRESS, port, name).into()
}

fn format_job_name(name: &str) -> OsString {
    format!("InProcessClient Test {}", name).into()
}

fn format_dir_prefix(tmp_dir: &TempDir) -> OsString {
    let mut dir = tmp_dir.path().to_path_buf().into_os_string();
    dir.push("\\");
    dir
}

fn cancel_jobs(name: &OsStr) {
    BackgroundCopyManager::connect()
        .unwrap()
        .cancel_jobs_by_name(name)
        .unwrap();
}

fn close_mock_http_server(port: u16) {
    let mut connection = TcpStream::connect((SERVER_ADDRESS, port)).unwrap();
    connection.write(b"SHUTDOWN").unwrap();
    connection.flush().unwrap();
    let mut buf = [0; 2];
    connection.read(&mut buf[..]).unwrap();

    // ensure that the port is available again
    let _ = TcpListener::bind((SERVER_ADDRESS, port)).unwrap();
}

struct HttpServerResponses {
    body: Box<[u8]>,
    delay: u64,
}

fn mock_http_server(name: &'static str, responses: HttpServerResponses) -> u16 {
    let mut bind_retries = 10;

    let (listener, port) = loop {
        let port = thread_rng().gen_range(1024, 0x10000u32) as u16;
        match TcpListener::bind((SERVER_ADDRESS, port)) {
            Ok(listener) => {
                break (listener, port);
            }
            r @ Err(_) => {
                if bind_retries == 0 {
                    r.unwrap();
                }
                bind_retries -= 1;
                continue;
            }
        }
    };

    let _join = thread::Builder::new()
        .name(format!("mock_http_server {}", name))
        .spawn(move || {
            let error_404 = Regex::new(r"^((GET)|(HEAD)) [[:print:]]*/error_404 ").unwrap();
            let error_500 = Regex::new(r"^((GET)|(HEAD)) [[:print:]]*/error_500 ").unwrap();
            let mut shut_down = false;
            loop {
                match listener.accept() {
                    Ok((mut socket, _addr)) => {
                        socket
                            .set_read_timeout(Some(Duration::from_millis(1000)))
                            .unwrap();
                        let mut s = Vec::new();
                        for b in Read::by_ref(&mut socket).bytes() {
                            if b.is_err() {
                                //eprintln!("read error {:?}", b);
                                break;
                            }
                            let b = b.unwrap();
                            s.push(b);
                            if s.starts_with(b"SHUTDOWN") {
                                shut_down = true;
                                break;
                            }
                            if s.ends_with(b"\r\n\r\n") {
                                break;
                            }
                        }

                        if shut_down {
                            mem::drop(listener);
                            socket.write(b"OK").unwrap();
                            return;
                        }

                        if s.starts_with(b"HEAD") || s.starts_with(b"GET") {
                            if error_404.is_match(&s) {
                                thread::sleep(Duration::from_millis(responses.delay));
                                let result = socket.write(b"HTTP/1.1 404 Not Found\r\n\r\n")
                                    .and_then(|_| {
                                        socket.flush()
                                    });
                                if let Err(e) = result {
                                    eprintln!("error writing 404 header {:?}", e);
                                }
                                continue;
                            }
                            if error_500.is_match(&s) {
                                thread::sleep(Duration::from_millis(responses.delay));
                                let result = socket.write(b"HTTP/1.1 500 Internal Server Error\r\n\r\n")
                                    .and_then(|_| {
                                        socket.flush()
                                    });
                                if let Err(e) = result {
                                    eprintln!("error writing 500 header {:?}", e);
                                }
                                continue;
                            }

                            let result = socket.write(
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                                    responses.body.len()
                                )
                                .as_bytes(),
                            );
                            if let Err(e) = result {
                                eprintln!("error writing header {:?}", e);
                                continue;
                            }
                        } else if s.starts_with(b"GET") {
                            continue;
                        }

                        if s.starts_with(b"GET") {
                            thread::sleep(Duration::from_millis(responses.delay));
                            let result = socket.write(&responses.body);
                            if let Err(_e) = result {
                                //eprintln!("error writing content {:?}", _e);
                                continue;
                            }
                        }

                        if let Err(e) = socket.flush() {
                            eprintln!("error flushing {:?}", e);
                            continue;
                        }
                    }
                    Err(e) => {
                        eprintln!("{:?}", e);
                        panic!(e);
                    }
                }
            }
        });

    port
}

// Test wrapper to ensure jobs are canceled, set up name strings
macro_rules! test {
    (fn $name:ident($param:ident : &str, $tmpdir:ident : &TempDir) $body:block) => {
        #[test]
        #[ignore]
        fn $name() {
            let $param = stringify!($name);
            let $tmpdir = &TempDir::new($param).unwrap();

            let result = panic::catch_unwind(|| $body);

            cancel_jobs(&format_job_name($param));

            if let Err(e) = result {
                panic::resume_unwind(e);
            }
        }
    };
}

test! {
    fn start_monitor_and_cancel(name: &str, tmp_dir: &TempDir) {
        let port = mock_http_server(name, HttpServerResponses {
            body: name.to_owned().into_boxed_str().into_boxed_bytes(),
            delay: 1000,
        });

        let mut client = InProcessClient::new(format_job_name(name), tmp_dir.path().into()).unwrap();

        let interval = 10 * 1000;
        let timeout = 60 * 1000;

        let (StartJobSuccess {guid}, mut monitor) =
            client.start_job(
                format_server_url(port, name),
                name.into(),
                BitsProxyUsage::Preconfig,
                interval
                ).unwrap();

        // cancel in ~250ms
        let _join = thread::Builder::new()
            .spawn(move || {
                thread::sleep(Duration::from_millis(250));
                client.cancel_job(guid).unwrap();
            });

        let start = Instant::now();

        // First immediate report
        monitor.get_status(timeout).expect("should initially be ok").unwrap();

        // ~250ms the cancel should cause an immediate disconnect (otherwise we wouldn't get
        // an update until 1s when the transfer completes or 10s when the interval expires)
        match monitor.get_status(timeout) {
            Err(Error::NotConnected) => {},
            Ok(r) => panic!("unexpected success from get_status() {:?}", r),
            Err(e) => panic!("unexpected failure from get_status() {:?}", e),
        }
        assert!(start.elapsed() < Duration::from_millis(500));

        // This will take ~750ms until BITS's HTTP request completes and this shutdown request
        // can be serviced.
        close_mock_http_server(port);
    }
}

test! {
    fn start_monitor_and_complete(name: &str, tmp_dir: &TempDir) {
        let file_path = tmp_dir.path().join(name);

        let port = mock_http_server(name, HttpServerResponses {
            body: name.to_owned().into_boxed_str().into_boxed_bytes(),
            delay: 500,
        });

        let mut client = InProcessClient::new(format_job_name(name), format_dir_prefix(tmp_dir)).unwrap();

        let interval = 100;
        let timeout = 1000;

        let (StartJobSuccess {guid}, mut monitor) =
            client.start_job(format_server_url(port, name).into(), name.into(), BitsProxyUsage::Preconfig, interval).unwrap();

        // get status reports until transfer finishes (~500ms)
        let mut completed = false;
        loop {
            match monitor.get_status(timeout) {
                Err(e) => {
                    if completed {
                        break;
                    } else {
                        panic!("monitor failed before completion {:?}", e);
                    }
                }
                Ok(Ok(status)) => match BitsJobState::from(status.state) {
                    BitsJobState::Connecting
                        | BitsJobState::Transferring => {
                            //eprintln!("{:?}", BitsJobState::from(status.state));
                            //eprintln!("{:?}", status);
                        }
                    BitsJobState::Transferred => {
                        client.complete_job(guid.clone()).unwrap();
                        completed = true;
                    }
                    _ => {
                        panic!(format!("{:?}", status));
                    }
                }
                Ok(Err(e)) => panic!(format!("{:?}", e)),
            }
        }

        close_mock_http_server(port);

        // Verify file contents
        let result = panic::catch_unwind(|| {
            let mut file = File::open(file_path.clone()).unwrap();
            let mut v = Vec::new();
            file.read_to_end(&mut v).unwrap();
            assert_eq!(v, name.as_bytes());
        });

        fs::remove_file(file_path).unwrap();

        if let Err(e) = result {
            panic::resume_unwind(e);
        }
    }
}

test! {
    fn async_notification(name: &str, tmp_dir: &TempDir) {
        let port = mock_http_server(name, HttpServerResponses {
            body: name.to_owned().into_boxed_str().into_boxed_bytes(),
            delay: 250,
        });

        let mut client = InProcessClient::new(format_job_name(name), format_dir_prefix(tmp_dir)).unwrap();

        let interval = 10 * 1000;
        let timeout = 1000;

        let (_, mut monitor) =
            client.start_job(format_server_url(port, name).into(), name.into(), BitsProxyUsage::Preconfig, interval).unwrap();

        // Start the timer now, the initial job creation may be delayed by BITS service startup.
        let start = Instant::now();

        // First immediate report
        monitor.get_status(timeout).expect("should initially be ok").unwrap();
        assert!(start.elapsed() < Duration::from_millis(100));

        // Transferred notification should come when the job completes in ~250 ms, otherwise we
        // will be stuck until timeout.
        let status = monitor.get_status(timeout).expect("should get status update").unwrap();
        assert!(start.elapsed() < Duration::from_millis(1000));
        assert_eq!(status.state, BitsJobState::Transferred);

        let err = monitor.get_status(timeout).expect_err("should timeout");
        assert_eq!(err, Error::Timeout);

        close_mock_http_server(port);

        // job will be cancelled by macro
    }
}

test! {
    fn change_interval(name: &str, tmp_dir: &TempDir) {
        let port = mock_http_server(name, HttpServerResponses {
            body: name.to_owned().into_boxed_str().into_boxed_bytes(),
            delay: 1000,
        });

        let mut client = InProcessClient::new(format_job_name(name), format_dir_prefix(tmp_dir)).unwrap();

        let interval = 10 * 1000;
        let timeout = 1000;

        let (StartJobSuccess { guid }, mut monitor) =
            client.start_job(format_server_url(port, name).into(), name.into(), BitsProxyUsage::Preconfig, interval).unwrap();

        let start = Instant::now();

        // reduce monitor interval in ~250ms to 500ms
        let _join = thread::Builder::new()
            .spawn(move || {
                thread::sleep(Duration::from_millis(250));
                client.set_update_interval(guid, 500).unwrap();
            });

        // First immediate report
        monitor.get_status(timeout).expect("should initially be ok").unwrap();
        assert!(start.elapsed() < Duration::from_millis(100));

        // Next report should be rescheduled to 500ms by the spawned thread
        monitor.get_status(timeout).expect("expected second status").unwrap();
        assert!(start.elapsed() < Duration::from_millis(750));
        assert!(start.elapsed() > Duration::from_millis(400));

        close_mock_http_server(port);

        // job will be cancelled by macro
    }
}

test! {
    fn permanent_error(name: &str, tmp_dir: &TempDir) {
        let port = mock_http_server(name, HttpServerResponses {
            body: name.to_owned().into_boxed_str().into_boxed_bytes(),
            delay: 100,
        });

        let mut client = InProcessClient::new(format_job_name(name), format_dir_prefix(tmp_dir)).unwrap();

        let interval = 10 * 1000;
        let timeout = 1000;

        let (_, mut monitor) =
            client.start_job(format_server_url(port, "error_404").into(), name.into(), BitsProxyUsage::Preconfig, interval).unwrap();

        // Start the timer now, the initial job creation may be delayed by BITS service startup.
        let start = Instant::now();

        // First immediate report
        monitor.get_status(timeout).expect("should initially be ok").unwrap();
        assert!(start.elapsed() < Duration::from_millis(100));

        // Error notification should come with HEAD response in 100ms.
        let status = monitor.get_status(timeout).expect("should get status update").unwrap();
        assert!(start.elapsed() < Duration::from_millis(1000));
        assert_eq!(status.state, BitsJobState::Error);

        close_mock_http_server(port);

        // job will be cancelled by macro
    }
}

test! {
    fn transient_error(name: &str, tmp_dir: &TempDir) {
        let port = mock_http_server(name, HttpServerResponses {
            body: name.to_owned().into_boxed_str().into_boxed_bytes(),
            delay: 100,
        });

        let mut client = InProcessClient::new(format_job_name(name), format_dir_prefix(tmp_dir)).unwrap();

        let interval = 1000;
        let timeout = 10 * 1000;

        let (_, mut monitor) =
            client.start_job(format_server_url(port, "error_500").into(), name.into(), BitsProxyUsage::Preconfig, interval).unwrap();

        // Start the timer now, the initial job creation may be delayed by BITS service startup.
        let start = Instant::now();

        // First immediate report
        monitor.get_status(timeout).expect("should initially be ok").unwrap();
        assert!(start.elapsed() < Duration::from_millis(100));

        // Transferred notification should come when the job completes in ~250 ms, otherwise we
        // will be stuck until timeout.
        let status = monitor.get_status(timeout).expect("should get status update").unwrap();
        assert!(start.elapsed() > Duration::from_millis(800));
        assert!(start.elapsed() < Duration::from_millis(2000));
        assert_eq!(status.state, BitsJobState::TransientError);

        close_mock_http_server(port);

        // job will be cancelled by macro
    }
}
