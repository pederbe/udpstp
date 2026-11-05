//! The retained measurement scripts' command-line seams and recorded outputs.

use std::path::Path;
use std::process::Command;

use bench_rounds::secrets::SecretDir;
use serde_json::{Value, json};

#[cfg(target_os = "linux")]
#[test]
fn the_sampler_writes_the_existing_record_shape_and_finishes_at_its_limit() {
    // The measurement migration preserves the Linux sampler's output interface.
    let work = SecretDir::create().unwrap();
    let out = work.path().join("samples.jsonl");
    let done = Command::new("timeout")
        .arg("5")
        .arg(env!("CARGO_BIN_EXE_udpstp-bench"))
        .arg("sample")
        .arg(&out)
        .args([std::process::id().to_string(), String::new(), "0.6".into()])
        .output()
        .unwrap();
    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    let samples = std::fs::read_to_string(out).unwrap();
    let samples: Vec<Value> = samples
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert!(!samples.is_empty());
    for sample in samples {
        assert!(sample["t"].is_number());
        assert!(!sample["cpus"].as_object().unwrap().is_empty());
        assert!(
            sample["cpus"]
                .as_object()
                .unwrap()
                .values()
                .all(|v| v.as_array().unwrap().len() == 8)
        );
        assert!(sample["process"].is_u64());
        assert!(!sample["threads"].as_object().unwrap().is_empty());
        assert!(sample["udp_server"].is_object());
    }
}

