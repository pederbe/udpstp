//! Reading a Round Run's evidence is separate from judging it. All file
//! access ends at `RunFiles::load`; `analyse` also accepts an in-memory run.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use bench_rounds::{counters, samples, utc};

use super::*;

/// Only the evidence the reference reads, relative to one Round Run.
/// Missing/unreadable files are absent; malformed contents remain visible.
#[derive(Clone, Debug, Default)]
pub struct RunFiles {
    pub files: BTreeMap<String, Vec<u8>>,
}

impl RunFiles {
    pub fn load(directory: &Path) -> Result<Self, Error> {
        let mut run = Self::default();
        let mut read = |path: &str| {
            if let Ok(bytes) = fs::read(directory.join(path)) {
                run.files.insert(path.to_owned(), bytes);
            }
        };
        read("meta.json");
        if let Ok(entries) = fs::read_dir(directory) {
            for entry in entries.flatten() {
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| Error("a Round Run's file name is not UTF-8".into()))?;
                if name.starts_with("counters.") && name.ends_with(".txt") {
                    read(&name);
                }
                if name == "server" || name.starts_with("client") {
                    for file in [
                        "p.json",
                        "q.json",
                        "p.log",
                        "server.log",
                        "samples.jsonl",
                        "host-samples.jsonl",
                    ] {
                        read(&format!("{name}/{file}"));
                    }
                    if let Ok(clients) = fs::read_dir(directory.join(&name).join("clients")) {
                        for client in clients.flatten() {
                            let client = client
                                .file_name()
                                .into_string()
                                .map_err(|_| Error("a Client result's name is not UTF-8".into()))?;
                            if client.ends_with(".json") {
                                read(&format!("{name}/clients/{client}"));
                            }
                        }
                    }
                }
            }
        }
        Ok(run)
    }

    pub(crate) fn json(&self, path: &str) -> Result<Json, Error> {
        super::super::evidence_json(self.files.get(path).map(Vec::as_slice), path)
    }

    pub(crate) fn text(&self, path: &str) -> Result<Option<&str>, Error> {
        self.files
            .get(path)
            .map(|b| std::str::from_utf8(b).map_err(|_| Error(format!("{path} is not UTF-8"))))
            .transpose()
    }

    pub(crate) fn lines(&self, path: &str) -> Result<Vec<Json>, Error> {
        let mut lines = Vec::new();
        for line in py::file_lines(self.text(path)?.unwrap_or("")) {
            match Json::parse(line) {
                Ok(value) => lines.push(value),
                Err(_) if py::python_also_refuses(line) => {}
                Err(error) => return Err(Error(format!("{path}: {error}"))),
            }
        }
        Ok(lines)
    }

    fn drops(&self, role: &str, names: &[&str]) -> Result<Json, Error> {
        let read = |when| {
            self.text(&format!("counters.{role}.{when}.txt"))?
                .map(counters::parse)
                .transpose()
                .map_err(|e| Error(e.to_string()))
        };
        let before = read("before")?;
        let after = read("after")?;
        Ok(counter_json(&counters::rise(
            before.as_ref(),
            after.as_ref(),
            names,
        )))
    }

    fn missing_counters(&self, required: &Json) -> Result<Vec<String>, Error> {
        let mut problems = Vec::new();
        for role in ["server", "client1", "client2"] {
            let fallback = Json::List(HOST_DROPS.iter().map(|s| string(s)).collect());
            let names = list(required.get_or(role, &fallback)?)?
                .iter()
                .map(text)
                .collect::<Result<Vec<_>, _>>()?;
            for when in ["before", "after"] {
                let source = self
                    .text(&format!("counters.{role}.{when}.txt"))?
                    .unwrap_or("");
                let gone = counters::missing(source, &names).map_err(|e| Error(e.to_string()))?;
                if !gone.is_empty() {
                    problems.push(format!(
                        "{role}'s counters {when} the run lack {}",
                        gone.join(", ")
                    ));
                }
            }
        }
        Ok(problems)
    }

    fn missing_results(&self, names: &[(&str, &str)]) -> Result<Option<String>, Error> {
        let mut gone = Vec::new();
        for (name, path) in names {
            if !self.json(path)?.truthy() {
                gone.push(*name);
            }
        }
        if gone.is_empty() {
            return Ok(None);
        }
        let tail = self.text("server/p.log")?.unwrap_or("");
        let tail = py::file_lines(tail)
            .into_iter()
            .map(py::strip)
            .rfind(|s| !s.is_empty());
        let tail = tail
            .map(|s| {
                format!(
                    " (side P's log: {})",
                    s.chars().take(200).collect::<String>()
                )
            })
            .unwrap_or_default();
        Ok(Some(format!(
            "the tooling failed: {} missing or unreadable{tail}",
            gone.join(", ")
        )))
    }

    fn clients(
        &self,
        server: bool,
        count: i64,
    ) -> Result<(BTreeMap<i64, Json>, Option<String>), Error> {
        if !(0..=250).contains(&count) {
            return Err(Error(
                "Client count is outside a Server-scale matrix's bounds".into(),
            ));
        }
        let mut found = BTreeMap::new();
        for path in self.files.keys() {
            let parts: Vec<_> = path.split('/').collect();
            if parts.len() != 3
                || parts[1] != "clients"
                || !(if server {
                    parts[0] == "server"
                } else {
                    parts[0].starts_with("client")
                })
            {
                continue;
            }
            let Some(name) = parts[2].strip_suffix(".json") else {
                continue;
            };
            if !name.is_ascii() {
                return Err(Error("a Client result's number is not ASCII".into()));
            }
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let number = name
                .parse::<i64>()
                .map_err(|_| Error("a Client's number is out of range".into()))?;
            if found.insert(number, self.json(path)?).is_some() {
                return Err(Error(format!(
                    "Client {number} has more than one result file"
                )));
            }
        }
        let gone = (1..=count)
            .filter(|n| !found.get(n).is_some_and(Json::truthy))
            .collect::<Vec<_>>();
        let problem = if gone.is_empty() {
            None
        } else {
            let mut shown = gone
                .iter()
                .take(5)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            if gone.len() > 5 {
                shown += " …";
            }
            Some(format!(
                "the tooling failed: {} of {count} Clients' results missing or unreadable (Client {shown})",
                gone.len()
            ))
        };
        found.retain(|_, result| result.truthy());
        Ok((found, problem))
    }
}

