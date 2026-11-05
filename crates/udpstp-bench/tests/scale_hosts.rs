//! The topology and host-state seam, compared with the Python (design §7).
mod common;
use bench_rounds::py::Json;
use udpstp_bench::scale::hosts;

fn int(v: &Json) -> i64 {
    match v {
        Json::Int(n) => (*n).try_into().unwrap(),
        _ => panic!("int"),
    }
}
fn ints(v: &Json) -> Vec<i64> {
    v.as_list().unwrap().iter().map(int).collect()
}
fn json_ints(v: &[i64]) -> Json {
    Json::List(v.iter().map(|n| Json::Int((*n).into())).collect())
}

// Rust-port design §3.7: a CPU set survives a range-list round trip.
#[test]
fn cpu_selections_have_one_range_list_representation() {
    let selection = hosts::cpu_list("6,1-3,2,0,8-9").unwrap();
    assert_eq!(selection.text(), "0-3,6,8-9");
    assert_eq!(hosts::cpu_list(&selection.text()).unwrap(), selection);
    let nic = hosts::cpu_list("3,7-8").unwrap();
    assert_eq!(selection.intersection(&nic).text(), "3,8");
    assert_eq!(selection.difference(&nic).text(), "0-2,6,9");
    assert!(!selection.is_disjoint(&nic));
    assert!(selection.intersection(&nic).is_subset(&selection));
}

// Benchmark host input limits: overlapping ranges spend expansion work too.
#[test]
fn cpu_list_rejects_cumulative_overlapping_expansion() {
    let source = ["0-4095"; 17].join(",");
    let error = hosts::cpu_list(&source).unwrap_err();
    assert_eq!(
        error.to_string(),
        "CPU topology exceeds 65536 expansion steps"
    );
}

// Benchmark topology limits: shared synthetic fixtures cover both runnable routes.
#[test]
fn oversized_topologies_fail_with_bounded_errors() {
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../../scripts/bench/tests/topology-limits.json"
    ))
    .unwrap();
    for case in cases.as_array().unwrap() {
        let source: String = case["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                b[0].as_str()
                    .unwrap()
                    .repeat(b[1].as_u64().unwrap() as usize)
            })
            .collect();
        let result = match case["parser"].as_str().unwrap() {
            "cpu_list" => hosts::cpu_list(&source).map(|_| ()),
            "as_found" => hosts::as_found(&source).map(|_| ()),
            "topology" => hosts::topology(&source).map(|_| ()),
            "queue_irqs" => hosts::queue_irqs(&source).map(|_| ()),
            "irq_affinities" => hosts::irq_affinities(&source).map(|_| ()),
            name => panic!("unknown parser {name}"),
        };
        assert_eq!(
            result.err().map(|e| e.to_string()).as_deref(),
            case["error"].as_str(),
            "{}",
            case["name"]
        );
    }
}

// Benchmark topology limits: inclusive boundaries do not truncate ordinary data.
#[test]
fn topology_limits_accept_exact_boundaries_and_preserve_interpretation() {
    let cpus = hosts::cpu_list(&["0-4095"; 16].join(",")).unwrap();
    assert_eq!(cpus.len(), 4096);
    assert_eq!(cpus.text(), "0-4095");
    assert_eq!(hosts::cpu_list(&["0"; 4096].join(",")).unwrap().text(), "0");
    assert!(hosts::cpu_list(&"x".repeat(1048576)).unwrap().is_empty());
    let topology = hosts::topology(&"0,0\n".repeat(4096)).unwrap();
    assert_eq!(topology, (vec![0; 4096], vec![0]));
    assert_eq!(
        hosts::queue_irqs(&"1 0\n".repeat(4096)).unwrap().len(),
        4096
    );
    let unknown = hosts::as_found(&"irq 1 ? nic\n".repeat(4096)).unwrap();
    assert_eq!(
        unknown.get("interrupts").unwrap().as_dict().unwrap().len(),
        1
    );
    let repeated = "irq 1 0-4095,0-4095,0-4095,0-4095 nic\n".repeat(4);
    let found = hosts::as_found(&repeated).unwrap();
    let irqs = found.get("interrupts").unwrap();
    let cpus = irqs.get("1").unwrap().get("cpus").unwrap();
    assert_eq!(cpus.as_list().unwrap().len(), 4096);
    assert_eq!(
        hosts::irq_affinities(&"1 ?\n".repeat(4096)).unwrap().len(),
        4096
    );
    for source in ["", "?", "3-1", ",x,", "0--1"] {
        assert!(hosts::cpu_list(source).unwrap().is_empty(), "{source}");
    }
    assert_eq!(hosts::cpu_list("1-,6,1-3,2").unwrap().text(), "1-3,6");
    assert_eq!(
        hosts::cpu_list("9223372036854775807").unwrap().text(),
        "9223372036854775807"
    );
    let found = hosts::as_found("irq 1 0-2 nic\r\nirq 1 6,1,6 other\u{85}").unwrap();
    let irq = found.get("interrupts").unwrap().get("1").unwrap();
    assert_eq!(ints(irq.get("cpus").unwrap()), [1, 6]);
    assert_eq!(irq.get("name").unwrap().as_str(), Some("other"));
    assert_eq!(
        hosts::irq_affinities("001 1,0,1\r\n1 ?\nignored extra fields\n").unwrap(),
        [("001".into(), "1,0,1".into()), ("1".into(), "?".into())]
    );
}

