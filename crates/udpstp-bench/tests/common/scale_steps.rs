//! ScaleRound's actual host calls, files and judgments against CPython.
//! Each call of each engine path is also failed and interrupted in turn.
use crate::common;
use crate::common::step_signal::Interrupt;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use bench_rounds::host::{Completed, HostError, Limit, RemotePath, Shell, Stop};
use bench_rounds::interrupt::Interrupted;
use bench_rounds::num::Num;
use bench_rounds::process::Runner;
use bench_rounds::provider::{Access, Leftover, Provider, ProviderError, RoundId};
use bench_rounds::py::{self, Json};
use bench_rounds::round::{Clock, Log, RoundBody, Stopped};
use bench_rounds::secrets::SecretDir;
use indexmap::IndexMap;
use udpstp_bench::first_round::round::{Obudpst, make_keys};
use udpstp_bench::scale::{
    matrix,
    round::{Options, ScaleRound},
};

fn s(s: &str) -> Json {
    Json::Str(s.into())
}
fn list(value: &Json) -> &[Json] {
    value.as_list().expect("list")
}
fn text(value: &Json) -> &str {
    value.as_str().expect("text")
}
fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    value
        .as_dict()
        .and_then(|d| d.get(key))
        .unwrap_or(&Json::Null)
}
fn number(value: &Json) -> f64 {
    Num::from_json(value).unwrap().to_f64()
}
fn int(value: &Json) -> i128 {
    if let Json::Int(n) = value {
        *n
    } else {
        panic!("int")
    }
}
fn n(value: Num) -> Json {
    match value {
        Num::Int(i) => Json::Int(i),
        Num::Float(f) => Json::Float(f),
    }
}

#[derive(Default)]
struct InterruptPoint {
    seen: Mutex<bool>,
    ready: Condvar,
}
impl InterruptPoint {
    fn raise(&self) {
        *self.seen.lock().unwrap() = true;
        self.ready.notify_all();
    }
    fn wait(&self) {
        let (seen, _) = self
            .ready
            .wait_timeout_while(self.seen.lock().unwrap(), Duration::from_secs(2), |seen| {
                !*seen
            })
            .unwrap();
        assert!(
            *seen,
            "the scripted Client interrupt must precede the sleep"
        );
    }
}

struct FakeClock {
    interrupt_first: Option<Arc<InterruptPoint>>,
    slept: Mutex<Vec<f64>>,
}
impl Clock for FakeClock {
    fn now(&self) -> f64 {
        1_790_000_000.0
    }
    fn sleep(&self, d: Duration) -> Result<(), Interrupted> {
        if let Some(interrupt) = &self.interrupt_first {
            interrupt.wait();
        }
        // The Python harness replaces sleep with a recorder. Interrupts
        // arrive at scripted host calls, independent of worker ordering.
        // Real signal propagation is checked separately by round_interrupt.
        self.slept.lock().unwrap().push(d.as_secs_f64());
        Ok(())
    }
    fn pause(&self, d: Duration) {
        self.slept.lock().unwrap().push(d.as_secs_f64());
    }
}

