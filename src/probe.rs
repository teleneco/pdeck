use std::fs::File;
use std::io::{BufRead, BufReader};
use std::net::IpAddr;
#[cfg(target_os = "windows")]
use std::net::{Ipv4Addr, ToSocketAddrs};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;
use tokio::io::{AsyncBufReadExt, BufReader as TokioBufReader};
use tokio::process::Command;
use tokio::sync::{Semaphore, mpsc, watch};

use crate::cli::{Args, IcmpBackendArg};
use crate::model::{ProbeEvent, Target, TargetKind};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum IcmpBackend {
    Exec,
    Api,
}

type ProbeResult = (String, String, f64, Option<String>);

struct ProbeWorker {
    index: usize,
    target: Target,
    args: Args,
    client: reqwest::Client,
    tx: mpsc::Sender<ProbeEvent>,
    pause_rx: watch::Receiver<bool>,
    semaphore: Arc<Semaphore>,
    icmp_backend: IcmpBackend,
}

pub async fn probe_loop(
    args: Args,
    targets: Vec<Target>,
    tx: mpsc::Sender<ProbeEvent>,
    pause_rx: watch::Receiver<bool>,
) -> Result<()> {
    let icmp_backend = select_icmp_backend(args.icmp_backend);
    let client = reqwest::Client::builder()
        .timeout(args.timeout.0)
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .expect("failed to build http client");
    let semaphore = Arc::new(Semaphore::new(args.concurrency.max(1)));
    for (index, target) in targets.into_iter().enumerate() {
        let tx = tx.clone();
        let args = args.clone();
        let client = client.clone();
        let pause_rx = pause_rx.clone();
        let semaphore = semaphore.clone();
        tokio::spawn(async move {
            run_probe_worker(ProbeWorker {
                index,
                target,
                args,
                client,
                tx,
                pause_rx,
                semaphore,
                icmp_backend,
            })
            .await;
        });
    }

    Ok(())
}

async fn run_probe_worker(worker: ProbeWorker) {
    let ProbeWorker {
        index,
        target,
        args,
        client,
        tx,
        mut pause_rx,
        semaphore,
        icmp_backend,
    } = worker;
    if matches!(target.kind, TargetKind::Icmp) {
        match icmp_backend {
            IcmpBackend::Exec => run_icmp_exec_worker(index, target, args, tx, pause_rx).await,
            IcmpBackend::Api => {
                run_icmp_api_worker(index, target, args, tx, pause_rx, semaphore).await
            }
        }
        return;
    }

    loop {
        while *pause_rx.borrow() {
            if pause_rx.changed().await.is_err() {
                return;
            }
        }
        tokio::time::sleep(args.interval.0).await;
        let permit = match semaphore.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => return,
        };

        let probe = match &target.kind {
            TargetKind::Icmp => probe_icmp_exec(&target, args.timeout.0).await,
            TargetKind::Tcp { port } => probe_tcp(&target.host, *port, args.timeout.0).await,
            TargetKind::Http { use_tls } => {
                probe_http(&client, &target.host, *use_tls, args.timeout.0).await
            }
        };

        let event = match probe {
            Ok((status, response, rtt_ms, resolved_ip)) => ProbeEvent {
                index,
                status: status.clone(),
                target: target.display.clone(),
                resolved_ip,
                response: response.clone(),
                log_line: format_log_line(&status, &target.display, &response, &target.description),
                ok: status == "o" || status == "200",
                rtt_ms: Some(rtt_ms),
                ts_ms: now_ts_ms(),
            },
            Err(err) => {
                let status = match target.kind {
                    TargetKind::Http { .. } => "000".to_string(),
                    _ => "x".to_string(),
                };
                let response = err.to_string();
                ProbeEvent {
                    index,
                    status: status.clone(),
                    target: target.display.clone(),
                    resolved_ip: None,
                    response: response.clone(),
                    log_line: format_log_line(
                        &status,
                        &target.display,
                        &response,
                        &target.description,
                    ),
                    ok: false,
                    rtt_ms: None,
                    ts_ms: now_ts_ms(),
                }
            }
        };
        drop(permit);

        if tx.send(event).await.is_err() {
            return;
        }
    }
}

