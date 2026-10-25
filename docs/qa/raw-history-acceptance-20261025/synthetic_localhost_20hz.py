"""Explicit SYNTHETIC localhost pacing test; never an original acquisition.

`send` repeats unmodified UDP payloads extracted from an actual PCAP at a new,
synthetic schedule. Only the explicit send subcommand creates a UDP socket.
`inspect` and `verify` never send traffic. No external host or device is used.
"""
import argparse
import datetime
import hashlib
import ipaddress
import json
import math
from pathlib import Path
import socket
import statistics
import sys
import time

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / 'tools/qa'))
from lvds_raw_recovery_reference import read_lvdsraw, read_pcap_udp


def file_info(path):
    path = Path(path)
    data = path.read_bytes()
    return {'path': str(path.resolve()), 'bytes': len(data),
            'sha256': hashlib.sha256(data).hexdigest()}


def loopback_destination(value):
    try:
        host, port = value.rsplit(':', 1)
        address = ipaddress.IPv4Address(host)
        port = int(port)
    except (ValueError, TypeError) as error:
        raise argparse.ArgumentTypeError('destination must be a literal loopback IPv4:port') from error
    if not address.is_loopback or not 1 <= port <= 65535:
        raise argparse.ArgumentTypeError('destination must be loopback IPv4 with port 1..65535')
    return str(address), port


def positive_count(value):
    count = int(value)
    if count <= 0:
        raise argparse.ArgumentTypeError('count must be positive')
    return count


def positive_hz(value):
    hz = float(value)
    if not math.isfinite(hz) or hz <= 0:
        raise argparse.ArgumentTypeError('hz must be finite and positive')
    return hz


def endpoint(value):
    return f'{value[0]}:{value[1]}'


def source_records(path):
    records, summary = read_pcap_udp(path)
    if len(records) != 467 or summary['packet_diagnostics'] or not summary['clean_pcap_eof']:
        raise ValueError(f'expected this actual clean 467-UDP PCAP: {summary}')
    if any(record['capture_truncated'] for record in records):
        raise ValueError('source PCAP contains captured truncation')
    if any(not 0 < len(record['payload']) <= 65507 for record in records):
        raise ValueError('source payload cannot be sent unchanged as one IPv4 UDP datagram')
    return records, summary


def timing_summary(records, started_ns, ended_ns):
    sent_times = [record['send_before_monotonic_ns'] for record in records]
    intervals = [(right - left) / 1_000_000 for left, right in zip(sent_times, sent_times[1:])]
    late_ms = [(record['send_before_monotonic_ns'] - record['scheduled_monotonic_ns']) / 1_000_000
               for record in records]
    return {
        'started_monotonic_ns': started_ns, 'ended_monotonic_ns': ended_ns,
        'elapsed_seconds': (ended_ns - started_ns) / 1_000_000_000,
        'first_to_last_seconds': (sent_times[-1] - sent_times[0]) / 1_000_000_000 if len(sent_times) > 1 else 0,
        'effective_hz_first_to_last': (len(sent_times) - 1) * 1_000_000_000 /
            (sent_times[-1] - sent_times[0]) if len(sent_times) > 1 else None,
        'interval_ms_min': min(intervals) if intervals else None,
        'interval_ms_median': statistics.median(intervals) if intervals else None,
        'interval_ms_mean': statistics.mean(intervals) if intervals else None,
        'interval_ms_max': max(intervals) if intervals else None,
        'lateness_ms_max': max(late_ms) if late_ms else None,
    }


def jsonline(stream, value):
    stream.write(json.dumps(value, ensure_ascii=False, separators=(',', ':')) + '\n')


def inspect(args):
    records, summary = source_records(args.pcap)
    return {
        'label': 'SYNTHETIC', 'status': 'INPUTS_VERIFIED_UDP_NOT_SENT',
        'source_pcap': file_info(args.pcap), 'pcap_summary': summary,
        'planned_count': args.count, 'planned_hz': args.hz,
        'planned_seconds': args.count / args.hz,
        'planned_payload_bytes': sum(len(records[index % len(records)]['payload'])
                                     for index in range(args.count)),
        'scope': 'unmodified actual payloads would be cyclically repeated on a new synthetic schedule',
    }