#[test]
fn precision_replays_recorded_results_and_keeps_the_failure_status() {
    // The precision suite's per-run gates remain those of the original suite.
    let work = SecretDir::create().unwrap();
    let input = work.path().join("input.json");
    std::fs::write(&input, r#"{"host":"recorded host","linux":true,"kernel_tx_timestamps":"1","runs":[{"row":50,"direction":"upstream","gated":true,"run":1,"result":{"Output":{"IncrementalResult":[{"IPLayerCapacity":49,"LossCount":0}]},"udpstp":{"KernelSendTimestamps":{"LagP50Us":1,"LagP99Us":101,"LagMaxUs":102}}}}]}"#).unwrap();
    let done = Command::new(env!("CARGO_BIN_EXE_udpstp-bench"))
        .args(["precision", "--replay"])
        .arg(&input)
        .args(["--json"])
        .arg(work.path().join("precision.json"))
        .output()
        .unwrap();
    assert_eq!(
        done.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    let text = String::from_utf8(done.stdout).unwrap();
    assert!(text.contains("achieved 49.0 Mbps, more than 1 % from 50"));
    assert!(text.contains("send lag p99 101 µs over 100"));
    assert!(text.ends_with("1 gated run(s) failed\n"));
}

#[test]
fn the_campaign_report_preserves_its_empty_recorded_report() {
    // The earlier campaign's report writes summary.json even with no runs.
    let work = SecretDir::create().unwrap();
    let done = Command::new(env!("CARGO_BIN_EXE_udpstp-bench"))
        .arg("campaign-report")
        .arg(work.path())
        .args(["2-3", "4-5"])
        .output()
        .unwrap();
    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(work.path().join("summary.json")).unwrap(),
        "[]"
    );
    assert_eq!(
        String::from_utf8(done.stdout).unwrap(),
        serde_json::from_str::<Value>(include_str!("golden/campaign-empty.json")).unwrap()["stdout"].as_str().unwrap()
    );
}

fn campaign_fixture(work: &Path) {
    let down = work.join("down/run1");
    let up = work.join("up/run2");
    for path in [&down, &up] {
        std::fs::create_dir_all(path.join("clients")).unwrap();
    }
    let write = |path: &Path, value: Value| std::fs::write(path, value.to_string()).unwrap();
    write(
        &down.join("meta.json"),
        json!({"case":"down","direction":"downstream","row":50,"mbps":50,"test_interval":10,"connections":2,"packet_bytes":1250}),
    );
    write(
        &up.join("meta.json"),
        json!({"case":"up","direction":"upstream","row":25,"mbps":25,"test_interval":10,"connections":1}),
    );
    write(
        &down.join("clients/1.json"),
        json!({"Output":{"BOMTime":"2026-10-13T06:00:00Z","EOMTime":"2026-10-13T06:00:10Z","IncrementalResult":[
        {"IPLayerCapacity":50.0,"LossCount":0},{"IPLayerCapacity":49.0,"LossCount":2},{"IPLayerCapacity":50.5,"LossCount":0}]},
        "udpstp":{"EndReason":"Completed","Warnings":[{"Code":"BurstsSkipped","Count":2},{"Code":"BurstsWithheld","Count":1}]}}),
    );
    std::fs::write(down.join("clients/2.json"), "interrupted JSON").unwrap();
    write(
        &up.join("clients/1.json"),
        json!({"Output":{},"udpstp":{"EndReason":"Completed"}}),
    );
    let log = [
        json!({"event":"BurstsSkipped","datagrams":7}),
        json!({"event":"BurstsWithheld","datagrams":2}),
        json!({"event":"Suppressed","suppressed_event":"BurstsSkipped","count":12}),
        json!({"event":"DatagramsDropped","count":3}),
        json!({"event":"DataPhaseEnded","reason":"TimedOut"}),
        json!({"event":"TestSummary","skipped_datagrams":3,"withheld_datagrams":1,"ip_layer_sender_bit_rate":49_000_000}),
    ];
    std::fs::write(
        down.join("server.log"),
        log.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\nbad JSON\n",
    )
    .unwrap();
    std::fs::write(up.join("server.log"), "bad JSON\n").unwrap();
    let samples = [
        json!({"t":1791871202.0,"cpus":{"2":[100,0,50,500,2,1,2,0],"4":[100,0,30,900,0,0,0,0]},"process":100,
            "threads":{"42":["udpstp-engine-0",100,10,2],"43":["control",0,3,1]},"udp_server":{"RcvbufErrors":0,"SndbufErrors":0},"udp_clients":{"RcvbufErrors":0,"SndbufErrors":0}}),
        json!({"t":1791871208.0,"cpus":{"2":[130,0,80,900,2,1,22,0],"4":[130,0,50,1300,0,0,0,0]},"process":220,
            "threads":{"42":["udpstp-engine-0",200,70,2],"43":["control",20,8,1]},"udp_server":{"RcvbufErrors":1,"SndbufErrors":2},"udp_clients":{"RcvbufErrors":3,"SndbufErrors":1}}),
    ];
    std::fs::write(
        down.join("samples.jsonl"),
        samples
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    std::fs::write(up.join("samples.jsonl"), "").unwrap();
    std::fs::write(down.join("pacing.start"), "1791871209.0").unwrap();
    let mut pcap = Vec::new();
    for value in [0xa1b23c4du32, 0x00040002, 0, 0, 70, 1] {
        pcap.extend(value.to_le_bytes());
    }
    for index in 0..16u32 {
        let mut packet = vec![0u8; 70];
        packet[14] = 0x45;
        packet[23] = 17;
        packet[34..36].copy_from_slice(&5000u16.to_be_bytes());
        packet[42..44].copy_from_slice(&[0xbe, 0xef]);
        packet[46..50].copy_from_slice(&(index * 2).to_be_bytes());
        packet[62..66].copy_from_slice(&1791871200u32.to_be_bytes());
        packet[66..70].copy_from_slice(&(100_000_000 + index * 1_000_000).to_be_bytes());
        for value in [
            1791871200u32,
            100_020_000 + index * 1_000_000 + (index % 4) * 5000,
            70,
            70,
        ] {
            pcap.extend(value.to_le_bytes());
        }
        pcap.extend(packet);
    }
    std::fs::write(down.join("pacing.pcap"), pcap).unwrap();
}

#[test]
fn the_campaign_report_matches_the_recorded_reference() {
    // Independent CPython output from the same fixed campaign files.
    let work = SecretDir::create().unwrap();
    campaign_fixture(work.path());
    let reference: Value =
        serde_json::from_str(include_str!("golden/campaign-recorded.json")).unwrap();
    let done = Command::new(env!("CARGO_BIN_EXE_udpstp-bench"))
        .arg("campaign-report")
        .arg(work.path())
        .args(["2", "4"])
        .output()
        .unwrap();
    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    assert_eq!(
        String::from_utf8(done.stdout).unwrap(),
        reference["stdout"].as_str().unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(work.path().join("summary.json")).unwrap(),
        reference["summary"].as_str().unwrap()
    );
}

#[test]
fn recorded_precision_judgments_and_json_match_the_reference() {
    //  preserves 's figures, gates and per-run failure lists.
    let documents: Value =
        serde_json::from_str(include_str!("golden/precision-recorded.json")).unwrap();
    for (index, document) in documents.as_array().unwrap().iter().enumerate() {
        for (case, expected) in document["runs"]
            .as_array()
            .unwrap()
            .iter()
            .zip(document["python"]["judgments"].as_array().unwrap())
        {
            let result = bench_rounds::py::Json::parse(&case["result"].to_string()).unwrap();
            let (figures, failures) = udpstp_bench::precision::judge(
                &result,
                case["row"].as_u64().unwrap() as u16,
                case["direction"].as_str().unwrap(),
                case["gated"].as_bool().unwrap(),
                document["linux"].as_bool().unwrap(),
            )
            .unwrap();
            let mut actual: Value =
                serde_json::from_str(&bench_rounds::py::dumps(&figures)).unwrap();
            actual["run"] = case["run"].clone();
            assert_eq!(actual, expected["figures"]);
            assert_eq!(json!(failures), expected["failures"]);
        }
        let work = SecretDir::create().unwrap();
        let input = work.path().join("input.json");
        std::fs::write(&input, document.to_string()).unwrap();
        let done = Command::new(env!("CARGO_BIN_EXE_udpstp-bench"))
            .args(["precision", "--replay"])
            .arg(&input)
            .arg("--json")
            .arg(work.path().join("precision.json"))
            .output()
            .unwrap();
        assert_eq!(
            done.status.code(),
            Some([2, 1, 0][index]),
            "{}",
            String::from_utf8_lossy(&done.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(work.path().join("precision.json")).unwrap(),
            document["python"]["json"].as_str().unwrap()
        );
    }
}

#[test]
fn a_missing_measurement_tool_is_refused_before_the_round_or_ledger() {
    // : an unusable host executable must not allocate static or rented hosts.
    let work = SecretDir::create().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = work.path().join("out");
    let ledger = work.path().join("ledger.json");
    let done = Command::new(env!("CARGO_BIN_EXE_udpstp-bench"))
        .args(["round", "--provider", "static", "--yes"])
        .args([
            "--host",
            "server=local,127.0.0.1",
            "--host",
            "client=local,127.0.0.1",
        ])
        .arg("--matrix")
        .arg(root.join("scripts/bench/tests/smoke.toml"))
        .arg("--udpstp")
        .arg(work.path().join("udpstp"))
        .arg("--out")
        .arg(&out)
        .arg("--ledger")
        .arg(&ledger)
        .arg("--measurement-tool")
        .arg(work.path().join("missing"))
        .output()
        .unwrap();
    assert_eq!(
        done.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    assert!(String::from_utf8_lossy(&done.stderr).contains("measurement tool"));
    assert!(!out.exists());
    assert!(!ledger.exists());
}
