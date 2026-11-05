//! The Server's Sending Rate Table (RFC 9946 §6.1, §7.2.2; TR-471 Annex A).
//!
//! Row N offers N Mbps at the IP layer for rows 1 to 1000, the mapping
//! OB-UDPST Clients and the TR-471 defaults assume
//!. Rows 1001 to 1152 offer
//! OB-UDPST's rates above 1 Gbps for the Server's CHSR_JUMBO_STATUS and
//! CHSR_TRADITIONAL_MTU settings (§2.4), so a row means the same rate to
//! any Client. Payload sizes are IPv4-based (ADR 0004).

use udpstp_wire::SendingRate;

/// IPv4 and UDP header bytes.
const IPV4_UDP_HEADERS: u32 = 28;

/// Largest IP packet up to 1 Gbps (RFC 9946 §6.1): 1250 bytes, or 1500
/// with CHSR_TRADITIONAL_MTU.
const MAX_IP_PACKET: u32 = 1250;
const MAX_TRADITIONAL_IP_PACKET: u32 = 1500;

/// Largest IP packet above 1 Gbps with CHSR_JUMBO_STATUS (RFC 9946 §6.1).
pub const MAX_JUMBO_IP_PACKET: u32 = 9000;

/// The first transmitter's interval: the RFC's recommended 100 µs
/// granularity (§7.1). One 1250-byte datagram per interval is 100 Mbps.
const TX1_INTERVAL_US: u32 = 100;

/// The second transmitter's interval. One 1250-byte datagram per interval
/// is 10 Mbps.
const TX2_INTERVAL_US: u32 = 1000;

/// IP bytes per second-transmitter interval that make 1 Mbps.
const ADD_ON_BYTES_PER_MBPS: u32 = TX2_INTERVAL_US / 8;

/// The most datagrams in a burst (OB-UDPST's, which its Clients check).
const MAX_BURST: u32 = 100;

/// The row offering 1 Gbps: Fast Mode's cap.
pub(crate) const GIGABIT_ROW: u16 = 1000;

/// The table's last row, as in OB-UDPST.
pub(crate) const LAST_ROW: u16 = 1152;

/// Row 0 sends one 625-byte IP packet every 50 ms: 0.1 Mbps, close to the
/// average of OB-UDPST's random-size row 0, but always the same size.
const ROW_ZERO_INTERVAL_US: u32 = 50_000;
const ROW_ZERO_IP_PACKET: u32 = 625;

/// The Server's Setup modifiers and Server MTU, which shape its table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableShape {
    /// CHSR_JUMBO_STATUS: packets above 1 Gbps may be jumbo-sized.
    pub jumbo: bool,
    /// CHSR_TRADITIONAL_MTU: packets may be 1500 bytes, not 1250.
    pub traditional_mtu: bool,
    /// The largest IP packet the Server's path takes, 1280 to 9000 bytes:
    /// a Jumbo row above it keeps its rate with more, smaller packets.
    pub mtu: u32,
}

impl Default for TableShape {
    fn default() -> Self {
        Self {
            jumbo: true,
            traditional_mtu: false,
            mtu: MAX_TRADITIONAL_IP_PACKET,
        }
    }
}

#[derive(Debug)]
pub(crate) struct SendingRateTable {
    rows: Vec<SendingRate>,
    /// Each row's IP-layer rate over IPv4, in kbit/s.
    rates: Vec<u64>,
    /// The first row at the top rate: the rows after it repeat it, so the
    /// Server goes no higher, or a search that climbed into them would
    /// take seconds of single steps to come down.
    top: u16,
}

