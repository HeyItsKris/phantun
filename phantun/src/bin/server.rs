use clap::{crate_version, Arg, ArgAction, Command};
use fake_tcp::packet::MAX_PACKET_LEN;
use fake_tcp::Stack;
use log::{debug, error, info};
use phantun::control_plane::{ControlMode, ControlState, StopReason, start_control_plane};
use phantun::task_group::TaskGroup;
use phantun::utils::{
    assign_ipv6_address, new_udp_reuseport, read_interface_kernel_state, wait_for_termination_signal,
};
use std::fs;
use std::io;
use std::net::Ipv4Addr;
use std::time::Duration;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::time;
use tokio_tun::TunBuilder;
use tokio_util::sync::CancellationToken;

use phantun::UDP_TTL;

const STOPPING_ACK_TIMEOUT: Duration = Duration::from_millis(800);
const DOWN_DELIVERY_TIMEOUT: Duration = Duration::from_millis(300);

#[tokio::main]
async fn main() -> io::Result<()> {
    pretty_env_logger::init();

    let matches = Command::new("Phantun Server")
        .version(crate_version!())
        .author("Datong Sun (github.com/dndx)")
        .arg(
            Arg::new("local")
                .short('l')
                .long("local")
                .required(true)
                .value_name("PORT")
                .help("Sets the port where Phantun Server listens for incoming Phantun Client TCP connections")
        )
        .arg(
            Arg::new("remote")
                .short('r')
                .long("remote")
                .required(true)
                .value_name("IP or HOST NAME:PORT")
                .help("Sets the address or host name and port where Phantun Server forwards UDP packets to, IPv6 address need to be specified as: \"[IPv6]:PORT\"")
        )
        .arg(
            Arg::new("tun")
                .long("tun")
                .required(false)
                .value_name("tunX")
                .help("Sets the Tun interface name, if absent, pick the next available name")
                .default_value("")
        )
        .arg(
            Arg::new("tun_local")
                .long("tun-local")
                .required(false)
                .value_name("IP")
                .help("Sets the Tun interface local address (O/S's end)")
                .default_value("192.168.201.1")
        )
        .arg(
            Arg::new("tun_peer")
                .long("tun-peer")
                .required(false)
                .value_name("IP")
                .help("Sets the Tun interface destination (peer) address (Phantun Server's end). \
                       You will need to setup DNAT rules to this address in order for Phantun Server \
                       to accept TCP traffic from Phantun Client")
                .default_value("192.168.201.2")
        )
        .arg(
            Arg::new("ipv4_only")
                .long("ipv4-only")
                .short('4')
                .required(false)
                .help("Do not assign IPv6 addresses to Tun interface")
                .action(ArgAction::SetTrue)
                .conflicts_with_all(["tun_local6", "tun_peer6"]),
        )
        .arg(
            Arg::new("tun_local6")
                .long("tun-local6")
                .required(false)
                .value_name("IP")
                .help("Sets the Tun interface IPv6 local address (O/S's end)")
                .default_value("fcc9::1")
        )
        .arg(
            Arg::new("tun_peer6")
                .long("tun-peer6")
                .required(false)
                .value_name("IP")
                .help("Sets the Tun interface IPv6 destination (peer) address (Phantun Client's end). \
                       You will need to setup SNAT/MASQUERADE rules on your Internet facing interface \
                       in order for Phantun Client to connect to Phantun Server")
                .default_value("fcc9::2")
        )
        .arg(
            Arg::new("handshake_packet")
                .long("handshake-packet")
                .required(false)
                .value_name("PATH")
                .help("Specify a file, which, after TCP handshake, its content will be sent as the \
                      first data packet to the client.\n\
                      Note: ensure this file's size does not exceed the MTU of the outgoing interface. \
                      The content is always sent out in a single packet and will not be further segmented")
        )
        .arg(
            Arg::new("control_target")
                .long("control-target")
                .required(false)
                .value_name("UNIX_PATH")
                .help("Push control-plane status to this UNIX socket path. Repeat this option to configure multiple targets")
                .action(ArgAction::Append)
                .num_args(1)
        )
        .get_matches();

    let local_port: u16 = matches
        .get_one::<String>("local")
        .unwrap()
        .parse()
        .expect("bad local port");

    let remote_addr = tokio::net::lookup_host(matches.get_one::<String>("remote").unwrap())
        .await
        .expect("bad remote address or host")
        .next()
        .expect("unable to resolve remote host name");

    info!("Remote address is: {}", remote_addr);

    let tun_local: Ipv4Addr = matches
        .get_one::<String>("tun_local")
        .unwrap()
        .parse()
        .expect("bad local address for Tun interface");
    let tun_peer: Ipv4Addr = matches
        .get_one::<String>("tun_peer")
        .unwrap()
        .parse()
        .expect("bad peer address for Tun interface");

    let (tun_local6, tun_peer6) = if matches.get_flag("ipv4_only") {
        (None, None)
    } else {
        (
            matches
                .get_one::<String>("tun_local6")
                .map(|v| v.parse().expect("bad local address for Tun interface")),
            matches
                .get_one::<String>("tun_peer6")
                .map(|v| v.parse().expect("bad peer address for Tun interface")),
        )
    };

    let tun_name = matches.get_one::<String>("tun").unwrap();
    let control_targets: Vec<String> = matches
        .get_many::<String>("control_target")
        .map(|targets| targets.cloned().collect())
        .unwrap_or_default();
    let handshake_packet: Option<Vec<u8>> = matches
        .get_one::<String>("handshake_packet")
        .map(fs::read)
        .transpose()?;

    let control_state = ControlState::new(
        ControlMode::Server,
        Some(format!("0.0.0.0:{local_port}")),
        Some(remote_addr.to_string()),
    );
    let control_reporter = start_control_plane(&control_targets, control_state)?;
    control_reporter.publish_starting();

    let num_cpus = num_cpus::get();
    info!("{} cores available", num_cpus);

    let tun = TunBuilder::new()
        .name(tun_name) // if name is empty, then it is set by kernel.
        .up() // or set it up manually using `sudo ip link set <tun-name> up`.
        .address(tun_local)
        .destination(tun_peer)
        .queues(num_cpus)
        .build()
        .unwrap();

    if let (Some(tun_local6), Some(tun_peer6)) = (tun_local6, tun_peer6) {
        assign_ipv6_address(tun[0].name(), tun_local6, tun_peer6);
    }

    let tun_device = tun[0].name().to_string();
    let kernel_state = read_interface_kernel_state(&tun_device);
    info!("Created TUN device {}", tun_device);
    control_reporter.publish_up(
        Some(tun_device),
        kernel_state.mtu,
        kernel_state.addr4,
        kernel_state.addr6,
    );

    //thread::sleep(time::Duration::from_secs(5));
    let mut stack = Stack::new(tun, tun_local, tun_local6);
    stack.listen(local_port);
    info!("Listening on {}", local_port);
    let task_group = TaskGroup::new();
    let main_loop_tasks = task_group.clone();

    let mut main_loop = tokio::spawn(async move {
        let mut buf_udp = [0u8; MAX_PACKET_LEN];
        let mut buf_tcp = [0u8; MAX_PACKET_LEN];
        let shutdown = main_loop_tasks.token();

        loop {
            let sock = tokio::select! {
                _ = shutdown.cancelled() => break,
                sock = stack.accept() => Arc::new(sock),
            };
            info!("New connection: {}", sock);
            if let Some(ref p) = handshake_packet {
                if sock.send(p).await.is_none() {
                    error!("Failed to send handshake packet to remote, closing connection.");
                    continue;
                }

                debug!("Sent handshake packet to: {}", sock);
            }

            let packet_received = Arc::new(Notify::new());
            let quit = CancellationToken::new();
            let udp_sock = UdpSocket::bind(if remote_addr.is_ipv4() {
                "0.0.0.0:0"
            } else {
                "[::]:0"
            })
            .await?;
            let local_addr = udp_sock.local_addr()?;
            drop(udp_sock);

            for i in 0..num_cpus {
                let sock = sock.clone();
                let quit = quit.clone();
                let packet_received = packet_received.clone();
                let udp_sock = new_udp_reuseport(local_addr);
                let shutdown = shutdown.clone();

                main_loop_tasks.spawn(async move {
                    udp_sock.connect(remote_addr).await.unwrap();

                    loop {
                        tokio::select! {
                            Ok(size) = udp_sock.recv(&mut buf_udp) => {
                                if sock.send(&buf_udp[..size]).await.is_none() {
                                    quit.cancel();
                                    return;
                                }

                                packet_received.notify_one();
                            },
                            res = sock.recv(&mut buf_tcp) => {
                                match res {
                                    Some(size) => {
                                        if size > 0
                                            && let Err(e) = udp_sock.send(&buf_tcp[..size]).await {
                                                error!("Unable to send UDP packet to {}: {}, closing connection", e, remote_addr);
                                                quit.cancel();
                                                return;
                                            }
                                    },
                                    None => {
                                        quit.cancel();
                                        return;
                                    },
                                }

                                packet_received.notify_one();
                            },
                            _ = quit.cancelled() => {
                                debug!("worker {} terminated", i);
                                return;
                            },
                            _ = shutdown.cancelled() => {
                                quit.cancel();
                                return;
                            },
                        };
                    }
                });
            }

            let shutdown = shutdown.clone();
            main_loop_tasks.spawn(async move {
                loop {
                    let read_timeout = time::sleep(UDP_TTL);
                    let packet_received_fut = packet_received.notified();

                    tokio::select! {
                        _ = read_timeout => {
                            info!("No traffic seen in the last {:?}, closing connection", UDP_TTL);

                            quit.cancel();
                            return;
                        },
                        _ = quit.cancelled() => return,
                        _ = shutdown.cancelled() => {
                            quit.cancel();
                            return;
                        },
                        _ = packet_received_fut => {},
                    }
                }
            });
        }

        Ok(())
    });

    tokio::select! {
        main_result = (&mut main_loop) => {
            let exit_result = match main_result {
                Ok(result) => result,
                Err(err) => Err(io::Error::other(format!("main loop join failure: {err}"))),
            };
            let stop_reason = if exit_result.is_ok() {
                StopReason::ProcessExit
            } else {
                StopReason::MainLoopError
            };

            if !control_reporter
                .publish_stopping_and_wait_ack(stop_reason, STOPPING_ACK_TIMEOUT)
                .await
            {
                info!("No stopping ACK received within {:?}", STOPPING_ACK_TIMEOUT);
            }

            task_group.cancel();
            task_group.wait().await;

            if !control_reporter
                .publish_down_and_wait_delivery(DOWN_DELIVERY_TIMEOUT)
                .await
            {
                info!(
                    "Down event delivery was not observed within {:?}",
                    DOWN_DELIVERY_TIMEOUT
                );
            }

            exit_result
        }
        signal_name = wait_for_termination_signal() => {
            info!("Received {}, initiating shutdown handshake", signal_name);
            if !control_reporter
                .publish_stopping_and_wait_ack(StopReason::Signal, STOPPING_ACK_TIMEOUT)
                .await
            {
                info!("No stopping ACK received within {:?}", STOPPING_ACK_TIMEOUT);
            }

            task_group.cancel();
            main_loop.abort();
            let _ = (&mut main_loop).await;
            task_group.wait().await;

            if !control_reporter
                .publish_down_and_wait_delivery(DOWN_DELIVERY_TIMEOUT)
                .await
            {
                info!(
                    "Down event delivery was not observed within {:?}",
                    DOWN_DELIVERY_TIMEOUT
                );
            }

            Ok(())
        }
    }
}
