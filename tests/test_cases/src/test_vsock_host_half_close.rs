#![cfg(any(feature = "host", target_os = "linux"))]

use macros::{guest, host};
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::time::Duration;

pub struct TestVsockHostHalfClose;

const VSOCK_PORT: u32 = 1234;
const VERIFIED: &[u8] = b"verified!";

fn stream_set_timeouts(stream: &mut UnixStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
}

fn stream_expect_msg(stream: &mut UnixStream, expected: &[u8]) {
    let mut buf = vec![0; expected.len()];
    stream.read_exact(&mut buf[..]).unwrap();
    assert_eq!(&buf[..], expected);
}

#[host]
mod host {
    use super::*;
    #[cfg(feature = "dynamic-linking")]
    use krun::DeviceManager;

    use std::io::Write;
    use std::net::Shutdown;
    use std::os::unix::net::UnixListener;
    use std::thread;

    use crate::common::{build_init_config, init_krun, setup_standard_devices};
    use crate::{ShouldRun, Test, TestSetup};

    #[cfg(feature = "dynamic-linking")]
    fn require_symbols() -> Result<(), libloading::Error> {
        crate::common::require_vm_symbols()?;
        krun::require(
            None,
            &[
                krun::Symbol::KrunVsockDeviceNew,
                krun::Symbol::KrunVsockDeviceDestroy,
                krun::Symbol::KrunVsockDeviceAddUnixPort,
            ],
        )
    }

    fn server(listener: UnixListener) {
        let (mut stream, _addr) = listener.accept().unwrap();
        stream_set_timeouts(&mut stream);
        stream.write_all(b"ping!").unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        stream_expect_msg(&mut stream, b"pong!");
        drop(stream);

        // The guest prints OK only after this, so a reply the host never got fails the test.
        let (mut done, _addr) = listener.accept().unwrap();
        stream_set_timeouts(&mut done);
        done.write_all(VERIFIED).unwrap();
    }

    impl Test for TestVsockHostHalfClose {
        fn should_run(&self) -> ShouldRun {
            #[cfg(feature = "dynamic-linking")]
            if require_symbols().is_err() {
                return ShouldRun::No("feature not enabled in this libkrun build");
            }
            ShouldRun::Yes
        }

        fn start_vm(self: Box<Self>, test_setup: TestSetup) -> anyhow::Result<()> {
            init_krun()?;
            #[cfg(feature = "dynamic-linking")]
            require_symbols().unwrap();

            let sock_path = test_setup.tmp_dir.join("test.sock");
            let listener = UnixListener::bind(&sock_path).unwrap();
            thread::spawn(move || server(listener));

            let init_config = build_init_config(&test_setup.test_case, &[]);
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            let stderr = std::io::stderr();
            let (mut devices, payload) =
                setup_standard_devices(&test_setup, &init_config, &stdin, &stdout, &stderr)?;
            let mut vsock = krun::VsockDevice::new(3, krun::TsiFlags::empty())
                .map_err(|e| anyhow::anyhow!("VsockDevice: {e:?}"))?;
            vsock.add_unix_port(VSOCK_PORT, sock_path.to_str().unwrap(), false);
            devices.add(vsock);

            let vmm = krun::VmmBuilder::new()
                .vcpus(1)
                .map_err(|e| anyhow::anyhow!("vcpus: {e:?}"))?
                .ram_mib(1024)
                .map_err(|e| anyhow::anyhow!("ram_mib: {e:?}"))?
                .payload(payload)
                .devices(devices)
                .build()
                .map_err(|e| anyhow::anyhow!("build: {e:?}"))?;

            vmm.run();
            unreachable!()
        }
    }
}

#[guest]
mod guest {
    use super::*;
    use crate::Test;

    use nix::libc::VMADDR_CID_HOST;
    use nix::sys::socket::{AddressFamily, SockFlag, SockType, VsockAddr, connect, socket};
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::thread;
    use std::time::Instant;

    fn connect_host() -> UnixStream {
        let sock = socket(
            AddressFamily::Vsock,
            SockType::Stream,
            SockFlag::empty(),
            None,
        )
        .unwrap();
        connect(
            sock.as_raw_fd(),
            &VsockAddr::new(VMADDR_CID_HOST, VSOCK_PORT),
        )
        .unwrap();
        let mut stream = UnixStream::from(sock);
        stream_set_timeouts(&mut stream);
        stream
    }

    impl Test for TestVsockHostHalfClose {
        fn in_guest(self: Box<Self>) {
            let mut stream = connect_host();
            stream_expect_msg(&mut stream, b"ping!");
            // The host shut only its write side: an end of stream, not a reset, and the
            // other direction still carries data.
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
            stream.write_all(b"pong!").unwrap();

            stream_expect_msg(&mut connect_host(), VERIFIED);

            // The host has since closed its socket for good, while this one stays open: the
            // close must still reach the guest.
            let deadline = Instant::now() + Duration::from_secs(3);
            while stream.write_all(b"x").is_ok() {
                assert!(Instant::now() < deadline, "the host's close never arrived");
                thread::sleep(Duration::from_millis(10));
            }

            println!("OK");
        }
    }
}
