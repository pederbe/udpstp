//! Host scripts kept byte for byte with the reference (design §3.3).

use indexmap::IndexMap;

pub const INTERRUPTS_MARK: &str = "--- /proc/softirqs\n";
pub const INTERRUPTS_SCRIPT: &str = "echo '--- /proc/softirqs'; cat /proc/softirqs; echo '--- /proc/interrupts'; cat /proc/interrupts";

/// Only the Round's programs whose current directory is inside its work
/// directory. An independently running Server on a static host is untouched.
pub const STRAYS_SCRIPT: &str = r#"for p in $(pgrep -x 'udpstp|udpstp-bench|udpst|dpproto'); do case $(readlink /proc/$p/cwd 2>/dev/null) in "$PWD"|"$PWD"/*) kill -KILL $p 2>/dev/null && echo $p;; esac; done; true"#;

pub fn queue_irqs_script(interrupts: &str) -> String {
    format!(
        concat!(
            r#"[ -n "$dev" ] || dev=$(ip -o route get 1.1.1.1 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p');"#,
            r#" [ -n "$dev" ] && grep -E "[[:space:]]$dev-[^[:space:]]*-[0-9]+\$" {interrupts} |"#,
            r#" while read -r irq rest; do name=${{rest##* }}; echo "${{irq%:}} ${{name##*-}} $name"; done;"#,
            r#" true"#,
        ),
        interrupts = interrupts
    )
}

pub fn irq_command(layout: &IndexMap<i64, i64>) -> String {
    let mut pairs: Vec<_> = layout.iter().collect();
    pairs.sort_unstable_by_key(|(irq, _)| *irq);
    let sets = pairs
        .iter()
        .map(|(irq, cpu)| {
            format!("echo {cpu} | sudo -n tee /proc/irq/{irq}/smp_affinity_list > /dev/null")
        })
        .collect::<Vec<_>>()
        .join("; ");
    let reads = pairs
        .iter()
        .map(|(irq, _)| format!("echo \"{irq} $(cat /proc/irq/{irq}/smp_affinity_list)\""))
        .collect::<Vec<_>>()
        .join("; ");
    format!("{sets}; {reads}")
}

pub fn as_found_script(interrupts: &str, irq_dir: &str) -> String {
    format!(
        concat!(
            r#"echo "rmem_max $(cat /proc/sys/net/core/rmem_max 2>/dev/null)";"#,
            r#" echo "wmem_max $(cat /proc/sys/net/core/wmem_max 2>/dev/null)";"#,
            r#" echo "irqbalance $(systemctl is-active irqbalance 2>/dev/null)";"#,
            r#" [ -n "$dev" ] || dev=$(ip -o route get 1.1.1.1 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p');"#,
            r#" [ -n "$dev" ] && grep -E "[[:space:]]$dev(-[^[:space:]]*)?\$" {interrupts} |"#,
            r#" while read -r irq rest; do irq=${{irq%:}};"#,
            r#" cpus=$(cat {irq_dir}/$irq/effective_affinity_list 2>/dev/null);"#,
            r#" [ -n "$cpus" ] || cpus=$(cat {irq_dir}/$irq/smp_affinity_list 2>/dev/null);"#,
            r#" echo "irq $irq ${{cpus:-?}} ${{rest##* }}"; done; true"#,
        ),
        interrupts = interrupts,
        irq_dir = irq_dir
    )
}

