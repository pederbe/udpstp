# Server log

The Server logs one line per event, covering what it admitted, refused and measured. Look up any event, field or reason here. [Hosting a Server](guides/hosting-a-server.md#read-the-log) shows how to set the log up.

Event names, field keys and the values below settle during 0.x and become stable from 1.0, as [Stable interfaces](interfaces.md) explains. New ones may appear, so ignore those you don't know.

## Format

The log is human-readable by default. A line longer than the terminal, or than 80 columns when redirected, continues on lines marked with `  | `.

```text
2026-11-10T17:45:21.532881Z INFO listening on 0.0.0.0:24601 event="Listening"
  | address=0.0.0.0:24601
2026-11-10T17:45:22.533499Z INFO Test Connection admitted
  | event="TestConnectionAdmitted" test_connection_id=1 client="127.0.0.1:45728"
  | mc_ident=3934 key_id=1
2026-11-10T17:45:22.533526Z INFO Test Connection activated
  | event="TestConnectionActivated" test_connection_id=1 direction="Downstream"
2026-11-10T17:45:33.074131Z INFO the Test Connection's Data Phase completed
  | event="DataPhaseEnded" test_connection_id=1 reason="Completed"
  | fast_paths="sendmmsg,UdpGso,recvmmsg,KernelRxTimestamps,epoll"
  | receive_buffer_overflows=0 burst_lateness_p99_us=409
2026-11-10T17:45:33.074158Z INFO Test ended: Completed event="TestSummary"
  | client=127.0.0.1:45728 mc_ident=3934 key_id=1 test_connections=1
  | direction="Downstream" test_interval=10 sub_interval=1000 trial_interval=50
  | low_thresh=30 upper_thresh=90 seq_err_thresh=10 slow_adj_thresh=3
  | high_speed_delta=10 ignore_ooo_dup=true one_way_delay_variation=true
  | random_payload=false rate_adj_algo="B" dscp=0 sr_index_conf=65535
  | sr_index_is_start=false bytes_sent=4089512844 bytes_received=4125335714
  | ip_layer_sender_bit_rate=3335136496 skipped_bursts=5 skipped_datagrams=102
  | skipped_bytes=145644 withheld_bursts=0 withheld_datagrams=0 withheld_bytes=0
  | receive_buffer_overflows=0 result="Completed"
```

With `log_format = "json"`, each line is one JSON object:

```json
{"timestamp":"2026-09-26T14:51:10.061951Z","level":"INFO","message":"Setup Request refused","event":"SetupRequestRejected","client":"anon-63d4269f2e4b0c0d","reason":"MaxBandwidthRequired"}
```

Every event has an `event` name and its data in fields. A field means one thing wherever it appears, and is left out when an event has nothing for it. The `message` is for people and may change, so filter on `event` and the fields.

In a terminal, the human log colours its level labels: INFO blue, WARN amber and ERROR red. `--color auto|always|never` controls this, as [Terminal color](interfaces.md#terminal-color) describes. JSON logs and log files are always plain.

## Where it goes

The log goes to standard output, and startup warnings and errors to standard error. With `log_file` set, the Server writes a new file each day named from it, so `/var/log/udpstp/server.log` gives `server.2026-10-01.log` (the date in UTC). It keeps the last 14, and copies its startup errors and warnings into them as `StartupError` and `StartupWarning`.

Give `log_file` a folder of its own: the Server deletes the oldest file whose name starts and ends as the log's does, whatever wrote it. A Windows service always logs to files. Elsewhere, leave the log to the service manager: logrotate's renames would fight the Server's own files, and SIGHUP stops the Server rather than reopening them.

Events an attacker can cause are rate-limited, ten of a kind every 10 s, so a flood can't fill the disk. The rest are counted in a `Suppressed` event.

## Fields

| Field | Type | What |
|---|---|---|
| `test_connection_id` | integer | The Test Connection, the same in each of its events; left out for the Standard Port |
| `client` | string | The Client's address, as `address_privacy` shows it |
| `mc_ident` | integer | The Test's mcIdent. With `client`, it identifies the Test |
| `key_id` | integer | The keyId of the Key Table row that authenticated the Test |
| `reason` | string | Why, a name from a fixed set ([below](#reasons)) |
| `count` | integer | How many times it happened since the last such event |
| `error` | string | The operating system's error, in its words: free text |
| `direction` | string | `Upstream` or `Downstream` |
| `parameter`, `requested`, `coerced` | string, integer, integer | An Activation parameter by its RFC 9946 name, the value the Client asked for, and the value the Server gave |
| `row` | integer | A Sending Rate Table row |
| `requested_bytes`, `receive_bytes`, `send_bytes` | integer | Socket buffer sizes: asked for, and what the OS gave |
| `bursts`, `datagrams`, `bytes` | integer | Bursts not sent, their Load PDUs, and their UDP payload bytes |
| `executor`, `engine_threads`, `grouping_us` | string, integer, integer | How the Data Phase Engines run ([`DataPhaseEngines`](#dataphaseengines)) |
| `fast_paths` | string | The fast paths a Test Connection used ([`DataPhaseEnded`](#dataphaseended)) |
| `receive_buffer_overflows` | integer | On Linux, datagrams the OS discarded at the socket before the Server read them, mostly for a full receive buffer |
| `send_lag_samples`, `send_lag_p50_us`, `send_lag_p99_us`, `send_lag_max_us`, `burst_lateness_p99_us` | integer | Experimental send timing, in µs ([`DataPhaseEnded`](#dataphaseended)) |

## Events

### Starting and stopping

| Event | When | Fields |
|---|---|---|
| `DataPhaseEngines` | At start: how the Data Phase Engines run ([below](#dataphaseengines)) | `executor`, `grouping_us`, `engine_threads`, `error` |
| `Listening` | The Server listens on an address | `address` |
| `NoIpv6Listener` | The host has no IPv6, so the Server listens on IPv4 alone | `error` |
| `TroubleshootingModeOn` | At start, and every minute while Troubleshooting Mode is on | |
| `StartupError` | With `log_file` set: the Server couldn't start | `error`, the message (free text); `exit_status` |
| `StartupWarning` | With `log_file` set: a warning at startup | `reason`: `DscpAskedAsZero`, `DscpCoercedToZero`, `DontFragmentUnsupported`, `AdmittedRateCapWithoutMaxBandwidth`, `NoTestAdmissible`, `TroubleshootingModeOn`, `SocketBuffersShort`, `SocketBuffersUnknown`, `MaxHopsNotSet`, `TestsPerSource` or `KeyTableFormatDeprecated` |
| `Stopping` | The Server was asked to stop. It ignores new Setup Requests, sends STOP to its active Test Connections and ends them | `reason`: `SIGTERM`, `SIGINT` or `SIGHUP`; on Windows `CtrlC`, `CtrlBreak`, `CtrlClose` or `CtrlShutdown`; as a Windows service `ServiceStop` or `ServiceShutdown` |

### Setup and Activation

| Event | When | Fields |
|---|---|---|
| `TestConnectionAdmitted` | A Setup Request was accepted | `test_connection_id`, `client`, `mc_ident`, and `key_id` in an authenticated mode |
| `SetupRequestRejected` | A Setup Request was refused with a Setup Response | `client`, `reason` |
| `SetupRequestSilentlyRejected` | A Setup Request was refused without a response (Silent Rejection) | `client`, `reason` |
| `SetupRequestIgnored` | A Setup Request was ignored: `DuplicateSetupRequest`, a copy of one already answered; `ArrivalAddressUnknown`, which the Client retries; or `ServerStopping` | `client`, `reason` |
| `TestConnectionActivated` | An Activation Request was accepted | `test_connection_id`, `direction` |
| `ActivationParameterCoerced` | The Activation Response changed a parameter | `test_connection_id`, `parameter`, `requested`, `coerced` |
| `ActivationRequestRejected` | An Activation Request was refused | `test_connection_id`, `reason`, and `parameter` for `UnacceptableParameter` |
| `ActivationRequestSilentlyRejected` | An Activation Request was refused without a response | `test_connection_id`, `reason` |
| `ActivationWatchdogExpired` | No valid Activation Request came within 3 s | `test_connection_id` |
| `UnactivatedTestConnectionEvicted` | The Unactivated Test Connections filled their share, so the oldest of the Key Table row holding the most was closed to make room | `test_connection_id` |

### The Data Phase

| Event | When | Fields |
|---|---|---|
| `FirstLoadPduReceived`, `FirstStatusPduReceived`, `StopReceived` | The first Load PDU or Status PDU, and the STOP exchange | `test_connection_id` |
| `BurstsSkipped` | This host fell behind sending load, so the Server, not the path, limited the Test | `test_connection_id`, `bursts`, `datagrams`, `bytes` |
| `BurstsWithheld` | The send buffer was full | `test_connection_id`, `bursts`, `datagrams`, `bytes` |
| `InvalidLoadPdus`, `IgnoredLoadPdus`, `InvalidStatusPdus`, `StaleStatusPdus`, `StatusPdusFailedVerification`, `StatusPdusMissing`, `PeerMissedStatusPdus`, `PeerRxStopped`, `DelaySamplesDropped`, `IcmpErrors` | PDUs ignored or missing, and ICMP errors, counted | `test_connection_id`, `count` |
| `PathMtuExceeded`, `HopLimitExceeded` | Datagrams too large for the path with DF set, or expired on the way because `max_hops` is too small | `test_connection_id`, `count` |
| `LostStatusBackoff` | No Status PDU came in time, so a Downstream Search Test backed off as for an Errored Interval | `test_connection_id`, `row` |
| `TrafficStopped` | Nothing arrived for 1 s, so this end stopped sending | `test_connection_id` |
| `DataPhaseEnded` | A Test Connection's Data Phase ended ([below](#dataphaseended)) | `test_connection_id`, `reason`, `fast_paths`, `receive_buffer_overflows`, `error` |
| `TestConnectionReleased` | A Test Connection ended and its Admission was released | `test_connection_id` |
| `TestSummary` | A Test ended ([below](#testsummary)) | |

### Problems and counts

| Event | When | Fields |
|---|---|---|
| `SocketError`, `NoEphemeralPort` | A socket failed, or no Ephemeral Port was free | `test_connection_id` (none for the Standard Port), `error` |
| `SocketBuffersShort` | The OS held a Test Connection's socket buffers below `socket_buffer_size` | `test_connection_id`, `requested_bytes`, `receive_bytes`, `send_bytes` |
| `DatagramsDropped` | The Server's Control Phase fell behind and discarded datagrams its socket readers had received. This is udpstp's own queue, not a Receive Buffer Overflow | `test_connection_id` (none for the Standard Port), `count` |
| `Statistics`, `StatisticsRefusals` | Every `statistics_interval` seconds, and once more when the Server stops ([below](#statistics)) | |
| `Suppressed` | Events of one kind beyond ten every 10 s, counted rather than logged | `suppressed_event`, `count`, and the sums of what they carried: `bursts`, `datagrams` and `bytes`, or `suppressed_count` for events with a `count` |

### DataPhaseEngines

`executor` is `pool` on Linux, where Engine Threads each serve many Test Connections, or `per_test_connection` on Windows and macOS, where each Test Connection has an Engine Thread of its own.

For `pool`, `engine_threads` is the most it starts: one for each CPU the process may use, with a fractional cgroup CPU quota rounded down, so a container limited to 1.9 CPUs gets 1. `error` is a warning when the CPUs the process may use couldn't be found.

`grouping_us` is the pool's wake-up grouping window, 0 by default and for `per_test_connection`. The experimental variables `UDPSTP_ENGINE_THREADS` and `UDPSTP_ENGINE_GROUPING_US` override `engine_threads` and `grouping_us`.

### DataPhaseEnded

`reason` says how the Data Phase ended ([below](#reasons)). `error` names the failure when the Data Phase Engine failed.

`fast_paths` lists, comma-separated, the fast paths the engine sent Load PDUs, read datagrams and waited with. It is empty except on Linux, where it can hold:

- `sendmmsg` and `UdpGso` when the Server sent Load PDUs (not in an Upstream Test);
- `recvmmsg` and `KernelRxTimestamps`;
- `epoll` when one of the Server's Engine Threads served the Test Connection.

With the experimental `UDPSTP_KERNEL_TX_TIMESTAMPS=1` on Linux 6.13 and later, each message goes with a `sendmsg` call of its own, so `UdpGso` appears without `sendmmsg`, unless the kernel refuses the timestamps. A Load Sender's event then adds `send_lag_samples`, `send_lag_p50_us`, `send_lag_p99_us` and `send_lag_max_us`: how much later than its lpduTime the kernel sent the Load PDUs it stamped. `burst_lateness_p99_us`, also experimental, is how late a Load Sender wrote its Send Plans, 99th percentile.

`receive_buffer_overflows`, on Linux, counts datagrams the OS discarded at the socket before the Server read them. In an Upstream Test the Client counts them as lost.

### TestSummary

A `TestSummary` has:

- `client`, `mc_ident`, `key_id` (in an authenticated mode) and `test_connections`;
- `bytes_sent` and `bytes_received`, the UDP payload bytes of the Load PDUs the Server sent and received;
- `ip_layer_sender_bit_rate`, in bit/s, when the Server was the Load Sender;
- what the Server didn't send as Load Sender, over all its Test Connections: `skipped_bursts`, `skipped_datagrams` and `skipped_bytes` when it fell behind, and `withheld_bursts`, `withheld_datagrams` and `withheld_bytes` when the send buffer was full;
- `receive_buffer_overflows`, on Linux, summed over its Test Connections;
- `result`: `Completed`; `ServerStopping` for a Test cut short when the Server stopped; how the first Test Connection that didn't complete ended (a `DataPhaseEnded` reason, `ActivationWatchdogExpired`, `UnactivatedTestConnectionEvicted`, or `ActivationRequestRejected` with its `reason`); or `NotActivated`.

A Test that was activated adds its `direction` and parameters: `test_interval` (s), `sub_interval` and `trial_interval` (ms), `low_thresh` and `upper_thresh` (ms), `seq_err_thresh`, `slow_adj_thresh`, `high_speed_delta`, `ignore_ooo_dup`, `one_way_delay_variation` and `random_payload` (booleans), `rate_adj_algo` (`B` or `C`), `dscp`, `sr_index_conf` and `sr_index_is_start`.

### Statistics

`Statistics` counts since the Server started: `tests`, `test_connections`, `active_test_connections`, `authentication_failures` (requests, and Status PDUs in mode 2), `rate_limited` (Setup Requests over a budget), `stale_status_pdus`, `setup_requests_ignored` and `unactivated_test_connections_evicted`.

It has a `report` number. One `StatisticsRefusals` line with the same `report` follows for each kind of refusal counted so far, with `kind` (the event that logged it), its `reason` and `count`. The counts overlap: a request refused for its authentication counts in `authentication_failures` and in its `StatisticsRefusals`, and one over a budget in `rate_limited` too.

## Reasons

- **For `DataPhaseEnded`**, how it ended: `Completed`, `Unconfirmed` (the Client didn't confirm the STOP), `StoppedByClient`, `Watchdog`, `OverRate` (the Client sent above 1.5 times its rate), `Stopped` (the Server stopped it) or `DataPhaseEngineFailed`.
- **For a Setup Request**: `TestsPerSource`, `TestConnectionsPerTest`, `TestConnections`, `MaxBandwidthRequired`, `CapacityExceeded`, `SocketUnavailable`, `QuotaExceeded`, `SourceBudgetExhausted`, `BadVersion`, `JumboMismatch`, `TraditionalMtuMismatch`, `Malformed`, `MulticastOrBroadcast`, `ChecksumFailed`, `Replayed`, `ReplayCacheFull`, `UnverifiedBudgetExhausted`, or a Multi-Connection field's fault: `MultiConnectionIdentZero`, `MultiConnectionIndexNotBelowCount`, `MultiConnectionIndexInUse`, `MultiConnectionCountDiffers`, `MultiConnectionAuthModeDiffers`, `MultiConnectionKeyIdDiffers`.
- **For an Activation Request**: `InvalidCommand`, `MixedDirection`, `UnacceptableParameter`, `FixedRateNeedsOperatorKey`, `TestsPerSource`, `TestConnections`, `MaxBandwidthDirection`, `CapacityExceeded`, `QuotaExceeded`, `AuthModeMismatch`, `KeyIdMismatch`, `MulticastOrBroadcast`, `ChecksumFailed`, `Replayed`, `ReplayCacheFull`, `NotFromClient`, `Malformed`.
- **For an authentication failure**, whether Troubleshooting Mode answered it or not: `AuthenticationRequired`, `AuthenticationNotConfigured`, `AuthenticationModeInvalid`, `AuthenticationUnknownKeyId`, `AuthenticationKeyOutsideLifetime`, `AuthenticationDigestFailed`, `AuthenticationTimeOutOfWindow`.
