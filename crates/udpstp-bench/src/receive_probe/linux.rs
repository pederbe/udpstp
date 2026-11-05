//! Native Linux socket adapter. No protocol/runtime API is widened.

use std::fs;
use std::io::{IoSlice, IoSliceMut};
use std::net::UdpSocket;
use std::os::fd::{AsFd, AsRawFd};
use std::process::Command;
use std::time::Duration;

use bench_rounds::{counters, interrupt::Flag, process::Runner};
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, ppoll};
use nix::sched::{CpuSet, sched_getaffinity, sched_setaffinity};
use nix::sys::socket::{
    ControlMessageOwned, MsgFlags, MultiHeaders, SockaddrStorage, getsockopt, recvmmsg, recvmsg,
    sendmmsg, setsockopt, sockopt,
};
use nix::sys::time::TimeSpec;
use nix::time::{ClockId, clock_gettime};
use nix::unistd::Pid;
use rustix::net::sockopt::{self as ip_options, Ipv4PathMtuDiscovery};
use socket2::SockRef;

use super::*;

const BATCH: usize = 64;

fn raw() -> Result<u64, String> {
    let t = clock_gettime(ClockId::CLOCK_MONOTONIC_RAW).map_err(|e| e.to_string())?;
    u64::try_from(t.tv_sec())
        .ok()
        .and_then(|s| s.checked_mul(1_000_000_000))
        .and_then(|s| s.checked_add(t.tv_nsec() as u64))
        .ok_or_else(|| "RAW clock outside u64 nanoseconds".into())
}
fn wall() -> Result<i64, String> {
    let t = clock_gettime(ClockId::CLOCK_REALTIME).map_err(|e| e.to_string())?;
    t.tv_sec()
        .checked_mul(1_000_000_000)
        .and_then(|s| s.checked_add(t.tv_nsec()))
        .ok_or_else(|| "wall clock outside i64 nanoseconds".into())
}