struct Replay<'a> {
    role: String,
    address: String,
    destination: String,
    work: RemotePath,
    python: &'a [Json],
    calls: Arc<Mutex<Vec<Json>>>,
    evidence: &'a Json,
    interrupt: Interrupt,
    interrupt_first: Option<Arc<InterruptPoint>>,
}
impl Replay<'_> {
    fn answer(&self, call: Json) -> Result<Json, Stop> {
        let mut made = self.calls.lock().unwrap();
        let answer = self.python.get(made.len()).map(|p| list(p)[1].clone());
        made.push(call);
        let answer = answer.ok_or_else(|| HostError::new("a call the Python didn't make"))?;
        match text(&list(&answer)[0]) {
            "interrupt" => {
                self.interrupt.raise();
                if let Some(interrupt) = &self.interrupt_first {
                    interrupt.raise();
                }
                Err(Stop::Interrupted)
            }
            "fail" => Err(HostError::new(text(&list(&answer)[1])).into()),
            _ => Ok(answer),
        }
    }
    fn call(&self, script: &str, limit: Limit, check: bool) -> Result<Completed, Stop> {
        let answer = self.answer(Json::List(vec![
            s("run"),
            s(script),
            n(limit.seconds()),
            Json::Bool(check),
        ]))?;
        if list(&answer)[0] == s("timeout") {
            return Err(HostError::timed_out(&self.role, script, limit).into());
        }
        let a = list(&answer);
        let done = Completed {
            returncode: int(&a[1]) as i32,
            stdout: text(&a[2]).into(),
            stderr: text(&a[3]).into(),
        };
        if check && !done.success() {
            return Err(
                HostError::exited(&self.role, script, done.returncode, &done.stderr).into(),
            );
        }
        Ok(done)
    }
}
impl Shell for Replay<'_> {
    fn role(&self) -> &str {
        &self.role
    }
    fn address(&self) -> &str {
        &self.address
    }
    fn public_address(&self) -> Option<&str> {
        None
    }
    fn destination(&self) -> &str {
        &self.destination
    }
    fn work(&self) -> &RemotePath {
        &self.work
    }
    fn run(&self, script: &str, limit: Limit) -> Result<Completed, Stop> {
        self.call(script, limit, false)
    }
    fn run_checked(&self, script: &str, limit: Limit) -> Result<Completed, Stop> {
        self.call(script, limit, true)
    }
    fn put(&self, local: &Path, name: &RemotePath, mode: u32) -> Result<(), Stop> {
        self.answer(Json::List(vec![
            s("put"),
            s(local.file_name().unwrap().to_str().unwrap()),
            s(name.as_str()),
            s(&format!("{mode:04o}")),
        ]))?;
        Ok(())
    }
    fn output(&self, _: &str, _: Limit) -> Result<Vec<u8>, Stop> {
        panic!("fetch overridden")
    }
    fn fetch_dir(&self, from: &RemotePath, to: &Path) -> Result<(), Stop> {
        let a = self.answer(Json::List(vec![
            s("fetch"),
            s(from.as_str()),
            s(&self.role),
        ]))?;
        if list(&a)[0] == s("timeout") {
            return Err(HostError::new("fetch timed out").into());
        }
        fs::create_dir_all(to).unwrap();
        for (path, value) in self.evidence.as_dict().unwrap() {
            if let Some(path) = path.strip_prefix(&format!("{}/", self.role)) {
                let path = to.join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, text(value)).unwrap();
            }
        }
        Ok(())
    }
    fn fetch_file(&self, from: &RemotePath, to: &Path) -> Result<(), Stop> {
        let a = self.answer(Json::List(vec![
            s("fetch_file"),
            s(from.as_str()),
            s(to.file_name().unwrap().to_str().unwrap()),
        ]))?;
        if list(&a)[0] == s("timeout") {
            return Err(HostError::new("fetch timed out").into());
        }
        fs::write(to, b"udpst-fixture\n").unwrap();
        Ok(())
    }
}

struct Provided {
    rented: bool,
    kept: bool,
}
impl Provider for Provided {
    fn name(&self) -> &'static str {
        if self.rented { "aws" } else { "static" }
    }
    fn rented(&self) -> bool {
        self.rented
    }
    fn keep_files(&mut self, role: &str, names: &[&str]) -> Result<(), ProviderError> {
        assert_eq!(role, "server");
        assert_eq!(names, ["udpstp", "udpst"]);
        self.kept = true;
        Ok(())
    }
    fn price_per_hour(&self, _: &str) -> Result<f64, ProviderError> {
        unreachable!()
    }
    fn check(&mut self, _: f64, _: f64) -> Result<(), ProviderError> {
        unreachable!()
    }
    fn reap(&mut self, _: f64) -> Result<Vec<RoundId>, ProviderError> {
        unreachable!()
    }
    fn create(
        &mut self,
        _: &RoundId,
        _: f64,
        _: &[&str],
        _: &Access,
    ) -> Result<IndexMap<String, Box<dyn Shell>>, ProviderError> {
        unreachable!()
    }
    fn destroy(&mut self, _: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        unreachable!()
    }
    fn leftovers(&mut self, _: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        unreachable!()
    }
    fn recheck_after(&self) -> Option<Duration> {
        unreachable!()
    }
    fn manual_cleanup(&self, _: &RoundId) -> Vec<String> {
        unreachable!()
    }
}