impl SendingRateTable {
    /// The table for `shape`: rows 1 to 1000 at N Mbps each, then
    /// OB-UDPST's rows above 1 Gbps, fitted to the MTU.
    pub(crate) fn new(shape: TableShape) -> Self {
        let row_zero = SendingRate {
            tx_interval2: ROW_ZERO_INTERVAL_US,
            udp_addon2: ROW_ZERO_IP_PACKET - IPV4_UDP_HEADERS,
            ..SendingRate::default()
        };
        let mut rows = vec![row_zero];
        for mbps in 1..=u32::from(GIGABIT_ROW) {
            rows.push(if shape.traditional_mtu {
                traditional_row(mbps)
            } else {
                mbps_row(mbps)
            });
        }
        // Traditional MTU's rows up to 1 Gbps need 1500 bytes anyway.
        let floor = if shape.traditional_mtu {
            MAX_TRADITIONAL_IP_PACKET
        } else {
            MAX_IP_PACKET
        };
        let mtu = shape.mtu.clamp(floor, MAX_JUMBO_IP_PACKET);
        for index in u32::from(GIGABIT_ROW) + 1..=u32::from(LAST_ROW) {
            let (rate, packet) = high_rate(shape, index);
            rows.push(fitted_row(rate, packet.min(mtu)));
        }
        let rates: Vec<u64> = rows.iter().map(kbps).collect();
        let highest = rates.iter().copied().max().unwrap_or(0);
        let top = rates.iter().position(|&r| r == highest).unwrap_or(0);
        Self {
            rows,
            rates,
            top: u16::try_from(top).unwrap_or(LAST_ROW),
        }
    }

    /// The row at `index`, if the table has one.
    pub(crate) fn row(&self, index: u16) -> Option<SendingRate> {
        self.rows.get(usize::from(index)).copied()
    }

    /// The highest row whose rate is at most `mbps` Mbps (at least row 1),
    /// so the Server never sends above a maxBandwidth: row N offers N Mbps
    /// up to row 1000, with IPv4 headers (ADR 0004); over IPv6 it sends
    /// about 1.6% more.
    pub(crate) fn row_for_mbps(&self, mbps: u32) -> u16 {
        let cap = u64::from(mbps) * 1000;
        let count = self.rates[1..=usize::from(self.top)]
            .iter()
            .take_while(|&&r| r <= cap)
            .count();
        u16::try_from(count.max(1)).unwrap_or(self.top)
    }

    /// The highest row the Server uses: the first at the table's top rate.
    /// A higher one asked for is coerced to it (RFC 9946 §7.2.1).
    pub(crate) fn last_index(&self) -> u16 {
        self.top
    }
}

/// The least sending rate row `row` offers from any Server with these
/// Setup modifiers, in bit/s at the IP layer, or `None` past the table's
/// last row: its rate fitted to the smallest packets the modifiers allow,
/// since a Server MTU fits rows above 1 Gbps to smaller packets, and caps
/// the fastest, but never raises one. Rows up to 1 Gbps offer N Mbps
/// whatever the MTU. The rate is the same over IPv4 and IPv6, whose Load
/// Sender takes 20 bytes off every size (ADR 0004). A Client compares a
/// Fixed-Rate row's with what the Server was seen to send.
pub fn least_sending_rate(jumbo: bool, traditional_mtu: bool, row: u16) -> Option<u64> {
    let table = SendingRateTable::new(TableShape {
        jumbo,
        traditional_mtu,
        // Clamped up to the smallest packet the modifiers allow.
        mtu: 0,
    });
    table
        .rates
        .get(usize::from(row))
        .map(|kbps| kbps.saturating_mul(1000))
}

/// The rate of the highest row at or below a maxBandwidth of `mbps` Mbps,
/// in bit/s, as the rows are rated with these Setup modifiers: where a
/// Server's search stops. Row N is N Mbps up to row 1000, and the
/// rows above are OB-UDPST's, 100 Mbps apart up to 7.2 Gbps and 720 Mbps
/// apart beyond with Jumbo rows, 100 Mbps apart without, 120 with
/// Traditional MTU. A Server whose MTU fits a row to smaller packets
/// offers it at most 1% below its rate, and caps its fastest at its top.
/// `None` where no row is rated above `mbps`: the table tops out within it.
pub(crate) fn highest_rate_within(jumbo: bool, traditional_mtu: bool, mbps: u32) -> Option<u64> {
    let shape = TableShape {
        jumbo,
        traditional_mtu,
        mtu: MAX_JUMBO_IP_PACKET,
    };
    let rate = |index: u32| match index {
        index if index <= u32::from(GIGABIT_ROW) => index,
        index => high_rate(shape, index).0,
    };
    let rows = 1..=u32::from(LAST_ROW);
    if rows.clone().all(|index| rate(index) <= mbps) {
        return None;
    }
    rows.map(rate)
        .take_while(|&r| r <= mbps)
        .last()
        .map(|r| u64::from(r) * 1_000_000)
}