def send(args):
    records, summary = source_records(args.pcap)
    sent, error = [], None
    # Exclusive creation precedes socket creation, so an existing manifest
    # cannot accidentally cause a second test transmission.
    with args.manifest.open('x', encoding='utf-8', newline='\n') as manifest:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
            udp.bind(('127.0.0.1', 0))
            actual_source = endpoint(udp.getsockname())
            header = {
                'kind': 'SYNTHETIC_HEADER', 'version': 1, 'label': 'SYNTHETIC',
                'scope': 'actual PCAP payload bytes cyclically repeated; NOT an original acquisition',
                'source_pcap': file_info(args.pcap), 'pcap_summary': summary,
                'source_pcap_udp_records': len(records),
                'source': actual_source, 'destination': endpoint(args.destination),
                'requested_count': args.count, 'requested_hz': args.hz,
                'interval_ns_requested': 1_000_000_000 / args.hz,
                'created_utc': datetime.datetime.now(datetime.UTC).isoformat(),
                'expected_science_source': '192.168.1.128:1534',
                'expected_source_matches': False,
                'verifier_script': file_info(__file__),
            }
            jsonline(manifest, header)
            manifest.flush()
            started_wall_ns, started_ns = time.time_ns(), time.monotonic_ns()
            try:
                for index in range(args.count):
                    record = records[index % len(records)]
                    payload = record['payload']
                    deadline = started_ns + round((index + 1) * 1_000_000_000 / args.hz)
                    remaining_ns = deadline - time.monotonic_ns()
                    if remaining_ns > 0:
                        time.sleep(remaining_ns / 1_000_000_000)
                    before_ns, wall_ns = time.monotonic_ns(), time.time_ns()
                    size = udp.sendto(payload, args.destination)
                    after_ns = time.monotonic_ns()
                    if size != len(payload):
                        raise OSError(f'partial datagram send: {size} of {len(payload)} bytes')
                    row = {
                        'kind': 'SYNTHETIC_DATAGRAM', 'index': index,
                        'expected_raw_record_id': index + 1,
                        'source_pcap_record_id': record['record_id'],
                        'source_pcap_packet_index': record['packet_index'],
                        'source': actual_source, 'destination': endpoint(args.destination),
                        'scheduled_monotonic_ns': deadline,
                        'send_before_monotonic_ns': before_ns,
                        'send_after_monotonic_ns': after_ns,
                        'sent_wall_time_ns': wall_ns,
                        'payload_length': len(payload),
                        'payload_sha256': hashlib.sha256(payload).hexdigest(),
                        'payload_hex': payload.hex(),
                    }
                    jsonline(manifest, row)
                    sent.append(row)
                    if (index + 1) % max(1, round(args.hz)) == 0:
                        manifest.flush()
                        print('SYNTHETIC_PROGRESS ' + json.dumps({
                            'sent_count': index + 1,
                            'elapsed_seconds': (after_ns - started_ns) / 1_000_000_000,
                        }), flush=True)
            except (OSError, KeyboardInterrupt) as failure:
                error = f'{type(failure).__name__}: {failure}'
            ended_ns, ended_wall_ns = time.monotonic_ns(), time.time_ns()
            end = {
                'kind': 'SYNTHETIC_END', 'label': 'SYNTHETIC',
                'complete': error is None and len(sent) == args.count,
                'error': error, 'sent_count': len(sent),
                'sent_payload_bytes': sum(row['payload_length'] for row in sent),
                'started_wall_time_ns': started_wall_ns, 'ended_wall_time_ns': ended_wall_ns,
                **timing_summary(sent, started_ns, ended_ns),
                'meaning': 'successful sendto calls only; receiving/raw preservation require separate verification',
            }
            jsonline(manifest, end)
            manifest.flush()
    return {'status': 'PASS' if end['complete'] else 'FAIL',
            'label': 'SYNTHETIC', 'manifest': file_info(args.manifest), **end}


def load_manifest(path):
    with Path(path).open(encoding='utf-8') as stream:
        rows = [json.loads(line) for line in stream if line.strip()]
    if len(rows) < 2 or rows[0].get('kind') != 'SYNTHETIC_HEADER' or rows[-1].get('kind') != 'SYNTHETIC_END':
        raise ValueError('manifest requires SYNTHETIC_HEADER and SYNTHETIC_END')
    header, records, end = rows[0], rows[1:-1], rows[-1]
    loopback_destination(header['destination'])
    loopback_destination(header['source'])
    if header['label'] != 'SYNTHETIC' or header['version'] != 1:
        raise ValueError('invalid synthetic manifest version/label')
    if not end['complete'] or end['error'] is not None:
        raise ValueError('synthetic send did not complete')
    if len(records) != header['requested_count'] or end['sent_count'] != len(records):
        raise ValueError('manifest sent count disagrees with records')
    for index, row in enumerate(records):
        payload = bytes.fromhex(row['payload_hex'])
        if row['kind'] != 'SYNTHETIC_DATAGRAM' or row['index'] != index or row['expected_raw_record_id'] != index + 1:
            raise ValueError(f'manifest index/record ID mismatch at {index}')
        if row['source'] != header['source'] or row['destination'] != header['destination']:
            raise ValueError(f'manifest endpoint mismatch at {index}')
        if len(payload) != row['payload_length'] or hashlib.sha256(payload).hexdigest() != row['payload_sha256']:
            raise ValueError(f'manifest payload length/hash mismatch at {index}')
    if end['sent_payload_bytes'] != sum(row['payload_length'] for row in records):
        raise ValueError('manifest payload byte total mismatch')
    return header, records, end