fn files(root: &Path, directory: &Path, found: &mut BTreeMap<String, String>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(root, &path, found);
        } else {
            found.insert(
                path.strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/"),
                fs::read_to_string(path).unwrap(),
            );
        }
    }
}

fn compare(got: &Json, expected: &Json, context: &str) {
    if got == expected {
        return;
    }
    if let (Some(a), Some(b)) = (got.as_list(), expected.as_list()) {
        for (i, (a, b)) in a.iter().zip(b).enumerate() {
            if a != b {
                compare(a, b, &format!("{context}[{i}]"));
            }
        }
        assert_eq!(a.len(), b.len(), "{context}: lengths");
    }
    if let (Some(a), Some(b)) = (got.as_dict(), expected.as_dict()) {
        assert_eq!(
            a.keys().collect::<Vec<_>>(),
            b.keys().collect::<Vec<_>>(),
            "{context}: keys"
        );
        for (key, a) in a {
            compare(a, &b[key], &format!("{context}.{key}"));
        }
    }
    if let (Some(a), Some(b)) = (got.as_str(), expected.as_str()) {
        for (line, (a, b)) in a.lines().zip(b.lines()).enumerate() {
            assert_eq!(a, b, "{context}, line {}", line + 1);
        }
    }
    assert_eq!(got, expected, "{context}");
}

pub fn replay(interrupt_first: bool, exhaustive: bool) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let golden: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("crates/udpstp-bench/tests/golden/scale-round.json")).unwrap(),
    )
    .unwrap();
    let pool = common::scale_judge::pool(&golden["values"]);
    let mut cases: Vec<_> = common::corpus_cases(&golden, exhaustive || interrupt_first)
        .iter()
        .enumerate()
        .collect();
    if interrupt_first {
        let selected = cases
            .iter()
            .copied()
            .find(|(_, case)| {
                let input = &pool[case["args"][0].as_u64().unwrap() as usize];
                let expected = &pool[case["python"]["ok"].as_u64().unwrap() as usize];
                field(input, "select") == &Json::List(vec![s("prototype-b"), s("curve")])
                    && list(field(field(expected, "calls"), "client2"))
                        .iter()
                        .any(|pair| {
                            let pair = list(pair);
                            pair[1] == Json::List(vec![s("interrupt")])
                                && text(&list(&pair[0])[1]).contains("--side q")
                        })
            })
            .expect("a prototype Client interrupt in the reference corpus");
        cases = vec![selected];
    }
    common::for_each_case(&cases, |_, &(index, case)| {
        let input = &pool[case["args"][0].as_u64().unwrap() as usize];
        let expected = &pool[case["python"]["ok"]
            .as_u64()
            .unwrap_or_else(|| panic!("case {index}: {case}"))
            as usize];
        steps(
            index,
            input,
            expected,
            Interrupt::default(),
            interrupt_first,
        );
    });
}