/// The top rate of a table whose fastest rows a Server's MTU fits to IP
/// packets of `ip_packet` bytes: a burst of the most, 100, every 100 µs
/// on the first Transmitter, in bit/s. 12 Gbps at the default 1500 bytes,
/// 72 Gbps with 9000-byte Jumbo packets, 10 Gbps with 1250 bytes.
pub(crate) fn top_rate_with_packets(ip_packet: u64) -> u64 {
    ip_packet * 8 * u64::from(MAX_BURST) * 1_000_000 / u64::from(TX1_INTERVAL_US)
}

/// The row offering `mbps` Mbps, for 1 to 1000: hundreds of Mbps as
/// full-size datagrams on the first transmitter, tens on the second, and the
/// units as one add-on datagram per second-transmitter interval.
fn mbps_row(mbps: u32) -> SendingRate {
    let payload = MAX_IP_PACKET - IPV4_UDP_HEADERS;
    split_row(mbps / 100, mbps / 10 % 10, mbps % 10, payload)
}

/// The same with 1500-byte datagrams (CHSR_TRADITIONAL_MTU): 120 Mbps a
/// first-transmitter datagram, 12 a second's, and add-ons of 1 to 11 Mbps,
/// as OB-UDPST builds it.
fn traditional_row(mbps: u32) -> SendingRate {
    let payload = MAX_TRADITIONAL_IP_PACKET - IPV4_UDP_HEADERS;
    split_row(mbps / 120, mbps % 120 / 12, mbps % 12, payload)
}

fn split_row(first: u32, second: u32, add_on_mbps: u32, payload: u32) -> SendingRate {
    let mut row = SendingRate::default();
    if first > 0 {
        row.tx_interval1 = TX1_INTERVAL_US;
        row.udp_payload1 = payload;
        row.burst_size1 = first;
    }
    if second > 0 {
        row.tx_interval2 = TX2_INTERVAL_US;
        row.udp_payload2 = payload;
        row.burst_size2 = second;
    }
    if add_on_mbps > 0 {
        row.tx_interval2 = TX2_INTERVAL_US;
        row.udp_addon2 = add_on_mbps * ADD_ON_BYTES_PER_MBPS - IPV4_UDP_HEADERS;
    }
    row
}

/// Row `index` above 1 Gbps: its rate in Mbps and the IP packet size
/// OB-UDPST sends it with.
/// Jumbo: 1375 to 9000-byte packets ten at a time (1100 to 7200 Mbps),
/// then 9000-byte bursts of 11 to 100 (7920 to 72000 Mbps). Without
/// jumbo: bursts of 11 to 100 1250-byte packets, or 9 to 100 1500-byte
/// ones with CHSR_TRADITIONAL_MTU, then the top rate repeated.
/// (conformance: table-steps-above-1-gbps)
fn high_rate(shape: TableShape, index: u32) -> (u32, u32) {
    let step = index - u32::from(GIGABIT_ROW);
    let (packet, burst) = match (shape.jumbo, shape.traditional_mtu) {
        (true, _) if step <= 62 => (1250 + 125 * step, 10),
        (true, _) => (MAX_JUMBO_IP_PACKET, step - 52),
        (false, false) => (MAX_IP_PACKET, (step + 10).min(MAX_BURST)),
        (false, true) => (MAX_TRADITIONAL_IP_PACKET, (step + 8).min(MAX_BURST)),
    };
    (rate_mbps(packet, burst), packet)
}

