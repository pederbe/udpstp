//! What a Round may cost, and what the Rounds of a month cost: the worst
//! case, the Round's and the month's caps, and the ledger both tools keep
//! (`scripts/bench/round.py:92-113, 176-204, 701-728`).
//!
//! The plans' budgets quote the worst case, so it comes out as the
//! Python's to the bit: the same operations in the same order, `sum()` as
//! CPython 3.14 adds ([`num::sum`]), and Python's rounding
//! ([`num::py_round`]).

pub mod ledger;

use indexmap::IndexMap;

use crate::num::{self, Num};
use crate::py::Json;

/// A gigabyte, as the egress is priced.
const GB: f64 = 1e9;

/// What a Round Run adds to the transfer a Round pays for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunTransfer {
    /// A Downstream Test to home leaves the provider as paid egress: its
    /// bound, Mbit/s, and its Test Interval, s.
    pub home_downstream: Option<(f64, f64)>,
    /// The results the Round Run brings back from its hosts, GB (a
    /// Server-scale run's; 0 for a first round's Test).
    pub download_gb: f64,
}

/// The provider's prices besides the hosts', `$` (`prices.toml`'s
/// `[aws.common]`; 0 where it doesn't say).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Prices {
    pub public_ipv4_per_hour: f64,
    pub disk_per_host_hour: f64,
    pub internet_egress_per_gb: f64,
}

/// The most a Round can cost, `$` (`worst_case`'s dict).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorstCase {
    pub compute: f64,
    pub ipv4: f64,
    pub disk: f64,
    pub egress_gb: f64,
    pub egress: f64,
    /// Rounded up to the cent: what the caps compare.
    pub total: f64,
}

impl WorstCase {
    /// `worst_case(...)`: every host for `max_hours` at its price
    /// (`host_prices`, one for each role, in the roles' order), its public
    /// address and disk, and the transfer the Round Runs could make, with
    /// a share of them run again (`rerun_share`) and 20 % margin on the
    /// egress to home.
    pub fn new(
        runs: &[RunTransfer],
        host_prices: &[Num],
        max_hours: f64,
        prices: &Prices,
        rerun_share: f64,
    ) -> WorstCase {
        let hours = max_hours;
        let hosts = host_prices.len() as f64;
        let compute = num::sum(host_prices).to_f64() * hours;
        let ipv4 = prices.public_ipv4_per_hour * hosts * hours;
        let disk = prices.disk_per_host_hour * hosts * hours;
        let mut egress_gb = 0.0;
        for run in runs {
            if let Some((mbps, test_interval)) = run.home_downstream {
                egress_gb += mbps * 1e6 / 8.0 * test_interval / GB * 1.2;
            }
            egress_gb += run.download_gb * (1.0 + rerun_share);
        }
        let egress = egress_gb * prices.internet_egress_per_gb;
        let total = compute + ipv4 + disk + egress;
        WorstCase {
            compute: num::py_round(compute, 4),
            ipv4: num::py_round(ipv4, 4),
            disk: num::py_round(disk, 4),
            egress_gb: num::py_round(egress_gb, 3),
            egress: num::py_round(egress, 4),
            total: if total == 0.0 {
                num::py_round(total, 2)
            } else {
                num::py_round(total + 0.005, 2)
            },
        }
    }

    /// The dict the plan prints: its six figures, in the Python's order.
    pub fn to_json(&self) -> Json {
        Json::Dict(IndexMap::from([
            ("compute".to_string(), Json::Float(self.compute)),
            ("ipv4".to_string(), Json::Float(self.ipv4)),
            ("disk".to_string(), Json::Float(self.disk)),
            ("egress_gb".to_string(), Json::Float(self.egress_gb)),
            ("egress".to_string(), Json::Float(self.egress)),
            ("total".to_string(), Json::Float(self.total)),
        ]))
    }
}

/// The caps (`round.py:724-728`): the refusal where the worst case is over
/// the Round's cap, or with what the month has spent over the month's; the
/// Python's words, its floats as `repr` writes them.
pub fn check_caps(total: f64, round_cap: f64, spent: Num, month_cap: f64) -> Result<(), String> {
    if total > round_cap {
        return Err(format!(
            "the worst case, ${}, is over the round cap ${}",
            num::repr(total),
            num::repr(round_cap)
        ));
    }
    if spent.py_add(Num::Float(total)).to_f64() > month_cap {
        return Err(format!(
            "${} this month plus up to ${} is over the month cap ${}",
            num::fixed(spent.to_f64(), 2),
            num::repr(total),
            num::repr(month_cap)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRICES: Prices = Prices {
        public_ipv4_per_hour: 0.005,
        disk_per_host_hour: 0.002,
        internet_egress_per_gb: 0.10,
    };

    #[test]
    fn the_worst_case_counts_hosts_addresses_disk_and_egress_to_home() {
        // tests/test_round.py's Cost: two Tests to home, one each way, and
        // one between the cloud hosts; only the Downstream one to home is
        // paid egress, 250 Mbit/s for 10 s with 20 % margin.
        let runs = [
            RunTransfer {
                home_downstream: Some((250.0, 10.0)),
                download_gb: 0.0,
            },
            RunTransfer {
                home_downstream: None,
                download_gb: 0.0,
            },
            RunTransfer {
                home_downstream: None,
                download_gb: 0.0,
            },
        ];
        let cost = WorstCase::new(&runs, &[Num::Float(0.142); 2], 2.0, &PRICES, 0.15);
        assert_eq!(cost.compute, 0.568);
        assert_eq!(cost.ipv4, 0.02);
        assert_eq!(cost.disk, 0.008);
        assert_eq!(cost.egress_gb, 0.375);
        assert_eq!(cost.egress, 0.0375);
        // Rounded up to the cent.
        assert_eq!(cost.total, 0.64);
        let free = WorstCase::new(&[], &[Num::Int(0)], 2.0, &Prices::default(), 0.15);
        assert_eq!(free.total.to_bits(), 0.0f64.to_bits());
    }

    #[test]
    fn the_caps_refuse_in_the_pythons_words() {
        assert_eq!(check_caps(2.5, 3.0, Num::Int(0), 20.0), Ok(()));
        assert_eq!(
            check_caps(9.6, 9.0, Num::Int(0), 20.0).unwrap_err(),
            "the worst case, $9.6, is over the round cap $9.0"
        );
        assert_eq!(
            check_caps(2.5, 3.0, Num::Float(18.0), 20.0).unwrap_err(),
            "$18.00 this month plus up to $2.5 is over the month cap $20.0"
        );
    }
}