fn select_icmp_backend(arg: IcmpBackendArg) -> IcmpBackend {
    match arg {
        IcmpBackendArg::Exec => IcmpBackend::Exec,
        IcmpBackendArg::Api => IcmpBackend::Api,
        IcmpBackendArg::Auto => {
            if cfg!(target_os = "macos") {
                IcmpBackend::Exec
            } else if cfg!(target_os = "windows") {
                IcmpBackend::Api
            } else {
                IcmpBackend::Exec
            }
        }
    }
}

async fn run_icmp_exec_worker(
    index: usize,
    target: Target,
    args: Args,
    tx: mpsc::Sender<ProbeEvent>,
    mut pause_rx: watch::Receiver<bool>,
) {
    loop {
        while *pause_rx.borrow() {
            if pause_rx.changed().await.is_err() {
                return;
            }
        }

        let mut last_resolved_ip = resolve_first_ip(&target.host).await;
        let mut child = match spawn_icmp_process(&target, args.interval.0, args.timeout.0) {
            Ok(child) => child,
            Err(err) => {
                let event = ProbeEvent {
                    index,
                    status: "x".to_string(),
                    target: target.display.clone(),
                    resolved_ip: last_resolved_ip.clone(),
                    response: err.to_string(),
                    log_line: format_log_line(
                        "x",
                        &target.display,
                        &err.to_string(),
                        &target.description,
                    ),
                    ok: false,
                    rtt_ms: None,
                    ts_ms: now_ts_ms(),
                };
                if tx.send(event).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill().await;
            return;
        };
        let mut lines = TokioBufReader::new(stdout).lines();

        loop {
            tokio::select! {
                changed = pause_rx.changed() => {
                    if changed.is_err() {
                        let _ = child.kill().await;
                        return;
                    }
                    if *pause_rx.borrow() {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        break;
                    }
                }
                line = lines.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            if let Some(resolved_ip) = parse_ping_output_ip(&line) {
                                last_resolved_ip = Some(resolved_ip);
                            }
                            if let Some(event) = build_icmp_event_from_line(index, &target, &line, last_resolved_ip.clone())
                                && tx.send(event).await.is_err()
                            {
                                let _ = child.kill().await;
                                return;
                            }
                        }
                        Ok(None) => {
                            let _ = child.wait().await;
                            break;
                        }
                        Err(err) => {
                            let event = ProbeEvent {
                                index,
                                status: "x".to_string(),
                                target: target.display.clone(),
                                resolved_ip: last_resolved_ip.clone(),
                                response: format!("ping output error: {err}"),
                                log_line: format_log_line(
                                    "x",
                                    &target.display,
                                    &format!("ping output error: {err}"),
                                    &target.description,
                                ),
                                ok: false,
                                rtt_ms: None,
                                ts_ms: now_ts_ms(),
                            };
                            let _ = tx.send(event).await;
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(args.timeout.0) => {
                    let status = "x".to_string();
                    let response = "request timeout".to_string();
                    let event = ProbeEvent {
                        index,
                        status: status.clone(),
                        target: target.display.clone(),
                        resolved_ip: last_resolved_ip.clone(),
                        response: response.clone(),
                        log_line: format_log_line(&status, &target.display, &response, &target.description),
                        ok: false,
                        rtt_ms: None,
                        ts_ms: now_ts_ms(),
                    };
                    if tx.send(event).await.is_err() {
                        let _ = child.kill().await;
                        return;
                    }
                }
            }
        }
    }
}

async fn run_icmp_api_worker(
    index: usize,
    target: Target,
    args: Args,
    tx: mpsc::Sender<ProbeEvent>,
    mut pause_rx: watch::Receiver<bool>,
    semaphore: Arc<Semaphore>,
) {
    loop {
        while *pause_rx.borrow() {
            if pause_rx.changed().await.is_err() {
                return;
            }
        }
        tokio::time::sleep(args.interval.0).await;
        let permit = match semaphore.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => return,
        };

        let probe = probe_icmp_api(&target, args.timeout.0).await;
        let event = match probe {
            Ok((status, response, rtt_ms, resolved_ip)) => ProbeEvent {
                index,
                status: status.clone(),
                target: target.display.clone(),
                resolved_ip,
                response: response.clone(),
                log_line: format_log_line(&status, &target.display, &response, &target.description),
                ok: status == "o",
                rtt_ms: Some(rtt_ms),
                ts_ms: now_ts_ms(),
            },
            Err(err) => ProbeEvent {
                index,
                status: "x".to_string(),
                target: target.display.clone(),
                resolved_ip: None,
                response: err.to_string(),
                log_line: format_log_line(
                    "x",
                    &target.display,
                    &err.to_string(),
                    &target.description,
                ),
                ok: false,
                rtt_ms: None,
                ts_ms: now_ts_ms(),
            },
        };
        drop(permit);

        if tx.send(event).await.is_err() {
            return;
        }
    }
}