pub(super) fn run(w: &Workload, mode: Option<ReceiveMode>, flag: &Flag) -> Result<(), String> {
    // Host commands run before pinning and after restoring the ordinary mask.
    let runner = Runner::new(flag.clone()).with_output_cap(JSON_LIMIT);
    let before = capture(w, "before", &runner);
    let binary = std::env::current_exe().map_err(|e| e.to_string())?;
    let binary_sha256 = hash_file(&binary)?;
    let socket = UdpSocket::bind(w.bind).map_err(|e| e.to_string())?;
    socket.connect(w.peer).map_err(|e| e.to_string())?;
    socket.set_nonblocking(true).map_err(|e| e.to_string())?;
    let sr = SockRef::from(&socket);
    sr.set_send_buffer_size(w.socket_buffer)
        .map_err(|e| e.to_string())?;
    sr.set_recv_buffer_size(w.socket_buffer)
        .map_err(|e| e.to_string())?;
    ip_options::set_ip_mtu_discover(&socket, Ipv4PathMtuDiscovery::DO)
        .map_err(|e| e.to_string())?;
    setsockopt(&socket, sockopt::UdpGsoSegment, &0).map_err(|e| e.to_string())?;
    let gro_error = mode.and_then(|m| {
        setsockopt(&socket, sockopt::UdpGroSegment, &(m == ReceiveMode::Gro))
            .err()
            .map(|e| e.to_string())
    });
    let _ = setsockopt(&socket, sockopt::ReceiveTimestampns, &true);
    let socket_facts = SocketFacts {
        local: socket.local_addr().map_err(|e| e.to_string())?.to_string(),
        peer: socket.peer_addr().map_err(|e| e.to_string())?.to_string(),
        family: "IPv4".into(),
        payload_bytes: PAYLOAD,
        ipv4_bytes: IP_BYTES,
        ip_options: "fresh socket; no IP options installed".into(),
        df: ip_options::ip_mtu_discover(&socket)
            .ok()
            .map(|v| v == Ipv4PathMtuDiscovery::DO),
        path_mtu: ip_options::ip_mtu(&socket).ok(),
        send_buffer: sr.send_buffer_size().ok(),
        receive_buffer: sr.recv_buffer_size().ok(),
        udp_gso: getsockopt(&socket, sockopt::UdpGsoSegment).ok(),
        udp_gro: getsockopt(&socket, sockopt::UdpGroSegment).ok(),
        timestampns: getsockopt(&socket, sockopt::ReceiveTimestampns).ok(),
        gro_error,
    };
    let allowed = sched_getaffinity(Pid::from_raw(0)).map_err(|e| e.to_string())?;
    let placement = pin(w.cpu, &allowed)?;
    let old_slack = nix::sys::prctl::get_timerslack().ok();
    let _ = nix::sys::prctl::set_timerslack(1);
    let start = raw()?;
    let mut result = ProbeResult {
        version: VERSION,
        role: if mode.is_some() { "receiver" } else { "sender" }.into(),
        result_id: format!(
            "{}-{}-{}-{start}",
            w.run_id,
            std::process::id(),
            if mode.is_some() { "r" } else { "s" }
        ),
        workload: w.clone(),
        mode,
        binary_sha256,
        hostname: fs::read_to_string("/proc/sys/kernel/hostname")
            .unwrap_or_default()
            .trim()
            .into(),
        kernel: fs::read_to_string("/proc/sys/kernel/osrelease")
            .unwrap_or_default()
            .trim()
            .into(),
        profiling_enabled: cfg!(feature = "hotpath"),
        socket: socket_facts,
        placement,
        start_raw_ns: start,
        end_raw_ns: start,
        elapsed_ns: 0,
        offered_pps: 0.0,
        accepted_pps: 0.0,
        unique_pps: 0.0,
        batch_datagrams: if w.pps == 1_000_000 { 100 } else { 1 },
        batch_period_ns: if w.pps == 1_000_000 {
            100_000
        } else {
            1_000_000_000 / w.pps
        },
        complete: false,
        errors: Vec::new(),
        sender: None,
        receiver: None,
        timestamps: TimestampEvidence {
            source: "SO_TIMESTAMPNS CLOCK_REALTIME, mapped by bracketing CLOCK_MONOTONIC_RAW pairs"
                .into(),
            sender_granularity:
                "one pre-syscall software stamp shared by each sendmmsg call (at most 64 datagrams)"
                    .into(),
            receiver_granularity: "one kernel stamp per receive message; GRO segments share it"
                .into(),
            ..TimestampEvidence::default()
        },
        counters_before: before.ok(),
        counters_after: None,
    };
    let operation = if let Some(mode) = mode {
        if mode == ReceiveMode::Gro
            && (result.socket.gro_error.is_some() || result.socket.udp_gro != Some(true))
        {
            Err("UDP GRO unsupported or its enabled readback is unavailable".into())
        } else {
            receive(&socket, w, mode, flag, &mut result)
        }
    } else {
        send(&socket, w, flag, &mut result)
    };
    if let Err(error) = operation {
        result.errors.push(error);
    }
    // Sending excludes startup delay. Receiving retains its entire admitted window.
    if mode.is_some() {
        result.end_raw_ns = raw()?;
        result.elapsed_ns = result.end_raw_ns.saturating_sub(result.start_raw_ns);
    }
    result.placement.affinity_after = affinity(Pid::from_raw(0)).ok();
    result.placement.helper_cpus_after = helper_cpus().ok();
    let _ = sched_setaffinity(Pid::from_raw(0), &allowed);
    if let Some(slack) = old_slack {
        let _ = nix::sys::prctl::set_timerslack(slack as _);
    }
    // Read the socket's complete tail count while it still exists.
    if let Some(received) = &mut result.receiver {
        received.socket_drops = socket_drops(&socket);
        received.socket_drop_source = if received.socket_drops.is_some() {
            "/proc/self/net/udp sk_drops; kernel message units"
        } else {
            "unavailable"
        }
        .into();
    }
    drop(socket);
    result.counters_after = capture(w, "after", &runner).ok();
    if result.counters_before.is_none() || result.counters_after.is_none() {
        result
            .errors
            .push("one or both actual-peer counter captures unavailable".into());
    }
    let elapsed = result.elapsed_ns.max(1) as f64 / 1e9;
    if let Some(s) = &result.sender {
        result.offered_pps = s.offered as f64 / elapsed;
        result.accepted_pps = s.accepted as f64 / elapsed;
    }
    if let Some(r) = &result.receiver {
        result.unique_pps = r.sequence.unique as f64 / elapsed;
    }
    result.complete = result.complete && result.errors.is_empty();
    write_json(&w.out, &result)
}