pub fn steps(
    index: usize,
    input: &Json,
    expected: &Json,
    signal: Interrupt,
    interrupt_first: bool,
) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let interrupt = interrupt_first.then(|| Arc::new(InterruptPoint::default()));
    if interrupt_first {
        assert_eq!(
            field(expected, "slept"),
            &Json::List(vec![Json::Float(1.5)])
        );
    }
    let temp = SecretDir::create().unwrap();
    let out = temp.path().join("round");
    let binaries = temp.path().join("binaries");
    fs::create_dir(&out).unwrap();
    fs::create_dir(&binaries).unwrap();
    for name in ["udpstp", "dpproto", "udpst"] {
        fs::write(binaries.join(name), format!("{name}-fixture\n")).unwrap();
    }
    fs::write(binaries.join("udpstp.commit"), "fixture-binary-commit\n").unwrap();
    let source = text(field(input, "matrix"));
    let source = if source.contains('\n') {
        source.to_string()
    } else {
        fs::read_to_string(root.join(source))
            .unwrap()
            .replace("\r\n", "\n")
    };
    let path = temp.path().join("matrix.toml");
    fs::write(&path, &source).unwrap();
    let mut matrix = matrix::parse(&source).unwrap();
    if field(input, "select").truthy() {
        let select = list(field(input, "select"));
        matrix
            .runs
            .retain(|r| r.engine == text(&select[0]) && r.kind == text(&select[1]));
        matrix.runs.truncate(1);
    }
    let opts = field(input, "options");
    let option = |key| {
        opts.as_dict()
            .and_then(|d| d.get(key))
            .is_some_and(Json::truthy)
    };
    let flag = signal.flag.clone();
    let clock = FakeClock {
        interrupt_first: interrupt.clone(),
        slept: Mutex::new(Vec::new()),
    };
    let log = Log::kept();
    let options = Options {
        measurement_tool: None,
        udpstp: binaries.join("udpstp"),
        prototype: Some(binaries.join("dpproto")),
        obudpst: Some(if field(opts, "obudpst").as_str() == Some("build") {
            Obudpst::Build
        } else {
            Obudpst::Binary(binaries.join("udpst"))
        }),
        raise_buffers: option("raise_buffers"),
        hold_irqs: option("hold_irqs"),
        as_found: opts
            .as_dict()
            .and_then(|d| d.get("as_found"))
            .is_none_or(Json::truthy),
        udpstp_port: 24601,
        obudpst_port: 25000,
        out: out.clone(),
        ssh_dir: None,
        matrix: path,
        scripts: root.join("scripts"),
        obudpst_script: udpstp_bench::obudpst::script(),
    };
    let calls: IndexMap<_, _> = field(expected, "calls")
        .as_dict()
        .unwrap()
        .keys()
        .map(|role| (role.clone(), Arc::new(Mutex::new(Vec::new()))))
        .collect();
    let mut hosts: IndexMap<String, Box<dyn Shell>> = IndexMap::new();
    for (i, role) in ["server", "client1", "client2"].iter().enumerate() {
        if let Some(made) = calls.get(*role) {
            hosts.insert(
                role.to_string(),
                Box::new(Replay {
                    role: role.to_string(),
                    destination: if field(input, "shared").truthy() {
                        "shared".into()
                    } else {
                        role.to_string()
                    },
                    address: format!("127.0.0.{}", i + 1),
                    work: RemotePath::new("/work").unwrap(),
                    python: list(field(field(expected, "calls"), role)),
                    calls: made.clone(),
                    evidence: field(expected, "evidence"),
                    interrupt: signal.clone(),
                    interrupt_first: interrupt.clone(),
                }),
            );
        }
    }
    let mut provider = Provided {
        rented: field(input, "provider").as_str() == Some("aws"),
        kept: false,
    };
    let mut body = ScaleRound::new(options, matrix, &clock, Runner::new(flag.clone())).unwrap();
    make_keys(&temp).unwrap();
    let deadline = if field(input, "deadline").is_null() {
        1_790_007_200.0
    } else {
        number(field(input, "deadline"))
    };
    let outcome = body.steps(hosts, &mut provider, temp.path(), deadline, &log);
    let stopping: BTreeMap<_, _> = calls
        .iter()
        .map(|(role, c)| (role.clone(), c.lock().unwrap().len()))
        .collect();
    body.stop_servers(&flag.stop_acting(), &log);
    let clean = |value: &str| {
        value
            .replace(out.to_str().unwrap(), "<out>")
            .replace(binaries.to_str().unwrap(), "<binaries>")
            .replace(temp.path().to_str().unwrap(), "<keys>")
            .replace("<out>\\scale\\report.md", "<out>/scale/report.md")
            .replace("<out>\\scale/report.md", "<out>/scale/report.md")
    };
    let outcome = Json::List(match outcome {
        Ok(()) => vec![s("ok")],
        Err(Stopped::Interrupted) => vec![s("interrupted")],
        Err(Stopped::Failed(why)) => vec![s("stopped"), s(&clean(&why))],
    });
    for (role, made) in &calls {
        let expected_calls = list(field(field(expected, "calls"), role))
            .iter()
            .map(|p| list(p)[0].clone())
            .collect();
        // Reference names the four scripts shared with first-Round fixtures.
        let actual = made
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .map(|mut c| {
                if let Json::List(v) = &mut c
                    && v[0] == s("run")
                {
                    let named = [
                        (bench_rounds::counters::SCRIPT, "<counters script>"),
                        (bench_rounds::host::QUIET_SCRIPT, "<quiet script>"),
                        (bench_rounds::dead_man::CHECK_SCRIPT, "<dead-man check>"),
                    ];
                    if let Some((_, name)) = named.iter().find(|(script, _)| v[1] == s(script)) {
                        v[1] = s(name);
                    }
                }
                c
            })
            .collect();
        compare(
            &Json::List(actual),
            &Json::List(expected_calls),
            &format!("case {index}, {role} calls"),
        );
        assert_eq!(
            stopping[role] as i128,
            int(field(field(expected, "stopping"), role)),
            "case {index}, {role} stopping"
        );
    }
    compare(
        &outcome,
        field(expected, "outcome"),
        &format!("case {index}, outcome"),
    );
    let logs = Json::List(log.lines().iter().map(|line| s(&clean(line))).collect());
    compare(
        &logs,
        field(expected, "log"),
        &format!("case {index}, logs"),
    );
    compare(
        &Json::List(body.results().to_vec()),
        field(expected, "results"),
        &format!("case {index}, results"),
    );
    assert_eq!(provider.kept, field(expected, "keep").truthy());
    let mut slept = clock.slept.lock().unwrap().clone();
    slept.sort_by(f64::total_cmp);
    let expected_slept: Vec<_> = list(field(expected, "slept")).iter().map(number).collect();
    assert_eq!(slept, expected_slept, "case {index}, sleeps");
    let mut made = BTreeMap::new();
    files(&out, &out, &mut made);
    if let Some(stamps) = made.get_mut("scale/stamps.json") {
        let mut value = Json::parse(stamps).unwrap();
        let Json::Dict(fields) = &mut value else {
            panic!("stamps");
        };
        let commit = fields.shift_remove("udpstp-bench built from").unwrap();
        assert_eq!(text(&commit), env!("UDPSTP_BENCH_COMMIT"));
        let hash = fields.shift_remove("udpstp-bench SHA-256").unwrap();
        assert_eq!(text(&hash).len(), 64);
        assert!(text(&hash).bytes().all(|b| b.is_ascii_hexdigit()));
        let sampler = fields.shift_remove("measurement tool SHA-256").unwrap();
        assert_eq!(
            sampler, hash,
            "the default host tool is the running executable"
        );
        *stamps = py::dumps_indent(&value, 1);
    }
    if let Some(report) = made.get_mut("scale/report.md") {
        *report = report
            .split('\n')
            .filter(|line| {
                ![
                    "udpstp-bench built from",
                    "udpstp-bench SHA-256",
                    "measurement tool SHA-256",
                ]
                .iter()
                .any(|k| line.contains(k))
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    let made = Json::Dict(made.into_iter().map(|(k, v)| (k, s(&clean(&v)))).collect());
    // Source identity changes with the sampler port. Every result, binary hash
    // and protocol-related stamp still compares against the independent record.
    let normalize = |files: &Json| {
        let mut files = files.as_dict().unwrap().clone();
        if let Some(value) = files.get_mut("scale/stamps.json") {
            let mut document = Json::parse(value.as_str().unwrap()).unwrap();
            let Json::Dict(fields) = &mut document else {
                panic!("stamps");
            };
            for key in ["server-scale.py SHA-256", "server-scale.sh SHA-256"] {
                fields.shift_remove(key);
            }
            *value = s(&py::dumps_indent(&document, 1));
        }
        if let Some(value) = files.get_mut("scale/report.md") {
            *value = s(&value
                .as_str()
                .unwrap()
                .split('\n')
                .filter(|line| {
                    !["server-scale.py SHA-256", "server-scale.sh SHA-256"]
                        .iter()
                        .any(|key| line.contains(key))
                })
                .collect::<Vec<_>>()
                .join("\n"));
        }
        Json::Dict(files)
    };
    compare(
        &normalize(&made),
        &normalize(field(expected, "files")),
        &format!("case {index}, files"),
    );
}