fn now_ts_ms() -> u64 {
    Local::now().timestamp_millis().max(0) as u64
}

async fn probe_icmp_exec(target: &Target, timeout: Duration) -> Result<ProbeResult> {
    let host = target.host.as_str();
    validate_icmp_exec_host(host)?;
    let fallback_resolved_ip = resolve_first_ip(host).await;
    let program = if cfg!(target_os = "macos") {
        if host.contains(':') {
            "/sbin/ping6"
        } else {
            "/sbin/ping"
        }
    } else if cfg!(target_os = "linux") {
        if host.contains(':') { "ping6" } else { "ping" }
    } else {
        bail!("icmp exec backend currently supports macOS and Linux only");
    };
    let timeout_arg = exec_timeout_arg(timeout);
    let start = Instant::now();
    let output = Command::new(program)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .args(["-n", "-c", "1", "-W", timeout_arg.as_str(), host])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("failed to execute {}", program))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        bail!(if detail.is_empty() {
            "ping failed".to_string()
        } else {
            detail
        });
    }

    let elapsed = start.elapsed();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rtt_ms = parse_ping_time(&stdout).unwrap_or(elapsed.as_secs_f64() * 1000.0);
    let resolved_ip = parse_ping_output_ip(&stdout).or(fallback_resolved_ip);
    Ok(("o".to_string(), format_rtt_ms(rtt_ms), rtt_ms, resolved_ip))
}

async fn probe_icmp_api(target: &Target, timeout: Duration) -> Result<ProbeResult> {
    let host = target.host.clone();
    tokio::task::spawn_blocking(move || probe_icmp_api_blocking(&host, timeout))
        .await
        .context("icmp api worker join failed")?
}