fn counter_json(counters: &counters::Counters) -> Json {
    Json::Dict(
        counters
            .iter()
            .map(|(k, n)| (k.clone(), Json::Int(*n)))
            .collect(),
    )
}

const RECORD_KEYS: [&str; 18] = [
    "label",
    "kind",
    "case",
    "direction",
    "row",
    "mbps",
    "count",
    "engine",
    "repeat",
    "rerun",
    "more",
    "test_interval",
    "seconds",
    "confine",
    "server_cpus",
    "client_env",
    "server_env",
    "executor",
];
fn record_from(meta: &Json) -> Result<Json, Error> {
    Ok(Json::Dict(
        RECORD_KEYS
            .iter()
            .map(|key| Ok(((*key).into(), meta.get(key)?.clone())))
            .collect::<Result<_, Error>>()?,
    ))
}
pub fn tooling_record(meta: &Json, reason: &str) -> Result<Json, Error> {
    let mut record = record_from(meta)?;
    extend(
        &mut record,
        dict([
            ("platform", EMPTY_DICT.clone()),
            ("flags", EMPTY_LIST.clone()),
            (
                "invalid",
                strings([format!("the tooling failed: {reason}")]),
            ),
            ("valid", Json::Bool(false)),
            ("meets", Json::Bool(false)),
            ("completed", Json::Int(0)),
        ]),
    )?;
    Ok(record)
}