fn capture(w: &Workload, when: &str, runner: &Runner) -> Result<PathBuf, String> {
    let path = w.out.with_extension(format!("{when}.txt"));
    let script = counters::towards(&w.peer.ip().to_string());
    let done = runner
        .run(
            Command::new("bash").arg("-s"),
            Some(script.as_bytes()),
            Duration::from_secs(30),
        )
        .map_err(|e| format!("{e:?}"))?;
    let mut bytes = done.stdout;
    bytes.extend_from_slice(&done.stderr);
    bytes.extend_from_slice(
        format!("\nreceive_probe_capture_exit: {}\n", done.returncode).as_bytes(),
    );
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    use std::io::Write;
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    if done.returncode == 0 {
        Ok(path)
    } else {
        Err("counter snapshot failed".into())
    }
}

fn affinity(pid: Pid) -> Result<Vec<usize>, String> {
    let mask = sched_getaffinity(pid).map_err(|e| e.to_string())?;
    Ok((0..CpuSet::count())
        .filter(|&c| mask.is_set(c).unwrap_or(false))
        .collect())
}
fn cpu_numbers(text: &str) -> Option<Vec<usize>> {
    let mut values = Vec::new();
    for part in text.trim().split(',') {
        let (a, b) = part.split_once('-').unwrap_or((part, part));
        let (a, b) = (a.parse::<usize>().ok()?, b.parse::<usize>().ok()?);
        if b < a || b >= CpuSet::count() {
            return None;
        }
        values.extend(a..=b);
    }
    values.sort_unstable();
    values.dedup();
    Some(values)
}
fn topology() -> Vec<CpuIdentity> {
    let online = fs::read_to_string("/sys/devices/system/cpu/online")
        .ok()
        .and_then(|t| cpu_numbers(&t))
        .unwrap_or_default();
    let present = fs::read_to_string("/sys/devices/system/cpu/present")
        .ok()
        .and_then(|t| cpu_numbers(&t))
        .unwrap_or_else(|| online.clone());
    let mut rows = Vec::new();
    for cpu in &present {
        let base = format!("/sys/devices/system/cpu/cpu{cpu}/topology");
        let read = |file| fs::read_to_string(format!("{base}/{file}")).ok();
        if let (Some(package), Some(core), Some(siblings)) = (
            read("physical_package_id").and_then(|s| s.trim().parse().ok()),
            read("core_id").and_then(|s| s.trim().parse().ok()),
            read("thread_siblings_list").and_then(|s| cpu_numbers(&s)),
        ) {
            rows.push(CpuIdentity {
                cpu: *cpu,
                package,
                core,
                siblings,
                online: online.contains(cpu),
            });
        }
    }
    rows
}
fn helper_cpus() -> Result<Vec<usize>, String> {
    let mut all = Vec::new();
    for entry in fs::read_dir("/proc/self/task").map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() as i32 {
            continue;
        }
        all.extend(affinity(Pid::from_raw(pid))?);
    }
    all.sort_unstable();
    all.dedup();
    Ok(all)
}
fn pin(cpu: usize, allowed: &CpuSet) -> Result<Placement, String> {
    if !allowed.is_set(cpu).unwrap_or(false) {
        return Err("requested CPU is outside application affinity".into());
    }
    let rows = topology();
    let app = rows.iter().find(|r| r.cpu == cpu);
    let outside = (0..CpuSet::count()).find(|&c| {
        allowed.is_set(c).unwrap_or(false) && app.is_none_or(|r| !r.siblings.contains(&c))
    });
    let tasks = fs::read_dir("/proc/self/task").map_err(|e| e.to_string())?;
    for entry in tasks {
        let entry = entry.map_err(|e| e.to_string())?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() as i32 {
            continue;
        }
        if let Some(other) = outside {
            let mut mask = CpuSet::new();
            mask.set(other).map_err(|e| e.to_string())?;
            sched_setaffinity(Pid::from_raw(pid), &mask).map_err(|e| e.to_string())?;
        }
    }
    let mut mask = CpuSet::new();
    mask.set(cpu).map_err(|e| e.to_string())?;
    sched_setaffinity(Pid::from_raw(0), &mask).map_err(|e| e.to_string())?;
    Ok(Placement {
        requested_cpu: cpu,
        allowed_before: (0..CpuSet::count())
            .filter(|&c| allowed.is_set(c).unwrap_or(false))
            .collect(),
        affinity_before: affinity(Pid::from_raw(0)).ok(),
        affinity_after: None,
        topology: rows,
        application_threads: 1,
        helper_cpus_before: helper_cpus().ok(),
        helper_cpus_after: None,
    })
}