#[cfg(target_os = "windows")]
fn probe_icmp_api_blocking(host: &str, timeout: Duration) -> Result<ProbeResult> {
    use std::ffi::c_void;
    use std::mem::{MaybeUninit, size_of};
    use std::ptr;

    type Handle = isize;

    #[repr(C)]
    struct IpOptionInformation {
        ttl: u8,
        tos: u8,
        flags: u8,
        options_size: u8,
        options_data: *mut u8,
    }

    #[repr(C)]
    struct IcmpEchoReply {
        address: u32,
        status: u32,
        round_trip_time: u32,
        data_size: u16,
        reserved: u16,
        data: *mut c_void,
        options: IpOptionInformation,
    }

    #[link(name = "iphlpapi")]
    unsafe extern "system" {
        fn IcmpCreateFile() -> Handle;
        fn IcmpCloseHandle(handle: Handle) -> i32;
        fn IcmpSendEcho(
            icmp_handle: Handle,
            destination_address: u32,
            request_data: *const c_void,
            request_size: u16,
            request_options: *const c_void,
            reply_buffer: *mut c_void,
            reply_size: u32,
            timeout: u32,
        ) -> u32;
    }

    const INVALID_HANDLE_VALUE: Handle = -1;
    const IP_SUCCESS: u32 = 0;
    const ICMP_REPLY_RESERVED_BYTES: usize = 8;
    const PAYLOAD: &[u8] = b"pdeck";

    #[repr(C)]
    struct IcmpReplyBuffer {
        reply: MaybeUninit<IcmpEchoReply>,
        extra: [u8; PAYLOAD.len() + ICMP_REPLY_RESERVED_BYTES],
    }

    let addr = resolve_ipv4(host)?;
    let handle = unsafe { IcmpCreateFile() };
    if handle == INVALID_HANDLE_VALUE {
        bail!("IcmpCreateFile failed");
    }

    let mut reply_buf = IcmpReplyBuffer {
        reply: MaybeUninit::uninit(),
        extra: [0; PAYLOAD.len() + ICMP_REPLY_RESERVED_BYTES],
    };
    let timeout_ms = timeout.as_millis().clamp(1, u128::from(u32::MAX)) as u32;

    let result = unsafe {
        IcmpSendEcho(
            handle,
            ipv4_network_order_u32(addr),
            PAYLOAD.as_ptr() as *const c_void,
            PAYLOAD.len() as u16,
            ptr::null(),
            &mut reply_buf as *mut IcmpReplyBuffer as *mut c_void,
            size_of::<IcmpReplyBuffer>() as u32,
            timeout_ms,
        )
    };

    let close_result = unsafe { IcmpCloseHandle(handle) };
    if close_result == 0 {
        bail!("IcmpCloseHandle failed");
    }

    if result == 0 {
        bail!("icmp request timed out");
    }

    let reply = unsafe { reply_buf.reply.assume_init_ref() };
    if reply.status != IP_SUCCESS {
        bail!("icmp status {}", reply.status);
    }

    let rtt_ms = reply.round_trip_time as f64;
    Ok((
        "o".to_string(),
        format_rtt_ms(rtt_ms),
        rtt_ms,
        Some(addr.to_string()),
    ))
}

#[cfg(not(target_os = "windows"))]
fn probe_icmp_api_blocking(_host: &str, _timeout: Duration) -> Result<ProbeResult> {
    bail!("icmp api backend currently supports Windows only")
}

fn spawn_icmp_process(
    target: &Target,
    interval: Duration,
    timeout: Duration,
) -> Result<tokio::process::Child> {
    let host = target.host.as_str();
    validate_icmp_exec_host(host)?;
    let program = if cfg!(target_os = "macos") {
        if host.contains(':') {
            "/sbin/ping6"
        } else {
            "/sbin/ping"
        }
    } else if cfg!(target_os = "linux") {
        if host.contains(':') { "ping6" } else { "ping" }
    } else {
        bail!("icmp exec backend currently supports macOS and Linux only");
    };
    let interval_secs = format!("{:.3}", interval.as_secs_f64());
    let timeout_arg = exec_timeout_arg(timeout);
    Command::new(program)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .args([
            "-n",
            "-i",
            interval_secs.as_str(),
            "-W",
            timeout_arg.as_str(),
            host,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to execute {}", program))
}

fn exec_timeout_arg(timeout: Duration) -> String {
    if cfg!(target_os = "linux") {
        timeout.as_secs().max(1).to_string()
    } else {
        timeout.as_millis().to_string()
    }
}

fn validate_icmp_exec_host(host: &str) -> Result<()> {
    if host.starts_with('-') {
        bail!("icmp target must not start with '-'");
    }
    Ok(())
}

async fn resolve_first_ip(host: &str) -> Option<String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(ip.to_string());
    }

    tokio::net::lookup_host((host, 0))
        .await
        .ok()?
        .next()
        .map(|addr| addr.ip().to_string())
}

#[cfg(target_os = "windows")]
fn resolve_ipv4(host: &str) -> Result<Ipv4Addr> {
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Ok(ip);
    }

    (host, 0)
        .to_socket_addrs()?
        .find_map(|addr| match addr.ip() {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        })
        .context("failed to resolve IPv4 address")
}

#[cfg(target_os = "windows")]
fn ipv4_network_order_u32(addr: Ipv4Addr) -> u32 {
    u32::from_ne_bytes(addr.octets())
}