pub fn kernel_script(
    config: &str,
    cmdline: &str,
    debugfs: &str,
    sysctl: &str,
    release: &str,
) -> String {
    format!(
        r#"c=$( (zcat {config} || cat "/boot/config-{release}") 2>/dev/null )
h=$(printf '%s\n' "$c" | sed -n 's/^CONFIG_HZ=//p'); [ -n "$h" ] && echo "hz $h"
d=; printf '%s\n' "$c" | grep -q '^CONFIG_PREEMPT_DYNAMIC=y' && d=1
p=$(sed -n 's/.*(\([a-z]*\)).*/\1/p' {debugfs} 2>/dev/null); w="as running"
if [ -z "$p" ] && [ -n "$d" ]; then p=$(tr ' ' '\n' < {cmdline} 2>/dev/null | sed -n 's/^preempt=//p' | tail -n 1); w="the command line's"; fi
if [ -z "$p" ]; then
  p=$(printf '%s\n' "$c" | sed -n 's/^CONFIG_PREEMPT_NONE=y$/none/p; s/^CONFIG_PREEMPT_VOLUNTARY=y$/voluntary/p; s/^CONFIG_PREEMPT=y$/full/p; s/^CONFIG_PREEMPT_LAZY=y$/lazy/p; s/^CONFIG_PREEMPT_RT=y$/rt/p' | paste -sd+ -)
  if [ -n "$d" ]; then w="the kernel's default"; else w="built in"; fi
fi
[ -n "$p" ] && echo "preempt $p ($w)"
if [ -r {sysctl}/sched_bore ]; then s="BORE, sched_bore $(cat {sysctl}/sched_bore)"
elif printf '%s\n' "$c" | grep -q '^CONFIG_SCHED_BORE=y'; then s=BORE
elif printf '%s\n' "$c" | grep -q '^CONFIG_SCHED_BMQ=y'; then s=BMQ
elif printf '%s\n' "$c" | grep -q '^CONFIG_SCHED_PDS=y'; then s=PDS
else s=$(printf '%s\n' "{release}" | awk -F. '{{ print ($1 > 6 || ($1 == 6 && $2 + 0 >= 6)) ? "EEVDF" : "CFS" }}'); fi
[ -r {sysctl}/sched_poc_selector ] && s="$s, sched_poc_selector $(cat {sysctl}/sched_poc_selector)"
echo "scheduler $s"
"#
    )
}

pub fn host_state_script() -> String {
    const BEFORE: &str = r#"
for p in /sys/class/power_supply/*; do [ -r "$p/type" ] && echo "power $(basename "$p") $(cat "$p/type") $(cat "$p/online" 2>/dev/null || echo -) $(cat "$p/status" 2>/dev/null | tr ' ' _)"; done
g=$(cat /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor 2>/dev/null | sort | uniq -c | awk '{ printf "%s%s on %s", s, $2, $1; s = ", " }'); [ -n "$g" ] && echo "governor $g"
d=$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_driver 2>/dev/null) && echo "driver $d"
e=$(cat /sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference 2>/dev/null) && echo "epp $e"
t=$(cat /sys/devices/system/cpu/intel_pstate/no_turbo 2>/dev/null) && echo "no_turbo $t"
i=$(cat /sys/devices/system/cpu/cpuidle/current_driver 2>/dev/null) && echo "idle_driver $i"
i=$(cat /sys/devices/system/cpu/cpuidle/current_governor 2>/dev/null || cat /sys/devices/system/cpu/cpuidle/current_governor_ro 2>/dev/null) && echo "idle_governor $i"
echo "kernel $(uname -r) $(uname -v)"
"#;
    const AFTER: &str = r#"
s=$(cat /sys/kernel/sched_ext/state 2>/dev/null) && echo "sched_ext $s $(cat /sys/kernel/sched_ext/root/ops 2>/dev/null)"
ip -br addr 2>/dev/null | awk '$1 != "lo" { $1 = $1; print "link " $0 }'
w=$(nmcli radio wifi 2>/dev/null) && echo "wifi $w"
"#;
    format!(
        "{BEFORE}{}{AFTER}",
        kernel_script(
            "/proc/config.gz",
            "/proc/cmdline",
            "/sys/kernel/debug/sched/preempt",
            "/proc/sys/kernel",
            "$(uname -r)"
        )
    )
}

pub const ENV_EXTRA: &str = r#"
echo; lscpu -p=CPU,CORE,SOCKET,NODE 2>/dev/null
echo; echo "irqbalance $(systemctl is-active irqbalance 2>/dev/null)"
grep -E "ena|eth|virtio|${dev:-ena}" /proc/interrupts 2>/dev/null | head -40
for i in $(grep -E "ena|eth|virtio|${dev:-ena}" /proc/interrupts 2>/dev/null | cut -d: -f1); do
  echo "irq $i: $(cat /proc/irq/$i/smp_affinity_list 2>/dev/null)"; done
for q in /sys/class/net/"$dev"/queues/rx-*; do echo "$q rps $(cat "$q"/rps_cpus 2>/dev/null)"; done
for q in /sys/class/net/"$dev"/queues/tx-*; do echo "$q xps $(cat "$q"/xps_cpus 2>/dev/null)"; done
sysctl net.core.netdev_max_backlog 2>/dev/null
echo; for i in $(grep -E '^ *[0-9]+:' /proc/interrupts 2>/dev/null | cut -d: -f1); do
  echo "irq $i on $(cat /proc/irq/$i/effective_affinity_list 2>/dev/null): $(grep -E "^ *$i:" /proc/interrupts | awk '{ print $NF }')"; done
"#;