fn wait_until(deadline: u64, flag: &Flag) -> Result<(), String> {
    loop {
        if flag.stops() {
            return Err("interrupted".into());
        }
        let now = raw()?;
        if now >= deadline {
            return Ok(());
        }
        // Relative waits are checked against RAW again on wake. No spin tail.
        let ns = (deadline - now).min(50_000_000);
        ppoll(
            &mut [],
            Some(TimeSpec::from_duration(Duration::from_nanos(ns))),
            None,
        )
        .or_else(|e| if e == Errno::EINTR { Ok(0) } else { Err(e) })
        .map_err(|e| e.to_string())?;
    }
}

#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn send(
    socket: &UdpSocket,
    w: &Workload,
    flag: &Flag,
    result: &mut ProbeResult,
) -> Result<(), String> {
    let mut buffers = vec![[0_u8; PAYLOAD]; BATCH];
    let mut headers = MultiHeaders::<()>::preallocate(BATCH, None);
    let addresses = [None::<()>; BATCH];
    let mut counts = SendCounts {
        planned: w.count,
        accepted_ranges: Ranges {
            values: Vec::with_capacity(4096),
            truncated: false,
        },
        offered_per_second: vec![0; 181],
        accepted_per_second: vec![0; 181],
        ..SendCounts::default()
    };
    let mut lateness = Histogram::default();
    let start = raw()? + w.start_delay_ms * 1_000_000;
    let hard_end = result.start_raw_ns + w.deadline_ms * 1_000_000;
    let burst = result.batch_datagrams;
    let period = result.batch_period_ns;
    let mut sequence = 0;
    let outcome = (|| {
        while sequence < w.count {
            let (_, _, offset) =
                scheduled_slot(w.count, w.pps, sequence / burst).expect("validated sequence slot");
            let due = start + offset;
            if due >= hard_end {
                counts.omitted_slots += (w.count - sequence).div_ceil(burst);
                break;
            }
            wait_until(due, flag)?;
            let now = raw()?;
            if now >= hard_end {
                counts.omitted_slots += (w.count - sequence).div_ceil(burst);
                break;
            }
            lateness.record(i64::try_from(now.saturating_sub(due)).unwrap_or(i64::MAX));
            if now >= due + period {
                counts.deadline_misses += 1;
            }
            let end = (sequence + burst).min(w.count);
            while sequence < end {
                let n = (end - sequence).min(BATCH as u64) as usize;
                let stamp = raw()?;
                for (i, buffer) in buffers[..n].iter_mut().enumerate() {
                    buffer[..HEADER].copy_from_slice(&packet_header(
                        w.run_id,
                        sequence + i as u64,
                        stamp,
                    ));
                }
                let slices: [[IoSlice<'_>; 1]; BATCH] =
                    std::array::from_fn(|i| [IoSlice::new(&buffers[i])]);
                let second = ((stamp.saturating_sub(start)) / 1_000_000_000).min(180) as usize;
                match sendmmsg(
                    socket.as_raw_fd(),
                    &mut headers,
                    &slices[..n],
                    &addresses[..n],
                    [],
                    MsgFlags::MSG_DONTWAIT,
                ) {
                    Ok(sent) => {
                        let sent = sent.count();
                        counts.record_attempt(sequence, n as u64, second, Some(sent as u64));
                    }
                    Err(e) => {
                        counts.record_attempt(sequence, n as u64, second, None);
                        if !matches!(e, Errno::EAGAIN | Errno::ENOBUFS | Errno::EINTR) {
                            return Err(format!("sendmmsg: {e}"));
                        }
                    }
                }
                // Every offered slot is final, including a partial or refused call.
                sequence += n as u64;
            }
        }
        Ok(())
    })();
    result.start_raw_ns = start;
    result.end_raw_ns = raw()?;
    result.elapsed_ns = result.end_raw_ns.saturating_sub(start);
    counts.unsent = w.count - counts.accepted;
    counts.schedule_lateness = lateness.finish(false);
    trim_seconds(&mut counts.offered_per_second);
    trim_seconds(&mut counts.accepted_per_second);
    result.sender = Some(counts);
    result.complete = outcome.is_ok() && sequence == w.count;
    outcome
}

fn trim_seconds(counts: &mut Vec<u64>) {
    while counts.len() > 1 && counts.last() == Some(&0) {
        counts.pop();
    }
}

fn socket_drops(socket: &UdpSocket) -> Option<u64> {
    let inode = fs::read_link(format!("/proc/self/fd/{}", socket.as_raw_fd())).ok()?;
    let inode = inode
        .to_str()?
        .strip_prefix("socket:[")?
        .strip_suffix(']')?;
    let table = fs::read_to_string("/proc/self/net/udp").ok()?;
    table.lines().skip(1).find_map(|line| {
        let mut columns = line.split_whitespace();
        let candidate = columns.nth(9)?;
        (candidate == inode)
            .then(|| columns.nth(2)?.parse().ok())
            .flatten()
    })
}

fn clock_pair() -> Result<ClockPair, String> {
    let raw_before_ns = raw()?;
    let wall_ns = wall()?;
    let raw_after_ns = raw()?;
    Ok(ClockPair {
        raw_before_ns,
        wall_ns,
        raw_after_ns,
    })
}
fn metadata(message: &nix::sys::socket::RecvMsg<'_, '_, SockaddrStorage>) -> ReceiveMessage {
    let mut found = ReceiveMessage {
        length: message.bytes,
        peer: message
            .address
            .as_ref()
            .and_then(|s| s.as_sockaddr_in())
            .map(|a| SocketAddrV4::new(a.ip(), a.port())),
        data_truncated: message.flags.contains(MsgFlags::MSG_TRUNC),
        control_truncated: message.flags.contains(MsgFlags::MSG_CTRUNC),
        ..ReceiveMessage::default()
    };
    match message.cmsgs() {
        Ok(cmsgs) => {
            for c in cmsgs {
                match c {
                    ControlMessageOwned::ScmTimestampns(t) => {
                        found.wall_ns = t
                            .tv_sec()
                            .checked_mul(1_000_000_000)
                            .and_then(|s| s.checked_add(t.tv_nsec()));
                    }
                    ControlMessageOwned::UdpGroSegments(n) => {
                        found.gro = Some(n);
                    }
                    _ => {}
                }
            }
        }
        Err(_) => found.control_truncated = true,
    }
    found
}

#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn receive(
    socket: &UdpSocket,
    w: &Workload,
    mode: ReceiveMode,
    flag: &Flag,
    result: &mut ProbeResult,
) -> Result<(), String> {
    let mut ingest = MessageIngest::new(w.count, w.run_id, w.peer, mode);
    let mut headers =
        MultiHeaders::<SockaddrStorage>::preallocate(BATCH, Some(nix::cmsg_space!(TimeSpec)));
    let mut small = [[0_u8; HEADER]; BATCH];
    let mut aggregate = vec![0_u8; MAX_AGGREGATE];
    let mut cmsg = nix::cmsg_space!(TimeSpec, i32);
    let hard_end = result.start_raw_ns + w.deadline_ms * 1_000_000;
    let mut last = None;
    let mut saw_all_at = None;
    ingest.clocks.observe(clock_pair()?);
    write_json(
        &w.out.with_extension("ready.json"),
        &Readiness {
            version: VERSION,
            pid: std::process::id(),
            result_id: result.result_id.clone(),
            socket: result.socket.clone(),
        },
    )?;
    let outcome = (|| {
        loop {
            let now = raw()?;
            if flag.stops() {
                return Err("interrupted".into());
            }
            if now >= hard_end {
                break;
            }
            if last.is_some_and(|t| now >= t + w.drain_ms * 1_000_000)
                || saw_all_at.is_some_and(|t| now >= t + w.drain_ms * 1_000_000)
            {
                break;
            }
            let mut messages = [ReceiveMessage::default(); BATCH];
            let before = clock_pair()?;
            ingest.clocks.observe(before);
            let read = if mode == ReceiveMode::HeaderOnly {
                let mut buffers = small.iter_mut();
                let mut slices: [[IoSliceMut<'_>; 1]; BATCH] =
                    std::array::from_fn(|_| [IoSliceMut::new(buffers.next().expect("64 slots"))]);
                recvmmsg(
                    socket.as_raw_fd(),
                    &mut headers,
                    slices.iter_mut(),
                    MsgFlags::MSG_DONTWAIT | MsgFlags::MSG_TRUNC,
                    None,
                )
                .map(|got| {
                    let mut n = 0;
                    for m in got {
                        messages[n] = metadata(&m);
                        n += 1;
                    }
                    n
                })
            } else {
                recvmsg::<SockaddrStorage>(
                    socket.as_raw_fd(),
                    &mut [IoSliceMut::new(&mut aggregate)],
                    Some(&mut cmsg),
                    MsgFlags::MSG_DONTWAIT,
                )
                .map(|m| {
                    messages[0] = metadata(&m);
                    1
                })
            };
            let read_ns = raw()?;
            let after = clock_pair()?;
            ingest.clocks.observe(after);
            match read {
                Ok(n) => {
                    last = Some(read_ns);
                    let second =
                        ((read_ns - result.start_raw_ns) / 1_000_000_000).min(180) as usize;
                    for (i, message) in messages[..n].iter().enumerate() {
                        let copied = if mode == ReceiveMode::HeaderOnly {
                            &small[i][..]
                        } else {
                            &aggregate[..message.length.min(MAX_AGGREGATE)]
                        };
                        ingest.ingest(*message, copied, before, after, read_ns, second);
                    }
                    if ingest.unique() == w.count && saw_all_at.is_none() {
                        saw_all_at = Some(read_ns);
                    }
                    if messages[..n].iter().any(|m| m.control_truncated) {
                        // nix retains shrunken control capacity across recvmmsg
                        // calls. Restore it, retaining the truncated observation.
                        headers =
                            MultiHeaders::preallocate(BATCH, Some(nix::cmsg_space!(TimeSpec)));
                    }
                }
                Err(Errno::EAGAIN) => {
                    let now = raw()?;
                    let drain_end = last
                        .map_or(hard_end, |t| t + w.drain_ms * 1_000_000)
                        .min(hard_end);
                    let wait = drain_end.saturating_sub(now).min(50_000_000);
                    let mut poll = [PollFd::new(socket.as_fd(), PollFlags::POLLIN)];
                    ppoll(
                        &mut poll,
                        Some(TimeSpec::from_duration(Duration::from_nanos(wait))),
                        None,
                    )
                    .or_else(|e| if e == Errno::EINTR { Ok(0) } else { Err(e) })
                    .map_err(|e| e.to_string())?;
                }
                Err(Errno::EINTR) => {}
                Err(e) => return Err(format!("receive: {e}")),
            }
        }
        Ok(())
    })();
    if let Ok(pair) = clock_pair() {
        ingest.clocks.observe(pair);
    }
    let (counts, timestamps) = ingest.finish();
    result.receiver = Some(counts);
    result.timestamps = timestamps;
    result.complete = outcome.is_ok();
    outcome
}