/// The IP-layer rate of `burst` packets of `packet` bytes every 100 µs.
fn rate_mbps(packet: u32, burst: u32) -> u32 {
    packet * burst * 8 / TX1_INTERVAL_US
}

/// A row offering `rate` Mbps with packets of at most `packet` bytes, for
/// a Server whose MTU is below OB-UDPST's packet: as few packets on the
/// first transmitter as reach the rate, at most [`MAX_BURST`], so a rate a
/// burst of the largest can't reach is capped; each of the same size,
/// rounded down, with what that leaves out sent as an add-on on the second
/// transmitter when it makes a Load PDU no bigger than the others. A row
/// never offers more than its rate.
fn fitted_row(rate: u32, packet: u32) -> SendingRate {
    // IP bytes per 100 µs interval.
    let bytes = rate * TX1_INTERVAL_US / 8;
    let burst = bytes.div_ceil(packet).clamp(1, MAX_BURST);
    let size = (bytes / burst).min(packet);
    let mut row = SendingRate {
        tx_interval1: TX1_INTERVAL_US,
        udp_payload1: size - IPV4_UDP_HEADERS,
        burst_size1: burst,
        ..SendingRate::default()
    };
    // Left out, per second-transmitter interval.
    let rest = bytes.saturating_sub(burst * size) * (TX2_INTERVAL_US / TX1_INTERVAL_US);
    if (MIN_ADD_ON_IP_PACKET..=size).contains(&rest) {
        row.tx_interval2 = TX2_INTERVAL_US;
        row.udp_addon2 = rest - IPV4_UDP_HEADERS;
    }
    row
}

/// The smallest add-on: a Load PDU header and the 20 bytes an IPv6 sender
/// takes off, with IPv4 and UDP headers (ADR 0004).
const MIN_ADD_ON_IP_PACKET: u32 = 80;