fn parse_ping_time(stdout: &str) -> Option<f64> {
    stdout
        .lines()
        .find_map(|line| line.split("time=").nth(1))
        .and_then(|tail| tail.split_whitespace().next())
        .and_then(|value| value.strip_suffix("ms").or(Some(value)))
        .and_then(|value| value.parse::<f64>().ok())
}

fn parse_ping_output_ip(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        let candidate = if let Some(tail) = line.split_once(" from ").map(|(_, tail)| tail) {
            tail.split_once(": icmp_seq")
                .map(|(ip, _)| ip)
                .or_else(|| tail.split_once(": seq").map(|(ip, _)| ip))
                .unwrap_or_else(|| tail.split_whitespace().next().unwrap_or(""))
        } else {
            let (_, tail) = line.split_once("PING ")?;
            tail.split_once('(')
                .and_then(|(_, rest)| rest.split_once(')'))
                .map(|(ip, _)| ip)
                .unwrap_or("")
        };
        candidate.parse::<IpAddr>().ok().map(|ip| ip.to_string())
    })
}

fn build_icmp_event_from_line(
    index: usize,
    target: &Target,
    line: &str,
    resolved_ip: Option<String>,
) -> Option<ProbeEvent> {
    if let Some(rtt_ms) = parse_ping_time(line) {
        let status = "o".to_string();
        let response = format_rtt_ms(rtt_ms);
        return Some(ProbeEvent {
            index,
            status: status.clone(),
            target: target.display.clone(),
            resolved_ip,
            response: response.clone(),
            log_line: format_log_line(&status, &target.display, &response, &target.description),
            ok: true,
            rtt_ms: Some(rtt_ms),
            ts_ms: now_ts_ms(),
        });
    }

    if line.contains("Request timeout") {
        let status = "x".to_string();
        let response = "request timeout".to_string();
        return Some(ProbeEvent {
            index,
            status: status.clone(),
            target: target.display.clone(),
            resolved_ip,
            response: response.clone(),
            log_line: format_log_line(&status, &target.display, &response, &target.description),
            ok: false,
            rtt_ms: None,
            ts_ms: now_ts_ms(),
        });
    }

    None
}

pub fn parse_targets(path: &Path, arp_entries: bool) -> Result<Vec<Target>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut targets = Vec::new();

    for line in reader.lines() {
        let mut line = line?;
        if line.trim().is_empty() {
            continue;
        }

        line = line.replace('\t', " ");
        line = line.trim().to_string();
        if line.starts_with('#') {
            continue;
        }

        if arp_entries && line.starts_with("Internet ") {
            line = line["Internet ".len()..].trim_start().to_string();
        }

        let mut parts = line.split_whitespace();
        let raw_target = match parts.next() {
            Some(host) => host.to_string(),
            None => continue,
        };
        let description = parts.collect::<Vec<_>>().join(" ");
        let description = if description.is_empty() {
            "noname_host".to_string()
        } else {
            description
        };

        let display = raw_target.clone();
        let (host, kind) = parse_target_spec(&raw_target)?;

        targets.push(Target {
            display,
            host,
            kind,
            description,
        });
    }

    Ok(targets)
}

async fn probe_tcp(host: &str, port: u16, timeout: Duration) -> Result<ProbeResult> {
    let endpoint = format_socket_endpoint(host, port);
    let start = Instant::now();
    let stream = tokio::time::timeout(timeout, tokio::net::TcpStream::connect(&endpoint))
        .await
        .map_err(|_| anyhow!("connect timeout"))?
        .with_context(|| format!("tcp connect failed: {endpoint}"))?;
    let resolved_ip = stream.peer_addr().ok().map(|addr| addr.ip().to_string());
    drop(stream);
    let rtt_ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok((
        "o".to_string(),
        format_duration(Duration::from_secs_f64(rtt_ms / 1000.0)),
        rtt_ms,
        resolved_ip,
    ))
}