def verify(args):
    header, expected, end = load_manifest(args.manifest)
    actual, summary = read_lvdsraw(args.raw)
    if summary['raw_header']['origin'] != 'LIVE_UDP' or len(actual) != len(expected):
        raise ValueError(f'raw origin/count mismatch: origin={summary["raw_header"]["origin"]}, actual={len(actual)}, expected={len(expected)}')
    if summary['raw_header']['expected_source'] != '192.168.1.128:1534':
        raise ValueError('test expects the normal fixed GUI science-source filter')
    _, destination_port = loopback_destination(header['destination'])
    if int(summary['raw_header']['binding'].rsplit(':', 1)[1]) != destination_port:
        raise ValueError('raw socket binding does not match the synthetic destination port')
    delays_ms = []
    for index, (received, sent) in enumerate(zip(actual, expected)):
        if received['record_id'] != index + 1:
            raise ValueError(f'raw record ID/order mismatch at {index}')
        payload = bytes.fromhex(sent['payload_hex'])
        if received['payload'] != payload:
            raise ValueError(f'raw payload bytes/length mismatch at {index}')
        if hashlib.sha256(received['payload']).hexdigest() != sent['payload_sha256']:
            raise ValueError(f'raw payload SHA-256 mismatch at {index}')
        if received['source'] != header['source'] or received['source_matches']:
            raise ValueError(f'raw source/filter flag mismatch at {index}')
        if received['destination_kind'] != 'SocketBinding' or received['destination'] != summary['raw_header']['binding']:
            raise ValueError(f'raw socket-binding destination mismatch at {index}')
        if received['origin_packet_index'] is not None:
            raise ValueError(f'live UDP raw record must not claim an original PCAP packet index at {index}')
        delays_ms.append((received['timestamp_ns'] - sent['sent_wall_time_ns']) / 1_000_000)
    return {
        'status': 'PASS', 'label': 'SYNTHETIC',
        'scope': 'synthetically paced repetitions of actual PCAP payloads; not original capture timing',
        'manifest': file_info(args.manifest), 'raw': file_info(args.raw),
        'records': len(actual), 'payload_bytes': sum(len(row['payload']) for row in actual),
        'source': header['source'], 'destination': header['destination'],
        'all_source_matches_false': True, 'all_payload_bytes_and_order_match': True,
        'raw_summary': summary, 'send_timing': end,
        'receive_minus_send_wall_ms': {
            'min': min(delays_ms), 'median': statistics.median(delays_ms), 'max': max(delays_ms),
            'note': 'receive wall timestamps differ from send timestamps; clock adjustments are not normalized',
        },
        'limits': 'No claim of original acquisition, genuine 192.168.1.128 ingress, scientific decode throughput, hardware, or RSS verification',
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    commands = parser.add_subparsers(dest='command', required=True)
    for name in ('inspect', 'send'):
        command = commands.add_parser(name, allow_abbrev=False)
        command.add_argument('--pcap', type=Path, required=True)
        command.add_argument('--count', type=positive_count, default=1200)
        command.add_argument('--hz', type=positive_hz, default=20)
        if name == 'send':
            command.add_argument('--destination', type=loopback_destination, required=True)
            command.add_argument('--manifest', type=Path, required=True,
                                 help='new UTF-8 JSONL path; refuses existing files')
    command = commands.add_parser('verify', allow_abbrev=False)
    command.add_argument('--manifest', type=Path, required=True)
    command.add_argument('--raw', type=Path, required=True)
    command.add_argument('--output-json', type=Path)
    args = parser.parse_args(argv)
    try:
        report = {'inspect': inspect, 'send': send, 'verify': verify}[args.command](args)
    except (OSError, ValueError, KeyError, TypeError) as error:
        report = {'status': 'FAIL', 'label': 'SYNTHETIC', 'error': str(error)}
    serialized = json.dumps(report, ensure_ascii=False, indent=2) + '\n'
    output = getattr(args, 'output_json', None)
    if output:
        with output.open('x', encoding='utf-8', newline='\n') as stream:
            stream.write(serialized)
    print(serialized, end='')
    return int(report['status'] == 'FAIL')


if __name__ == '__main__':
    raise SystemExit(main())