/// A row's IP-layer rate over IPv4, in kbit/s.
fn kbps(row: &SendingRate) -> u64 {
    let per = |interval: u32, bytes: u64| match interval {
        0 => 0,
        interval => bytes * 8000 / u64::from(interval),
    };
    let packets = |payload: u32, count: u32| match payload {
        0 => 0,
        payload => u64::from(payload + IPV4_UDP_HEADERS) * u64::from(count),
    };
    per(row.tx_interval1, packets(row.udp_payload1, row.burst_size1))
        + per(
            row.tx_interval2,
            packets(row.udp_payload2, row.burst_size2) + packets(row.udp_addon2, 1),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row's IP-layer rate over IPv4, in Mbps.
    fn rate(row: &SendingRate) -> u32 {
        let per = |interval: u32, bytes: u32| match interval {
            0 => 0,
            _ => bytes * 8 / interval,
        };
        per(
            row.tx_interval1,
            (row.udp_payload1 + IPV4_UDP_HEADERS) * row.burst_size1,
        ) + per(
            row.tx_interval2,
            (row.udp_payload2 + IPV4_UDP_HEADERS) * row.burst_size2,
        ) + per(
            row.tx_interval2,
            if row.udp_addon2 > 0 {
                row.udp_addon2 + IPV4_UDP_HEADERS
            } else {
                0
            },
        )
    }

    fn largest_packet(row: &SendingRate) -> u32 {
        [row.udp_payload1, row.udp_payload2, row.udp_addon2]
            .into_iter()
            .filter(|&p| p > 0)
            .max()
            .unwrap_or(0)
            + IPV4_UDP_HEADERS
    }

    fn shapes() -> Vec<TableShape> {
        let mut shapes = Vec::new();
        for jumbo in [true, false] {
            for traditional_mtu in [false, true] {
                for mtu in [1500, 9000] {
                    shapes.push(TableShape {
                        jumbo,
                        traditional_mtu,
                        mtu,
                    });
                }
            }
        }
        shapes
    }

    // IP packet sizes by CHSR_JUMBO_STATUS and CHSR_TRADITIONAL_MTU;
    #[udpstp_req::req(
        "RFC9946-6.1-8",
        "RFC9946-6.1-9",
        "RFC9946-6.1-12",
        "RFC9097-8.1-L3",
        "RFC9946-5-L1"
    )]
    #[test]
    fn packet_sizes_follow_the_modifiers_and_the_mtu() {
        for shape in shapes() {
            let table = SendingRateTable::new(shape);
            assert_eq!(table.rows.len(), usize::from(LAST_ROW) + 1, "{shape:?}");
            let low = if shape.traditional_mtu { 1500 } else { 1250 };
            for index in 0..=LAST_ROW {
                let row = table.row(index).unwrap();
                let largest = largest_packet(&row);
                let limit = if index <= GIGABIT_ROW || !shape.jumbo {
                    low
                } else {
                    MAX_JUMBO_IP_PACKET.min(shape.mtu).max(low)
                };
                assert!(
                    largest <= limit,
                    "{shape:?} row {index}: {largest} > {limit}"
                );
                assert!(row.burst_size1 <= MAX_BURST && row.burst_size2 <= MAX_BURST);
            }
        }
    }

    // : row N is N Mbps up to 1 Gbps in every variant, and the rows
    // above are OB-UDPST's rates, never falling
    #[udpstp_req::req("RFC9097-8.1-2", "RFC9097-8.1-6")]
    #[test]
    fn rows_offer_their_rates() {
        for shape in shapes() {
            let table = SendingRateTable::new(shape);
            for index in 1..=GIGABIT_ROW {
                assert_eq!(
                    rate(&table.row(index).unwrap()),
                    u32::from(index),
                    "{shape:?}"
                );
            }
            // Above: never more than OB-UDPST's rate, within 1 % of it up to
            // the top, and never falling.
            let mut last = u64::from(GIGABIT_ROW) * 1000;
            for index in GIGABIT_ROW + 1..=LAST_ROW {
                let offered = table.rates[usize::from(index)];
                let (nominal, _) = high_rate(shape, u32::from(index));
                let nominal = u64::from(nominal) * 1000;
                assert!(offered >= last, "{shape:?} row {index}: {offered} < {last}");
                assert!(
                    offered <= nominal,
                    "{shape:?} row {index}: {offered} > {nominal}"
                );
                if index < table.last_index() {
                    assert!(
                        offered * 100 >= nominal * 99,
                        "{shape:?} row {index}: {offered}"
                    );
                }
                last = offered;
            }
        }
        // OB-UDPST's Jumbo rows: 1100 Mbps at row 1001, 7200 at 1062, 72
        // Gbps at 1152, with 9000-byte packets when the MTU takes them.
        let jumbo = SendingRateTable::new(TableShape {
            mtu: 9000,
            ..TableShape::default()
        });
        let at = |i: u16| rate(&jumbo.row(i).unwrap());
        assert_eq!(
            (at(1001), at(1062), at(1063), at(1152)),
            (1100, 7200, 7920, 72_000)
        );
        assert_eq!(largest_packet(&jumbo.row(1152).unwrap()), 9000);
        // With a 1500-byte MTU, 7200 Mbps as 60 1500-byte packets, and the
        // top a burst of 100 of them: 12 Gbps.
        let fitted = SendingRateTable::new(TableShape::default());
        let row = fitted.row(1062).unwrap();
        assert_eq!((row.burst_size1, largest_packet(&row)), (60, 1500));
        assert_eq!(rate(&fitted.row(1152).unwrap()), 12_000);
    }

    // : the least a row offers from any Server: N Mbps up to 1 Gbps,
    // and above it the rate fitted to the smallest packets, which caps the
    // fastest rows (10 Gbps as bursts of 100 1250-byte packets)
    #[test]
    fn a_rows_least_sending_rate() {
        assert_eq!(least_sending_rate(true, false, 0), Some(100_000));
        assert_eq!(least_sending_rate(true, false, 1), Some(1_000_000));
        assert_eq!(least_sending_rate(true, false, 875), Some(875_000_000));
        assert_eq!(least_sending_rate(false, true, 1000), Some(1_000_000_000));
        // 1100 Mbps as ten 1375-byte packets, or eleven of 1250 bytes.
        assert_eq!(least_sending_rate(true, false, 1001), Some(1_100_000_000));
        assert_eq!(least_sending_rate(true, false, 1152), Some(10_000_000_000));
        // Never above what a Server with a larger MTU offers.
        let jumbo = SendingRateTable::new(TableShape {
            mtu: 9000,
            ..TableShape::default()
        });
        for row in [1001, 1062, 1100, 1152] {
            let least = least_sending_rate(true, false, row).unwrap();
            assert!(least <= jumbo.rates[usize::from(row)] * 1000, "row {row}");
        }
        assert_eq!(least_sending_rate(true, false, LAST_ROW + 1), None);
    }

    // : where a search stops under a maxBandwidth, the highest row at or
    // below it: N Mbps up to 1 Gbps; above it OB-UDPST's rows, 1100 to 7200
    // Mbps 100 apart then 7920, 8640, 9360, 10080 with Jumbo rows, 1100 to
    // 10000 100 apart without, 1080, 1200, 1320 with Traditional MTU;
    // none where the table tops out within it
    #[test]
    fn the_highest_rate_within_a_max_bandwidth() {
        let within = |jumbo, traditional_mtu, mbps| {
            highest_rate_within(jumbo, traditional_mtu, mbps).map(|bps| bps / 1_000_000)
        };
        assert_eq!(within(true, false, 200), Some(200));
        assert_eq!(within(true, false, 1000), Some(1000));
        assert_eq!(within(true, false, 1099), Some(1000));
        assert_eq!(within(true, false, 1190), Some(1100));
        assert_eq!(within(true, false, 7200), Some(7200));
        assert_eq!(within(true, false, 7919), Some(7200));
        assert_eq!(within(true, false, 8500), Some(7920));
        assert_eq!(within(true, false, 10_000), Some(9360));
        assert_eq!(within(false, false, 8500), Some(8500));
        assert_eq!(within(false, false, 10_000), None);
        assert_eq!(within(false, true, 1190), Some(1080));
        assert_eq!(within(false, true, 1300), Some(1200));
        assert_eq!(within(false, true, 12_000), None);
        assert_eq!(within(true, false, 72_000), None);
    }

    // : a table's top rate follows from the packets its fastest rows
    // are fitted to: 12 Gbps at the default MTU, 72 with Jumbo packets up
    // to 9000 bytes, 10 without Jumbo rows, 12 with Traditional MTU
    #[test]
    fn the_top_rate_follows_the_packets_of_the_fastest_rows() {
        let shapes = [
            (TableShape::default(), 1500),
            (
                TableShape {
                    mtu: 9000,
                    ..TableShape::default()
                },
                9000,
            ),
            (
                TableShape {
                    jumbo: false,
                    ..TableShape::default()
                },
                1250,
            ),
            (
                TableShape {
                    jumbo: false,
                    traditional_mtu: true,
                    mtu: 9000,
                },
                1500,
            ),
        ];
        for (shape, packet) in shapes {
            let table = SendingRateTable::new(shape);
            let top = table.row(table.last_index()).unwrap();
            assert_eq!(largest_packet(&top), packet, "{shape:?}");
            assert_eq!(
                top_rate_with_packets(u64::from(packet)),
                u64::from(rate(&top)) * 1_000_000,
                "{shape:?}"
            );
        }
    }

    // : a rate cap finds the highest row within it, above 1 Gbps too
    #[test]
    fn a_rate_finds_its_row() {
        let table = SendingRateTable::new(TableShape {
            mtu: 9000,
            ..TableShape::default()
        });
        assert_eq!(table.row_for_mbps(1), 1);
        assert_eq!(table.row_for_mbps(0), 1);
        assert_eq!(table.row_for_mbps(875), 875);
        assert_eq!(table.row_for_mbps(1150), 1001);
        assert_eq!(table.row_for_mbps(u32::MAX), LAST_ROW);
    }
}