fn format_socket_endpoint(host: &str, port: u16) -> String {
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

async fn probe_http(
    client: &reqwest::Client,
    host: &str,
    use_tls: bool,
    timeout: Duration,
) -> Result<ProbeResult> {
    let url = normalize_url(host, use_tls);
    let start = Instant::now();
    let response = tokio::time::timeout(timeout, client.get(url).send())
        .await
        .map_err(|_| anyhow!("request timeout"))?
        .context("request failed")?;
    let resolved_ip = response.remote_addr().map(|addr| addr.ip().to_string());
    let status = response.status().as_u16().to_string();
    let elapsed = start.elapsed();
    let rtt_ms = elapsed.as_secs_f64() * 1000.0;
    Ok((status, format_duration(elapsed), rtt_ms, resolved_ip))
}

fn normalize_url(host: &str, use_tls: bool) -> String {
    if use_tls {
        format!("https://{host}")
    } else {
        format!("http://{host}")
    }
}

fn parse_target_spec(raw_target: &str) -> Result<(String, TargetKind)> {
    if let Some(rest) = raw_target.strip_prefix("tcp://") {
        let (host, port) =
            split_host_port(rest).context("tcp:// targets must include host:port")?;
        return Ok((host.to_string(), TargetKind::Tcp { port }));
    }

    if let Some(rest) = raw_target.strip_prefix("http://") {
        return Ok((rest.to_string(), TargetKind::Http { use_tls: false }));
    }

    if let Some(rest) = raw_target.strip_prefix("https://") {
        return Ok((rest.to_string(), TargetKind::Http { use_tls: true }));
    }

    if let Some((host, port)) = split_host_port(raw_target) {
        return Ok((host.to_string(), TargetKind::Tcp { port }));
    }

    Ok((raw_target.to_string(), TargetKind::Icmp))
}

fn split_host_port(input: &str) -> Option<(&str, u16)> {
    if let Some(rest) = input.strip_prefix('[') {
        let (host, tail) = rest.split_once("]:")?;
        let port = tail.parse::<u16>().ok()?;
        return Some((host, port));
    }

    let (host, port) = input.rsplit_once(':')?;
    if host.contains(':') {
        return None;
    }
    let port = port.parse::<u16>().ok()?;
    Some((host, port))
}

fn format_log_line(status: &str, target: &str, response: &str, description: &str) -> String {
    format!(
        "[{}] {} {} {} {}\n",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        status,
        target,
        response,
        description
    )
}

fn format_duration(duration: Duration) -> String {
    let millis = duration.as_secs_f64() * 1000.0;
    format!("{millis:.3}ms")
}

fn format_rtt_ms(rtt_ms: f64) -> String {
    format!("{rtt_ms:.3}ms")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ipv4_from_ping_reply() {
        let line = "64 bytes from 93.184.216.34: icmp_seq=1 ttl=56 time=12.345 ms";

        assert_eq!(
            parse_ping_output_ip(line),
            Some("93.184.216.34".to_string())
        );
    }

    #[test]
    fn parses_ipv6_from_ping_reply() {
        let line =
            "64 bytes from 2606:2800:220:1:248:1893:25c8:1946: icmp_seq=1 ttl=56 time=12.345 ms";

        assert_eq!(
            parse_ping_output_ip(line),
            Some("2606:2800:220:1:248:1893:25c8:1946".to_string())
        );
    }

    #[test]
    fn parses_ip_from_ping_header() {
        let output = "PING netflix.com (52.89.124.203) 56(84) bytes of data.";

        assert_eq!(
            parse_ping_output_ip(output),
            Some("52.89.124.203".to_string())
        );
    }

    #[test]
    fn formats_ipv6_tcp_endpoint_with_brackets() {
        assert_eq!(
            format_socket_endpoint("2606:2800:220:1:248:1893:25c8:1946", 443),
            "[2606:2800:220:1:248:1893:25c8:1946]:443"
        );
    }

    #[test]
    fn rejects_icmp_exec_targets_that_look_like_options() {
        let err = validate_icmp_exec_host("-i").unwrap_err();
        assert!(err.to_string().contains("must not start with '-'"));
    }
}