#[test]
fn host_readings_match_the_reference() {
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("golden/scale-hosts.json")).unwrap();
    for (i, case) in golden["cases"].as_array().unwrap().iter().enumerate() {
        let a: Vec<_> = case["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(common::untag)
            .collect();
        let result = match case["fn"].as_str().unwrap() {
            "scale.host_script" => Ok(Json::Str(match a[0].as_str().unwrap() {
                "INTERRUPTS_MARK" => hosts::INTERRUPTS_MARK.into(),
                "INTERRUPTS_SCRIPT" => hosts::INTERRUPTS_SCRIPT.into(),
                "STRAYS_SCRIPT" => hosts::STRAYS_SCRIPT.into(),
                "QUEUE_IRQS_SCRIPT" => hosts::queue_irqs_script("/proc/interrupts"),
                "AS_FOUND_SCRIPT" => hosts::as_found_script("/proc/interrupts", "/proc/irq"),
                "HOST_STATE_SCRIPT" => hosts::host_state_script(),
                "ENV_EXTRA" => hosts::ENV_EXTRA.into(),
                name => panic!("{name}"),
            })),
            "scale.queue_irqs_script" => Ok(Json::Str(hosts::queue_irqs_script(
                a.first()
                    .and_then(Json::as_str)
                    .unwrap_or("/proc/interrupts"),
            ))),
            "scale.as_found_script" => Ok(Json::Str(hosts::as_found_script(
                a.first()
                    .and_then(Json::as_str)
                    .unwrap_or("/proc/interrupts"),
                a.get(1).and_then(Json::as_str).unwrap_or("/proc/irq"),
            ))),
            "scale.kernel_script" => {
                let defaults = [
                    "/proc/config.gz",
                    "/proc/cmdline",
                    "/sys/kernel/debug/sched/preempt",
                    "/proc/sys/kernel",
                    "$(uname -r)",
                ];
                let arg = |i| a.get(i).and_then(Json::as_str).unwrap_or(defaults[i]);
                Ok(Json::Str(hosts::kernel_script(
                    arg(0),
                    arg(1),
                    arg(2),
                    arg(3),
                    arg(4),
                )))
            }
            "scale.irq_command" => {
                let layout = a[0]
                    .as_list()
                    .unwrap()
                    .iter()
                    .map(|v| {
                        let v = ints(v);
                        (v[0], v[1])
                    })
                    .collect();
                Ok(Json::Str(hosts::irq_command(&layout)))
            }
            "scale.sha256" | "scale.binary_commit" => {
                let tree = common::Tree::write(&common::dict([
                    ("name", Json::Str("binary-files".into())),
                    ("dirs", Json::List(Vec::new())),
                    ("files", common::dict([])),
                ]));
                let hash = case["fn"] == "scale.sha256";
                let name = if hash {
                    "binary"
                } else {
                    a[0].as_str().unwrap()
                };
                let path = tree.out.join(name);
                if let Some(content) = a[usize::from(!hash)].as_str() {
                    let bytes: Vec<_> = (0..content.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&content[i..i + 2], 16).unwrap())
                        .collect();
                    std::fs::write(
                        if hash {
                            path.clone()
                        } else {
                            tree.out.join(format!("{name}.commit"))
                        },
                        bytes,
                    )
                    .unwrap();
                }
                if hash {
                    hosts::sha256(&path)
                } else {
                    hosts::binary_commit(&path)
                }
                .map(Json::Str)
            }
            "scale.split" => hosts::split(int(&a[0]), int(&a[1]))
                .map(|parts| Json::List(parts.iter().map(|(a, b)| json_ints(&[*a, *b])).collect())),
            "scale.cpu_text" => Ok(Json::Str(hosts::cpu_text(&ints(&a[0])))),
            "scale.topology" => hosts::topology(a[0].as_str().unwrap())
                .map(|(a, b)| Json::List(vec![json_ints(&a), json_ints(&b)])),
            "scale.queue_irqs" => hosts::queue_irqs(a[0].as_str().unwrap())
                .map(|v| Json::List(v.iter().map(|(a, b)| json_ints(&[*a, *b])).collect())),
            "scale.irq_layout" => {
                let pairs: Vec<_> = a[0]
                    .as_list()
                    .unwrap()
                    .iter()
                    .map(|v| {
                        let v = ints(v);
                        (v[0], v[1])
                    })
                    .collect();
                hosts::irq_layout(&pairs, &ints(&a[1]), &ints(&a[2]), a[3].as_str().unwrap())
                    .map(|v| Json::List(v.iter().map(|(k, v)| json_ints(&[*k, *v])).collect()))
            }
            "scale.cpu_list" => hosts::cpu_list(a[0].as_str().unwrap())
                .map(|v| json_ints(&v.into_iter().collect::<Vec<_>>())),
            "scale.as_found" => hosts::as_found(a[0].as_str().unwrap()),
            "scale.host_state" => hosts::host_state(a[0].as_str().unwrap()),
            "scale.nic_receive_drops" => Ok(Json::Str(
                hosts::nic_receive_drops(a[0].as_str().unwrap()).into(),
            )),
            name => panic!("{name}"),
        };
        if case["python"].get("raises").is_some() {
            assert!(result.is_err(), "case {i}");
        } else {
            assert_eq!(
                result.unwrap(),
                common::untag(&case["python"]["ok"]),
                "case {i}"
            );
        }
    }
}