fn client_figures(files: &RunFiles, meta: &Json, leave_out: bool) -> Result<Json, Error> {
    let udpstp = meta.item("engine")?.as_str() == Some("udpstp");
    let downstream = meta.item("direction")?.as_str() == Some("downstream");
    let mbps = number(meta.item("mbps")?)?.to_f64();
    let expected = integer(meta.item("test_interval")?)?;
    if !(0..=60).contains(&expected) {
        return Err(Error(
            "Test Interval is outside a Server-scale matrix's bounds".into(),
        ));
    }
    let count = integer(meta.item("count")?)?;
    let (results, problem) = files.clients(false, count)?;
    let mut invalid = problem.into_iter().collect::<Vec<_>>();
    let mut tcs = Vec::new();
    let mut bom = Vec::new();
    let mut eom = Vec::new();
    let mut completed = 0;
    let mut loss = Num::Int(0);
    let mut client_not_sent = Num::Int(0);
    let mut overflows = Num::Int(0);
    let mut left_out = 0;
    let mut no_answer = IndexMap::<String, i128>::new();
    let mut estimated = 0;
    let mut estimated_sum = 0.0;
    let mut on_sender_clock = false;
    for r in results.values() {
        let exchange = unanswered(r)?;
        if exchange.truthy() {
            *no_answer.entry(text(&exchange)?.into()).or_default() += 1;
            if leave_out {
                left_out += 1;
                continue;
            }
        }
        let out = r.get("Output")?.or(&EMPTY_DICT);
        let ours = r.get("udpstp")?;
        let done = if !ours.is_null() {
            ours.get("EndReason")?.as_str() == Some("Completed")
        } else {
            out.get("Status")?.as_str() == Some("Complete")
        };
        let ours = ours.or(&EMPTY_DICT);
        completed += i128::from(done);
        let mut subs = IndexMap::<i64, &Json>::new();
        for s in array(out, "IncrementalResult")? {
            subs.insert(integer(s.get("Interval")?)?, s);
        }
        loss = loss.py_add(num::sum(
            &subs
                .values()
                .map(|s| number(s.get_or("LossCount", &Json::Int(0))?))
                .collect::<Result<Vec<_>, Error>>()?,
        ));
        let received = (1..=expected)
            .map(|i| {
                subs.get(&i)
                    .map(|s| s.get_or("IPLayerCapacity", &Json::Float(0.0)).cloned())
                    .transpose()
                    .map(|v| v.unwrap_or(Json::Null))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut sent = IndexMap::<i64, &Json>::new();
        for e in array(ours, "IPLayerSenderBitRate")? {
            if e.as_dict()
                .ok_or_else(|| Error("a sender rate is not a dict".into()))?
                .contains_key("IPLayerSenderBitRateEstimate")
            {
                sent.insert(
                    integer(e.get("SubInterval")?)?,
                    e.item("IPLayerSenderBitRateEstimate")?,
                );
            }
        }
        let mut judged = Vec::new();
        for i in 1..=expected {
            if downstream && let Some(rate) = sent.get(&i) {
                let rate_f = number(rate)?.to_f64();
                judged.push(Json::List(vec![(*rate).clone(), Json::Bool(false)]));
                on_sender_clock = true;
                estimated += 1;
                estimated_sum += rate_f;
                if i == 1 {
                    estimated_sum +=
                        num::max2(0.0, mbps - rate_f).min(mbps * START_SHORT_PCT / 100.0);
                }
            } else if !received[(i - 1) as usize].is_null() {
                judged.push(Json::List(vec![
                    received[(i - 1) as usize].clone(),
                    Json::Bool(true),
                ]));
            } else {
                judged.push(Json::Null);
            }
        }
        tcs.push(Json::List(vec![
            Json::List(judged),
            Json::List(received),
            Json::Bool(done),
        ]));
        if out.get("BOMTime")?.truthy() && out.get("EOMTime")?.truthy() {
            let timestamp =
                |key| utc::timestamp(text(out.item(key)?)?).map_err(|e| Error(e.to_string()));
            bom.push(Num::Float(timestamp("BOMTime")?));
            eom.push(Num::Float(timestamp("EOMTime")?));
        }
        for warning in array(ours, "Warnings")? {
            match warning.get("Code")?.as_str() {
                Some("BurstsSkipped" | "BurstsWithheld") => {
                    client_not_sent =
                        client_not_sent.py_add(number(warning.get_or("Count", &Json::Int(0))?)?)
                }
                Some("ReceiveBufferOverflows") => {
                    overflows = overflows.py_add(number(warning.get_or("Count", &Json::Int(0))?)?)
                }
                _ => {}
            }
        }
    }
    let ran = count - left_out;
    let mut figures = dict([
        ("completed", Json::Int(completed)),
        ("loss", num_json(loss)),
        ("unanswered", Json::Int(no_answer.values().sum())),
        (
            "unanswered_by",
            Json::Dict(
                no_answer
                    .into_iter()
                    .map(|(k, v)| (k, Json::Int(v)))
                    .collect(),
            ),
        ),
        ("left_out", Json::Int(left_out.into())),
    ]);
    extend(&mut figures, rates_figures(&tcs, mbps, expected, ran)?)?;
    set(
        &mut figures,
        "rates_from",
        string(if on_sender_clock {
            "the Load Sender's clock"
        } else {
            "the Load Receivers"
        }),
    )?;
    let planned = planned_datagrams(meta, Some(Num::Int(ran.into())))?;
    let sender_short = if estimated == 0 {
        Json::Null
    } else {
        Json::Float(100.0 * num::max2(0.0, 1.0 - estimated_sum / (mbps * f64::from(estimated))))
    };
    set(&mut figures, "sender_short_pct", sender_short.clone())?;
    let mut server_not_sent = Num::Int(0);
    let mut server_overflows = Json::Null;
    if udpstp {
        let mut overflows = Num::Int(0);
        let mut summaries = 0i128;
        let events = files.lines("server/server.log")?;
        for e in &events {
            if e.get("event")?.as_str() == Some("TestSummary") {
                summaries += 1;
                server_not_sent = server_not_sent.py_add(
                    number(e.get_or("skipped_datagrams", &Json::Int(0))?)?
                        .py_add(number(e.get_or("withheld_datagrams", &Json::Int(0))?)?),
                );
                overflows = overflows.py_add(zero(e, "receive_buffer_overflows")?);
            }
        }
        set(&mut figures, "test_summaries", Json::Int(summaries))?;
        set(&mut figures, "server_not_sent", num_json(server_not_sent))?;
        let ends = engine_ends(&events)?;
        for (i, key) in ["executor_ran", SEND_LAG_P99, BURST_LATENESS_P99]
            .iter()
            .enumerate()
        {
            set(&mut figures, key, ends.at(i as i64)?.clone())?;
        }
        set(&mut figures, "engine_startups", engine_startups(&events)?)?;
        server_overflows = num_json(overflows);
        if summaries < completed {
            invalid.push(format!("the tooling failed: the Server logged {summaries} TestSummary events for {completed} completed Test Connections"));
        }
    }
    let not_sent = if !downstream {
        Json::Null
    } else if udpstp {
        if planned == 0.0 {
            Json::Null
        } else {
            Json::Float(100.0 * server_not_sent.to_f64() / planned)
        }
    } else {
        sender_short
    };
    set(&mut figures, "not_sent_pct", not_sent)?;
    set(&mut figures, "server_overflows", server_overflows)?;
    let clients_not_sent = if downstream || planned == 0.0 {
        Json::Null
    } else {
        Json::Float(100.0 * client_not_sent.to_f64() / planned)
    };
    set(
        &mut figures,
        "clients_not_sent_pct",
        clients_not_sent.clone(),
    )?;
    if downstream && !is_zero(Some(overflows)) {
        invalid.push(format!(
            "the Clients' sockets overflowed: {} datagrams",
            display(&num_json(overflows))
        ));
    }
    if gt(numeric(&clients_not_sent)?, CLIENT_NOT_SENT) {
        invalid.push(format!(
            "the Clients didn't send {} % of their schedule",
            num::fixed(number(&clients_not_sent)?.to_f64(), 2)
        ));
    }
    let span = if bom.is_empty() {
        Json::Null
    } else {
        eom.sort_by(|a, b| a.py_cmp(*b).unwrap_or(Ordering::Equal));
        Json::List(vec![
            Json::Float(num::max(&bom).unwrap().to_f64() + 1.0),
            Json::Float(eom[0].to_f64() - 1.0),
        ])
    };
    set(&mut figures, "window", span)?;
    set(&mut figures, "invalid", strings(invalid))?;
    Ok(figures)
}

fn prototype_figures(files: &RunFiles, meta: &Json) -> Result<Json, Error> {
    let p = files.json("server/p.json")?;
    let qs = [files.json("client1/q.json")?, files.json("client2/q.json")?];
    let mbps = number(meta.item("mbps")?)?.to_f64();
    let expected = integer(meta.item("test_interval")?)?;
    if !(0..=60).contains(&expected) {
        return Err(Error(
            "Test Interval is outside a Server-scale matrix's bounds".into(),
        ));
    }
    let mut figures = dict([("completed", Json::Int(0)), ("loss", Json::Null)]);
    extend(&mut figures, rates_figures(&[], mbps, expected, 0)?)?;
    extend(
        &mut figures,
        dict([
            ("not_sent_pct", Json::Null),
            ("clients_not_sent_pct", Json::Null),
            ("server_overflows", Json::Null),
            ("sender_short_pct", Json::Null),
            ("rates_from", string("the Load Receivers")),
            ("invalid", EMPTY_LIST.clone()),
        ]),
    )?;
    let start = meta.get("start_wall")?;
    let warm_up = number(meta.item("warm_up")?)?.to_f64();
    if !(0.0..=10.0).contains(&warm_up) {
        return Err(Error(
            "warm-up is outside a Server-scale matrix's bounds".into(),
        ));
    }
    let span = if start.truthy() {
        let first = number(start)?.to_f64() / 1e9 + warm_up;
        Json::List(vec![
            Json::Float(first),
            Json::Float(first + expected as f64),
        ])
    } else {
        Json::Null
    };
    set(&mut figures, "window", span)?;
    if let Some(gone) = files.missing_results(&[
        ("side P's results", "server/p.json"),
        ("client1's side Q results", "client1/q.json"),
        ("client2's side Q results", "client2/q.json"),
    ])? {
        set(&mut figures, "invalid", strings([gone]))?;
        return Ok(figures);
    }
    let mut q_conns = IndexMap::new();
    let mut p_conns = IndexMap::new();
    for q in &qs {
        for c in list(q.item("conns")?)? {
            q_conns.insert(integer(c.item("idx")?)?, c);
        }
    }
    for c in list(p.item("conns")?)? {
        p_conns.insert(integer(c.item("idx")?)?, c);
    }
    let mut fine = Vec::new();
    for (i, c) in &p_conns {
        if !c.item("panicked")?.truthy()
            && let Some(q) = q_conns.get(i)
            && !q.item("panicked")?.truthy()
        {
            fine.push(*i);
        }
    }
    let downstream = meta.item("direction")?.as_str() == Some("downstream");
    let (sender_conns, receiver_conns) = if downstream {
        (&p_conns, &q_conns)
    } else {
        (&q_conns, &p_conns)
    };
    let mut not_sent = Vec::new();
    let mut sent = Vec::new();
    for i in &fine {
        let sender = sender_conns[i].item("sender")?;
        if sender.truthy() {
            let stats = sender.item("stats")?;
            not_sent.push(
                number(stats.item("skipped_datagrams")?)?
                    .py_add(number(stats.item("withheld_datagrams")?)?),
            );
            sent.push(number(stats.item("sent_datagrams")?)?);
        }
    }
    let not_sent = num::sum(&not_sent);
    let sent = num::sum(&sent);
    let total = sent.py_add(not_sent);
    let share = if is_zero(Some(total)) {
        Json::Null
    } else {
        Json::Float(100.0 * not_sent.to_f64() / total.to_f64())
    };
    let base = Num::Float(warm_up / BIN_S)
        .round(None)
        .ok_or_else(|| Error("warm-up is out of range".into()))?
        .to_f64() as usize;
    let per_si = Num::Float(1.0 / BIN_S).round(None).unwrap().to_f64() as usize;
    let mut tcs = Vec::new();
    let mut drops = Vec::new();
    let mut losses = Vec::new();
    for i in &fine {
        let receiver = receiver_conns[i].item("receiver")?;
        let bins = list(receiver.item("bins")?)?;
        let mut received = Vec::new();
        for k in 0..expected as usize {
            let first = k
                .checked_mul(per_si)
                .and_then(|offset| base.checked_add(offset))
                .ok_or_else(|| Error("prototype sample range is out of bounds".into()))?;
            let last = first
                .checked_add(per_si)
                .ok_or_else(|| Error("prototype sample range is out of bounds".into()))?;
            received.push(if last > bins.len() {
                Json::Null
            } else {
                Json::Float(
                    num::sum(
                        &bins[first..last]
                            .iter()
                            .map(number)
                            .collect::<Result<Vec<_>, _>>()?,
                    )
                    .to_f64()
                        * 8.0
                        / 1e6,
                )
            });
        }
        let judged = received
            .iter()
            .map(|r| {
                if r.is_null() {
                    Json::Null
                } else {
                    Json::List(vec![r.clone(), Json::Bool(false)])
                }
            })
            .collect();
        tcs.push(Json::List(vec![
            Json::List(judged),
            Json::List(received),
            Json::Bool(true),
        ]));
        drops.push(number(receiver.item("udp_drops")?)?);
        losses.push(number(receiver.item("stats")?.item("lost")?)?);
    }
    extend(
        &mut figures,
        rates_figures(&tcs, mbps, expected, integer(meta.item("count")?)?)?,
    )?;
    let drops = num::sum(&drops);
    extend(
        &mut figures,
        dict([
            ("completed", Json::Int(fine.len() as i128)),
            ("loss", num_json(num::sum(&losses))),
            (
                "not_sent_pct",
                if downstream {
                    share.clone()
                } else {
                    Json::Null
                },
            ),
            (
                "clients_not_sent_pct",
                if downstream {
                    Json::Null
                } else {
                    share.clone()
                },
            ),
            (
                "server_overflows",
                if downstream {
                    Json::Null
                } else {
                    num_json(drops)
                },
            ),
            ("commit", p.get("commit")?.clone()),
        ]),
    )?;
    let mut invalid = Vec::new();
    if downstream && !is_zero(Some(drops)) {
        invalid.push(format!(
            "the Clients' sockets overflowed: {} datagrams",
            display(&num_json(drops))
        ));
    }
    if !downstream && gt(numeric(&share)?, CLIENT_NOT_SENT) {
        invalid.push(format!(
            "the Clients didn't send {} % of their schedule",
            num::fixed(number(&share)?.to_f64(), 2)
        ));
    }
    set(&mut figures, "invalid", strings(invalid))?;
    Ok(figures)
}

fn p1_figures(files: &RunFiles, meta: &Json) -> Result<Json, Error> {
    if meta.item("engine")?.as_str() == Some("udpstp") {
        let (results, problem) = files.clients(true, integer(meta.item("count")?)?)?;
        let mut lags = Vec::new();
        let mut completed = 0;
        for r in results.values() {
            let ours = r.get("udpstp")?.or(&EMPTY_DICT);
            completed += i128::from(ours.get("EndReason")?.as_str() == Some("Completed"));
            let stamps = ours.get("KernelSendTimestamps")?;
            if stamps.truthy() && stamps.get("Samples")?.truthy() {
                lags.push(stamps);
            }
        }
        for lag in &lags {
            number(lag.item("LagP99Us")?)?;
            number(lag.item("LagMaxUs")?)?;
        }
        return Ok(dict([
            ("completed", Json::Int(completed)),
            ("invalid", strings(problem)),
            ("window", Json::Null),
            ("p1_p99_us", med(&lags, "LagP99Us")?),
            ("p1_p999_us", Json::Null),
            ("p1_worst_us", maximum(&lags, "LagP99Us")?),
            ("p1_max_us", maximum(&lags, "LagMaxUs")?),
        ]));
    }
    let p = files.json("server/p.json")?;
    if !p.truthy() {
        return Ok(dict([
            ("completed", Json::Int(0)),
            ("window", Json::Null),
            ("p1_p99_us", Json::Null),
            ("p1_p999_us", Json::Null),
            ("p1_worst_us", Json::Null),
            ("p1_max_us", Json::Null),
            (
                "invalid",
                strings(files.missing_results(&[("side P's results", "server/p.json")])?),
            ),
        ]));
    }
    let pooled = p.item("pooled")?;
    let mut completed = 0;
    for c in list(p.item("conns")?)? {
        completed += i128::from(!c.item("panicked")?.truthy());
    }
    Ok(dict([
        ("completed", Json::Int(completed)),
        ("invalid", EMPTY_LIST.clone()),
        ("window", Json::Null),
        ("p1_p99_us", pooled.item("i")?.item("p99_us")?.clone()),
        ("p1_p999_us", pooled.item("i")?.item("p999_us")?.clone()),
        ("p1_worst_us", pooled.item("worst_conn_i_p99")?.clone()),
        ("p1_max_us", pooled.item("i")?.item("max_us")?.clone()),
        ("commit", p.get("commit")?.clone()),
    ]))
}

/// Turn files into the reference's ordered record. Malformed shapes fail
/// the analysis instead of silently replacing figures with zero.
pub fn analyse(files: &RunFiles) -> Result<Json, Error> {
    let meta = files.json("meta.json")?;
    let meta = meta.or(&EMPTY_DICT);
    let mut record = record_from(meta)?;
    let mut invalid = Vec::new();
    let mut conditions = Vec::new();
    if meta.get("error")?.truthy() {
        invalid.push(format!(
            "the tooling failed: {}",
            display(meta.item("error")?)
        ));
    }
    if meta.truthy() {
        let missing = files.missing_counters(meta.get("counters")?.or(&EMPTY_DICT))?;
        if !missing.is_empty() {
            invalid.push(format!("the tooling failed: {}", missing.join("; ")));
        }
    }
    let sends = meta.get("kind")?.as_str() == Some("p1")
        || meta.get("direction")?.as_str() == Some("downstream");
    let mut counter_files = BTreeMap::new();
    for name in files
        .files
        .keys()
        .filter(|name| name.starts_with("counters.") && name.ends_with(".txt"))
    {
        counter_files.insert(name.clone(), files.text(name)?.map(str::to_owned));
    }
    let limits =
        counters::platform_limits("counters", &counter_files).map_err(|e| Error(e.to_string()))?;
    set(
        &mut record,
        "platform",
        Json::Dict(
            limits
                .iter()
                .map(|(role, counters)| (role.clone(), counter_json(counters)))
                .collect(),
        ),
    )?;
    if !limits.is_empty() {
        (if sends { &mut conditions } else { &mut invalid }).push(format!(
            "the platform limited it: {}",
            counters::limits_text(&limits)
        ));
    }
    let drops = files.drops(
        "server",
        &[HOST_DROPS.as_slice(), &["SndbufErrors"]].concat(),
    )?;
    set(&mut record, "server_drops", drops.clone())?;
    let nic = meta
        .get("server_nic_drops")?
        .or(&string(DEFAULT_NIC_DROPS))
        .clone();
    set(&mut record, "server_nic_drops", nic.clone())?;
    let receive_drops = server_receive_drops(&drops, text(&nic)?)?;
    set(&mut record, "server_receive_drops", num_json(receive_drops))?;
    let silent = if meta.get("engine")?.as_str() == Some("udpstp")
        && meta.get("kind")?.as_str() != Some("p1")
    {
        num_json(silent_rejections(&files.lines("server/server.log")?)?)
    } else {
        Json::Null
    };
    set(&mut record, "silent_rejections", silent.clone())?;
    let mut figures = if meta.get("kind")?.as_str() == Some("p1") {
        p1_figures(files, meta)?
    } else if matches!(meta.get("engine")?.as_str(), Some("udpstp" | "obudpst")) {
        client_figures(
            files,
            meta,
            is_zero(Some(receive_drops)) && !silent.truthy(),
        )?
    } else if meta.truthy() {
        prototype_figures(files, meta)?
    } else {
        dict([
            ("completed", Json::Int(0)),
            (
                "invalid",
                strings(["the tooling failed: the run's meta.json is missing".into()]),
            ),
            ("window", Json::Null),
        ])
    };
    invalid.extend(
        array(&figures, "invalid")?
            .iter()
            .map(text)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(str::to_owned),
    );
    let span_json = figures.item("window")?.clone();
    let span = if span_json.truthy() {
        Some((
            number(span_json.at(0)?)?.to_f64(),
            number(span_json.at(1)?)?.to_f64(),
        ))
    } else {
        None
    };
    if let Json::Dict(figures) = &mut figures {
        figures.shift_remove("invalid");
        figures.shift_remove("window");
    }
    extend(&mut record, figures)?;
    invalid.extend(engine_problems(meta, &record)?);
    set(&mut record, "window", span_json)?;
    let left_out = zero(&record, "left_out")?;
    if left_out.to_f64() > PATH_LEFT_OUT_SHARE * zero(meta, "count")?.to_f64() {
        invalid.push(format!("{PATH_LEFT_OUT} {} of {} Test Connections, more than the 5 % a run may: their Control Phase got no answer", display(&num_json(left_out)), display(meta.item("count")?)));
    }
    let mut client_hosts = IndexMap::new();
    let mut room = Vec::new();
    for role in CLIENT_ROLES {
        let discarded = files.drops(role, &HOST_DROPS)?;
        if discarded.truthy() && meta.get("kind")?.as_str() != Some("p1") {
            let drops = discarded
                .as_dict()
                .unwrap()
                .iter()
                .map(|(key, value)| format!("{key} +{}", display(value)))
                .collect::<Vec<_>>()
                .join(", ");
            invalid.push(format!("{role}'s host discarded datagrams: {drops}"));
        }
        let usage = match span {
            Some((t0, t1)) => samples::cpu_use(
                &files.lines(&format!("{role}/host-samples.jsonl"))?,
                t0,
                t1,
                None,
            )?,
            None => None,
        };
        client_hosts.insert(
            role.into(),
            usage
                .as_ref()
                .map(samples::CpuUse::to_json)
                .unwrap_or(Json::Null),
        );
        if let Some(usage) = usage
            && (usage.busy > CLIENT_BUSY * usage.cpus as f64 || usage.busiest > CLIENT_BUSIEST)
        {
            let said = format!(
                "{role} had no room: {} of {} logical CPUs busy, the busiest {} %",
                num::fixed(usage.busy, 1),
                usage.cpus,
                num::fixed(100.0 * usage.busiest, 0)
            );
            if !sends && usage.busy > CLIENT_BUSY * usage.cpus as f64 {
                invalid.push(said);
            } else {
                conditions.push(said.clone());
                room.push(said);
            }
        }
    }
    set(&mut record, "client_hosts", Json::Dict(client_hosts))?;
    set(&mut record, "room_flags", strings(room))?;
    let samples = files.lines("server/samples.jsonl")?;
    let every = meta
        .get("cpus")?
        .or(&EMPTY_DICT)
        .get_or("server", &EMPTY_DICT)?
        .get("all")?;
    let cpus = meta.get("server_cpus")?;
    let confined = if every.truthy() && !cpus.is_null() && cpus != every {
        Some(
            list(cpus)?
                .iter()
                .map(|c| integer(c).map(i128::from))
                .collect::<Result<Vec<_>, _>>()?,
        )
    } else {
        None
    };
    let (cpu, threads, other) = if let Some((t0, t1)) = span {
        (
            samples::cpu_use(&samples, t0, t1, confined.as_deref())?
                .map(|u| u.to_json())
                .unwrap_or(Json::Null),
            samples::wakeups(&samples, t0, t1)?
                .map(|w| w.to_json())
                .unwrap_or(Json::Null),
            samples::other_cpus(&samples, t0, t1)?
                .map(Json::Float)
                .unwrap_or(Json::Null),
        )
    } else {
        (Json::Null, Json::Null, Json::Null)
    };
    set(&mut record, "server_cpu", cpu)?;
    set(&mut record, "server_threads", threads)?;
    set(&mut record, "other_cpus", other.clone())?;
    if gt(numeric(&other)?, OTHER_CPUS) {
        conditions.push(format!(
            "other processes used {} logical CPUs of the Server host",
            num::fixed(number(&other)?.to_f64(), 2)
        ));
    }
    set(
        &mut record,
        "server_throttles",
        files.drops("server", &THROTTLES)?,
    )?;
    let planned = if matches!(
        meta.get("kind")?.as_str(),
        Some("curve" | "probe" | "confine")
    ) {
        let count = sub(number(meta.item("count")?)?, left_out)?;
        num_json(
            Num::Float(planned_datagrams(meta, Some(count))?)
                .round(None)
                .ok_or_else(|| Error("planned datagrams are out of range".into()))?,
        )
    } else {
        Json::Null
    };
    set(&mut record, "planned_pdus", planned)?;
    if let Some(loss) = n(&record, "loss")? {
        let accounted = server_accounts(&record, &drops, sends, text(&nic)?)?;
        let loss = num::max(&[loss, Num::Int(0)]).unwrap();
        let engine = if loss.py_cmp(accounted) == Some(Ordering::Greater) {
            accounted
        } else {
            loss
        };
        let path = sub(loss, engine)?;
        set(&mut record, "engine_loss", num_json(engine))?;
        set(&mut record, "path_loss", num_json(path))?;
        if gt(Some(path), 0.0) && is_zero(Some(engine)) {
            invalid.push(format!(
                "{PATH_LOST} {} Load PDUs that no host accounts for",
                display(&num_json(path))
            ));
        }
    } else {
        set(&mut record, "engine_loss", Json::Null)?;
        set(&mut record, "path_loss", Json::Null)?;
    }
    let met = meets(&record)?;
    set(&mut record, "meets", Json::Bool(met))?;
    set(&mut record, "flags", strings(conditions))?;
    let valid = meta.truthy() && invalid.is_empty();
    set(&mut record, "invalid", strings(invalid))?;
    set(&mut record, "valid", Json::Bool(valid))?;
    Ok(record)
}
