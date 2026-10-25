"""Independent, read-only UDP/bit-boundary audit; never hardware fault proof.

No record supplies bits to another. A complete frame requires 2152 real bits
inside one declared UDP science payload. CRC-CCITT-FALSE over frame[7:267]
is reference evidence only; ordinary native frames remain CRC_NOT_CHECKED.
"""
import argparse
from collections import Counter
import hashlib
import ipaddress
import json
from pathlib import Path
import struct
import sys


HEADER = bytes.fromhex('eb9055aa0107aa')
HEADER_BITS = ''.join(f'{byte:08b}' for byte in HEADER)
FRAME_BITS = 269 * 8
MAX_RECOVERY_PAYLOAD_BYTES = 4096


def reference_crc(data):
    value = 0xffff
    for byte in data:
        value ^= byte << 8
        for _ in range(8):
            value = ((value << 1) ^ (0x1021 if value & 0x8000 else 0)) & 0xffff
    return value


def _file_info(path):
    path = Path(path)
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for part in iter(lambda: stream.read(65536), b''):
            digest.update(part)
    return {'path': str(path.resolve()), 'bytes': path.stat().st_size, 'sha256': digest.hexdigest()}


def _options(data, endian):
    offset = 0
    while offset < len(data):
        if len(data) - offset < 4:
            raise ValueError('truncated PCAPNG option header')
        code, size = struct.unpack_from(endian + 'HH', data, offset)
        offset += 4
        if code == 0:
            return
        if offset + size > len(data):
            raise ValueError('truncated PCAPNG option value')
        yield code, data[offset:offset + size]
        offset += (size + 3) & ~3


def _network_udp(ethernet):
    if len(ethernet) < 14:
        raise ValueError('truncated Ethernet header')
    protocol = int.from_bytes(ethernet[12:14], 'big')
    offset = 14
    for _ in range(5):
        if protocol not in (0x8100, 0x88a8, 0x9100):
            break
        if offset + 4 > len(ethernet):
            raise ValueError('truncated Ethernet VLAN header')
        protocol = int.from_bytes(ethernet[offset + 2:offset + 4], 'big')
        offset += 4
    ip = ethernet[offset:]
    if protocol == 0x0800:
        if len(ip) < 20 or ip[0] >> 4 != 4:
            raise ValueError('truncated/invalid IPv4 header')
        header = (ip[0] & 15) * 4
        total = int.from_bytes(ip[2:4], 'big')
        if header < 20 or not header <= total <= len(ip):
            raise ValueError('invalid/truncated IPv4 length')
        if int.from_bytes(ip[6:8], 'big') & 0x3fff:
            raise ValueError('fragmented IPv4: no speculative reassembly')
        if ip[9] != 17:
            return None
        source, destination = ip[12:16], ip[16:20]
        udp = ip[header:total]
    elif protocol == 0x86dd:
        if len(ip) < 40 or ip[0] >> 4 != 6:
            raise ValueError('truncated/invalid IPv6 header')
        total = 40 + int.from_bytes(ip[4:6], 'big')
        if not 40 < total <= len(ip):
            raise ValueError('unsupported/truncated IPv6 length')
        source, destination = ip[8:24], ip[24:40]
        next_header, offset = ip[6], 40
        for _ in range(8):
            if next_header == 17:
                break
            if next_header == 44:
                raise ValueError('fragmented IPv6: no speculative reassembly')
            if next_header not in (0, 43, 60, 51):
                return None
            if offset + 2 > total:
                raise ValueError('truncated IPv6 extension')
            size = (ip[offset + 1] + 2) * 4 if next_header == 51 else (ip[offset + 1] + 1) * 8
            if offset + size > total:
                raise ValueError('truncated IPv6 extension length')
            next_header, offset = ip[offset], offset + size
        if next_header != 17:
            raise ValueError('too many IPv6 extensions')
        udp = ip[offset:total]
    else:
        return None
    if len(udp) < 8:
        raise ValueError('truncated UDP header')
    sport, dport, length, _ = struct.unpack_from('>HHHH', udp)
    if not 8 <= length <= len(udp):
        raise ValueError('invalid/truncated UDP payload length')
    def endpoint(address, port):
        address = ipaddress.ip_address(address)
        return f'[{address}]:{port}' if address.version == 6 else f'{address}:{port}'
    return endpoint(source, sport), endpoint(destination, dport), udp[8:length]


def read_pcap_udp(path):
    """Read standard Ethernet EPBs and preserve every decodable UDP payload.

    Packet parsing failures are explicit diagnostics, never omitted silently.
    A truncated PCAP block fails instead of appearing to be a clean EOF.
    """
    records, errors = [], []
    endian, interfaces, section, packets = None, [], -1, 0
    with Path(path).open('rb') as stream:
        while True:
            header = stream.read(8)
            if not header:
                break
            if len(header) != 8:
                raise ValueError('truncated PCAPNG block header')
            if header[:4] == bytes.fromhex('0a0d0d0a'):
                bom = stream.read(4)
                if bom == bytes.fromhex('4d3c2b1a'):
                    endian = '<'
                elif bom == bytes.fromhex('1a2b3c4d'):
                    endian = '>'
                else:
                    raise ValueError('invalid/truncated PCAPNG byte-order magic')
                size = struct.unpack(endian + 'I', header[4:])[0]
                if size < 28 or size > 16777216 or size % 4:
                    raise ValueError('invalid PCAPNG section length')
                tail = stream.read(size - 12)
                body = bom + tail[:-4]
                section += 1
                interfaces = []
            else:
                if endian is None:
                    raise ValueError('PCAPNG must begin with a section')
                size = struct.unpack(endian + 'I', header[4:])[0]
                if size < 12 or size > 16777216 or size % 4:
                    raise ValueError('invalid PCAPNG block length')
                tail = stream.read(size - 8)
                body = tail[:-4]
            if len(tail) != size - (12 if header[:4] == bytes.fromhex('0a0d0d0a') else 8):
                raise ValueError('truncated PCAPNG block boundary')
            if struct.unpack(endian + 'I', tail[-4:])[0] != size:
                raise ValueError('PCAPNG block trailer length mismatch')
            kind = struct.unpack(endian + 'I', header[:4])[0]
            if kind == 1:
                if len(body) < 8:
                    raise ValueError('truncated PCAPNG interface')
                linktype, _, snaplen = struct.unpack_from(endian + 'HHI', body)
                units, time_offset = 1000000, 0
                for code, value in _options(body[8:], endian):
                    if code == 9 and len(value) == 1:
                        units = (2 ** (value[0] & 127)) if value[0] & 128 else 10 ** value[0]
                    elif code == 14 and len(value) == 8:
                        time_offset = struct.unpack(endian + 'q', value)[0]
                interfaces.append((linktype, snaplen, units, time_offset))
            elif kind == 6:
                index = packets
                packets += 1
                if len(body) < 20:
                    raise ValueError('truncated PCAPNG enhanced packet header')
                interface, high, low, captured, original = struct.unpack_from(endian + 'IIIII', body)
                if interface >= len(interfaces) or captured > original or 20 + captured > len(body):
                    raise ValueError('invalid/truncated PCAPNG enhanced packet length/interface')
                linktype, _, units, time_offset = interfaces[interface]
                try:
                    if linktype != 1:
                        raise ValueError(f'unsupported PCAPNG linktype {linktype}')
                    udp = _network_udp(body[20:20 + captured])
                    if udp is None:
                        continue
                except ValueError as error:
                    errors.append({'packet_index': index, 'error': str(error)})
                    continue
                ticks = (high << 32) + low
                records.append({'record_id': len(records), 'packet_index': index,
                                'session': f'pcap-section-{section}-interface-{interface}',
                                'timestamp_ns': ticks * 1000000000 // units + time_offset * 1000000000,
                                'capture_ticks': ticks, 'ticks_per_second': units,
                                'source': udp[0], 'destination': udp[1], 'payload': udp[2],
                                'capture_truncated': captured != original})
            elif kind in (2, 3):
                raise ValueError(f'unsupported PCAPNG packet block kind {kind}; refusing an incomplete UDP audit')
    return records, {'packets': packets, 'udp_records': len(records),
                     'packet_diagnostics': errors, 'clean_pcap_eof': True}


def _raw_endpoint(data, allow_unknown=False):
    if len(data) != 24:
        raise ValueError('invalid raw endpoint length')
    family, reserved, port = struct.unpack_from('<BBH', data)
    scope = struct.unpack_from('<I', data, 20)[0]
    if family == 0 and allow_unknown and data == bytes(24):
        return None
    if reserved or family not in (4, 6):
        raise ValueError('invalid raw endpoint family/reserved')
    if family == 4:
        if data[8:20] != bytes(12) or scope:
            raise ValueError('invalid raw IPv4 endpoint padding/scope')
        return f'{ipaddress.ip_address(data[4:8])}:{port}'
    address = str(ipaddress.ip_address(data[4:20]))
    return f'[{address}{"%" + str(scope) if scope else ""}]:{port}'


def read_lvdsraw(path):
    """Independent strict LVDSRAW v1 reader, including clean-close footer.

    No checksum is invented for this container. Header, every boundary, record
    metadata, footer counters and exact EOF are checked before returning PASS.
    """
    records = []
    with Path(path).open('rb') as stream:
        header = stream.read(128)
        if len(header) != 128:
            raise ValueError('truncated lvdsraw header')
        magic, version, size, endian, origin, maximum, created_sec, created_ns, flags = struct.unpack_from('<8sHHIIIQII', header)
        expected = [(magic, b'LVDSRAW\0', 'magic'), (version, 1, 'version'), (size, 128, 'header length'),
                    (endian, 0x01020304, 'endian'), (maximum, 65535, 'maximum payload'), (flags, 0, 'header flags')]
        for actual, wanted, field in expected:
            if actual != wanted:
                raise ValueError(f'lvdsraw {field}: actual {actual!r}; expected {wanted!r}')
        if origin not in (1, 2) or created_ns >= 1000000000 or header[104:] != bytes(24):
            raise ValueError('invalid lvdsraw origin/creation time/reserved')
        expected_source = _raw_endpoint(header[56:80])
        binding = _raw_endpoint(header[80:104])
        last_id, payload_bytes = 0, 0
        while True:
            prefix = stream.read(8)
            if not prefix:
                raise ValueError('incomplete lvdsraw: missing close footer')
            if len(prefix) != 8:
                raise ValueError('truncated lvdsraw record prefix')
            kind, head_size, total = struct.unpack('<HHI', prefix)
            if not 12 <= total <= 100 + 65535:
                raise ValueError('invalid lvdsraw record length')
            remainder = stream.read(total - 8)
            if len(remainder) != total - 8:
                raise ValueError('truncated lvdsraw record/footer')
            block = prefix + remainder
            if struct.unpack_from('<I', block, total - 4)[0] != total:
                raise ValueError('lvdsraw record boundary trailer length mismatch')
            if kind == 1:
                if head_size != 96 or total < 100:
                    raise ValueError('invalid lvdsraw data record header length')
                record_id, seconds, nanos, record_flags = struct.unpack_from('<QQII', block, 8)
                payload_size, reserved, packet_index = struct.unpack_from('<IIQ', block, 80)
                if total != 100 + payload_size or payload_size > 65535 or reserved:
                    raise ValueError('invalid lvdsraw data payload length/reserved')
                if record_id <= last_id or nanos >= 1000000000 or record_flags & ~15:
                    raise ValueError('invalid lvdsraw record id/time/flags')
                destination_kind = (record_flags >> 2) & 3
                if destination_kind == 3:
                    raise ValueError('invalid lvdsraw destination kind')
                source = _raw_endpoint(block[32:56])
                destination = _raw_endpoint(block[56:80], allow_unknown=destination_kind == 0)
                if destination_kind == 0 and destination is not None:
                    raise ValueError('unknown raw destination must have a zero endpoint')
                if not record_flags & 2 and packet_index:
                    raise ValueError('raw packet index exists without its flag')
                if bool(record_flags & 1) != (source == expected_source):
                    raise ValueError('raw source_matches disagrees with header expected source')
                records.append({'record_id': record_id, 'packet_index': packet_index if record_flags & 2 else None,
                                'origin_packet_index': packet_index if record_flags & 2 else None,
                                'timestamp_ns': seconds * 1000000000 + nanos,
                                'source': source, 'destination': destination,
                                'destination_kind': {0: 'Unknown', 1: 'Captured', 2: 'SocketBinding'}[destination_kind],
                                'source_matches': bool(record_flags & 1), 'session': header[40:56].hex(),
                                'payload': block[96:96 + payload_size]})
                last_id = record_id
                payload_bytes += payload_size
            elif kind == 2:
                if head_size != 88 or total != 92 or block[72:88] != bytes(16):
                    raise ValueError('invalid lvdsraw footer length/reserved')
                names = ['observed', 'enqueued', 'written', 'dropped', 'payload_bytes', 'last_observed_id', 'status', 'reason', 'last_written_id']
                footer = dict(zip(names, struct.unpack_from('<QQQQQQIIQ', block, 8)))
                if footer['status'] not in (1, 2) or footer['reason'] not in (1, 2):
                    raise ValueError('invalid lvdsraw footer status/reason')
                if footer['written'] != len(records) or footer['payload_bytes'] != payload_bytes or footer['last_written_id'] != last_id:
                    raise ValueError('lvdsraw footer disagrees with actual written records/bytes/id')
                if not footer['observed'] >= footer['enqueued'] == footer['written'] or footer['dropped'] != footer['observed'] - footer['enqueued'] or footer['last_observed_id'] != footer['observed'] or last_id > footer['observed']:
                    raise ValueError('inconsistent lvdsraw footer queue counters')
                if footer['status'] != 1 or footer['dropped'] or footer['written'] != footer['observed'] or footer['reason'] != 1:
                    raise ValueError('incomplete lvdsraw: loss/abandoned recording footer')
                if any(record['record_id'] != index + 1 for index, record in enumerate(records)):
                    raise ValueError('complete lvdsraw has non-continuous record ids')
                if stream.read(1):
                    raise ValueError('extra data after lvdsraw footer; expected EOF')
                return records, {'raw_header': {'version': version, 'header_bytes': size, 'origin': {1: 'LIVE_UDP', 2: 'PCAP_REPLAY'}[origin],
                                                'created_at_ns': created_sec * 1000000000 + created_ns,
                                                'session_id_hex': header[40:56].hex(), 'expected_source': expected_source, 'binding': binding},
                                 'footer': footer, 'clean_raw_eof': True}
            else:
                raise ValueError(f'unknown lvdsraw record kind {kind}')


def compare_raw_records(pcap_records, raw_records, raw_summary):
    if raw_summary['raw_header']['origin'] != 'PCAP_REPLAY':
        raise ValueError('raw archive origin must be PCAP_REPLAY for PCAP byte comparison')
    if len(pcap_records) != len(raw_records):
        raise ValueError(f'raw record count: actual {len(raw_records)}; expected {len(pcap_records)}')
    total = 0
    for index, (expected, actual) in enumerate(zip(pcap_records, raw_records)):
        for field in ('payload', 'source', 'destination', 'timestamp_ns'):
            if actual[field] != expected[field]:
                raise ValueError(f'raw record {index} {field} mismatch')
        if actual['origin_packet_index'] != expected['packet_index']:
            raise ValueError(f'raw record {index} origin_packet_index mismatch')
        if actual['record_id'] != index + 1 or actual['destination_kind'] != 'Captured':
            raise ValueError(f'raw record {index} id/destination kind mismatch')
        total += len(actual['payload'])
    return {'status': 'PASS', 'records': len(raw_records), 'udp_payload_bytes': total,
            'fields_checked': ['payload length/order/bytes', 'source', 'captured destination', 'timestamp_ns', 'origin_packet_index', 'record_id', 'source_matches'],
            'clean_raw_eof': raw_summary['clean_raw_eof']}


def _science_prefix(data):
    errors = []
    if len(data) < 267:
        return {'structure_valid': False, 'errors': ['fewer than 267 complete bytes; fixed data/two channels unavailable']}
    if data[:7] != HEADER:
        errors.append('fixed header mismatch')
    seconds, micros = struct.unpack_from('>II', data, 7)
    if micros >= 1000000:
        errors.append('microseconds outside 0..999999')
    if data[266] != 0:
        errors.append('DATA251 reserved byte is not 00')
    channels = []
    for start in (15, 140):
        channel = []
        for offset in range(start, start + 125, 5):
            value = int.from_bytes(data[offset:offset + 5], 'big')
            channel.extend((value >> shift) & 1023 for shift in (30, 20, 10, 0))
        channels.append(channel)
    return {'structure_valid': not errors, 'errors': errors, 'fixed_header_hex': data[:7].hex(),
            'seconds': seconds, 'microseconds': micros, 'seq8': data[265], 'reserved': data[266],
            'sample_counts': list(map(len, channels)), 'channel_1_code10': channels[0],
            'channel_2_code10': channels[1], 'device_time_us': seconds * 1000000 + micros}


def _candidate(bits, start):
    available = len(bits) - start
    full_bytes = min(269, available // 8)
    prefix = int(bits[start:start + full_bytes * 8], 2).to_bytes(full_bytes, 'big')
    shift = start % 8
    result = {'bit_start': start, 'byte_start': start // 8, 'shift': shift,
              'alignment_mode': 'BYTE_SYNC' if not shift else 'MSB_FIRST_LEFT_SHIFT',
              'direction': f'extract MSB-first real bits [{start}, {min(start + FRAME_BITS, len(bits))})',
              'available_bits': available, 'required_bits': FRAME_BITS,
              'missing_tail_bits': max(0, FRAME_BITS - available),
              'complete_269_bytes': available >= FRAME_BITS,
              'requires_cross_record_bits': available < FRAME_BITS,
              'dependency_record_ids': [], **_science_prefix(prefix)}
    result['reference_crc'] = {'algorithm': 'CRC-16/CCITT-FALSE', 'coverage': 'normalized frame[7:267], end exclusive',
                               'status': 'NOT_EVALUATED', 'manufacturer_coverage_confirmed': False}
    if full_bytes >= 267:
        expected = reference_crc(prefix[7:267])
        received_bits = bits[start + 267 * 8:min(start + FRAME_BITS, len(bits))]
        result['reference_crc'].update({'calculated': f'{expected:04X}',
                                         'received_crc_available_bits': len(received_bits),
                                         'received_crc_prefix_bits': received_bits,
                                         'received_prefix_matches': f'{expected:016b}'.startswith(received_bits) if received_bits else None})
    if available >= FRAME_BITS:
        received = int.from_bytes(prefix[267:269], 'big')
        result['reference_crc'].update({'calculated': f'{expected:04X}', 'received': f'{received:04X}',
                                         'status': 'MATCH' if expected == received else 'MISMATCH'})
        result['normalized_frame_hex'] = prefix.hex()
        result['normalized_frame_sha256'] = hashlib.sha256(prefix).hexdigest()
    return result


def audit_inner(raw):
    if len(raw) > MAX_RECOVERY_PAYLOAD_BYTES:
        return {'raw_frame_length': len(raw), 'raw_first_bytes_hex': raw[:16].hex(),
                'native_valid': False, 'quality': 'REJECTED',
                'reason': 'payload exceeds the 4096-byte bounded synchronization domain',
                'candidates': [], 'omitted_candidates': 0, 'selected': None}
    bits = ''.join(f'{byte:08b}' for byte in raw)
    candidates, start, omitted = [], 0, 0
    while True:
        start = bits.find(HEADER_BITS, start)
        if start < 0:
            break
        if len(candidates) < 32:
            candidates.append(_candidate(bits, start))
        else:
            omitted += 1
        start += 1
    native = len(raw) == 269 and raw[:7] == HEADER and _science_prefix(raw)['structure_valid']
    viable = [item for item in candidates if item.get('structure_valid')]
    selected = viable[0] if len(viable) == 1 and not omitted else None
    quality = 'NATIVE' if native else 'RECOVERY_CANDIDATE' if viable else 'REJECTED'
    reason = ('direct complete native frame; CRC is not an admission gate' if native else
              'unique full-bit candidate requires continuity/reference evidence' if selected and selected['complete_269_bytes'] else
              'not enough same-record bits for a complete frame' if selected else
              'ambiguous valid candidates' if viable else
              'fixed header found but structure/time/reserved validation failed' if candidates else 'no complete 56-bit fixed header')
    return {'raw_frame_length': len(raw), 'raw_first_bytes_hex': raw[:16].hex(),
            'native_valid': native, 'quality': quality, 'reason': reason,
            'candidates': candidates, 'omitted_candidates': omitted, 'selected': selected}


def _unwrap(payload):
    if len(payload) < 24 or payload[:4] != bytes.fromhex('55aa01a5'):
        raise ValueError('invalid/truncated UDP 24-byte magic/version/type')
    sequence, size, index, count, offset, length, flags = struct.unpack_from('>IIHHIHH', payload, 4)
    if not count or index >= count or length != len(payload) - 24 or offset + length > size:
        raise ValueError('invalid UDP wrapper segment length/range/count')
    header = {'outer_sequence_u32': sequence, 'frame_length': size, 'segment_index': index,
              'segment_count': count, 'byte_offset': offset, 'segment_length': length, 'flags': flags}
    if count != 1 or index or offset or size != length:
        raise ValueError('fragmented UDP wrapper: audit will not infer a contiguous bitstream across records')
    return header, payload[24:]


def audit_records(records):
    rows, previous = [], None
    for record in records:
        payload = record['payload']
        row = {key: value for key, value in record.items() if key != 'payload'}
        row.update({'udp_payload_length': len(payload), 'udp_payload_sha256': hashlib.sha256(payload).hexdigest(),
                    'udp_payload_hex': payload.hex()})
        try:
            wrapper, raw = _unwrap(payload)
        except ValueError as error:
            row.update({'quality': 'REJECTED', 'reason': str(error), 'candidates': [], 'selected': None})
            rows.append(row)
            previous = None
            continue
        row.update(wrapper)
        row.update(audit_inner(raw))
        selected = row['selected']
        auxiliary = {'previous_record_id': previous['record_id'] if previous else None,
                     'same_source_destination_session': False, 'outer_u32_continuous': False,
                     'inner_seq8_continuous': False, 'device_delta_50000us': False}
        if previous and selected and previous.get('selected'):
            last = previous['selected']
            auxiliary.update({
                'same_source_destination_session': (row['source'], row['destination'], row.get('session')) == (previous['source'], previous['destination'], previous.get('session')),
                'outer_u32_continuous': row['outer_sequence_u32'] == (previous['outer_sequence_u32'] + 1) % (1 << 32),
                'inner_seq8_continuous': selected['seq8'] == (last['seq8'] + 1) % 256,
                'device_delta_50000us': selected['device_time_us'] - last['device_time_us'] == 50000,
            })
        row['auxiliary_continuity'] = auxiliary
        if not row['native_valid'] and selected and selected['complete_269_bytes']:
            if selected['shift'] == 0:
                row.update({'quality': 'NATIVE', 'reason': 'unique complete byte-sync frame; unchanged payload bytes'})
            elif selected['shift'] in (1, 4, 7) and selected['reference_crc']['status'] == 'MATCH' and previous and previous['quality'] in ('NATIVE', 'RECOVERED_1BIT', 'RECOVERED_4BIT') and all(auxiliary[key] for key in auxiliary if key != 'previous_record_id'):
                quality = 'RECOVERED_4BIT' if selected['shift'] == 4 else 'RECOVERED_1BIT'
                row.update({'quality': quality, 'reason': 'complete unique same-record bits plus reference CRC and all continuity checks'})
        row['cross_record_bits_used'] = False
        row['ordinary_science_integrity'] = 'CRC_NOT_CHECKED'
        rows.append(row)
        previous = row
    counts = Counter(row['quality'] for row in rows)
    sequences = [row['outer_sequence_u32'] for row in rows if 'outer_sequence_u32' in row]
    summary = {'udp_records': len(rows), 'quality_counts': dict(counts),
               'confirmed_recovered': counts['RECOVERED_1BIT'] + counts['RECOVERED_4BIT'],
               'source_counts': dict(Counter(row['source'] for row in rows)),
               'inner_length_counts': dict(Counter(row['raw_frame_length'] for row in rows if 'raw_frame_length' in row)),
               'outer_sequence_first': sequences[0] if sequences else None,
               'outer_sequence_last': sequences[-1] if sequences else None,
               'outer_sequence_step_counts': dict(Counter((b - a) % (1 << 32) for a, b in zip(sequences, sequences[1:]))),
               'candidate_shift_counts': dict(Counter(candidate['shift'] for row in rows if not row.get('native_valid') for candidate in row['candidates'])),
               'candidate_missing_tail_bit_counts': dict(Counter(candidate['missing_tail_bits'] for row in rows if not row.get('native_valid') for candidate in row['candidates'])),
               'complete_same_record_candidates': sum(candidate['complete_269_bytes'] and candidate['structure_valid'] for row in rows if not row.get('native_valid') for candidate in row['candidates']),
               'reference_crc_status_counts': dict(Counter(candidate['reference_crc']['status'] for row in rows for candidate in row['candidates'])),
               'partial_crc_prefix_match_counts': dict(Counter(str(candidate['reference_crc'].get('received_prefix_matches')) for row in rows for candidate in row['candidates'] if not candidate['complete_269_bytes'])),
               'cross_record_bits_used': False,
               'scope': 'Independent offline evidence; no scientific admission or hardware fault claim'}
    return rows, summary


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pcap', type=Path, required=True)
    parser.add_argument('--jsonl', type=Path, required=True, help='new per-UDP record audit')
    parser.add_argument('--summary', type=Path, required=True, help='new summary JSON')
    parser.add_argument('--raw', type=Path, help='independently compare a completed PCAP_REPLAY lvdsraw archive')
    args = parser.parse_args(argv)
    try:
        for path in (args.jsonl, args.summary):
            if path.exists() or path.resolve() == args.pcap.resolve():
                raise FileExistsError(f'refusing to overwrite existing/source file: {path}')
        if args.jsonl.resolve() == args.summary.resolve():
            raise ValueError('JSONL and summary paths must differ')
        records, packet_summary = read_pcap_udp(args.pcap)
        rows, summary = audit_records(records)
        summary.update({'capture': _file_info(args.pcap), **packet_summary})
        if args.raw:
            raw_records, raw_summary = read_lvdsraw(args.raw)
            summary['raw_archive'] = {**_file_info(args.raw), **raw_summary,
                                      'comparison': compare_raw_records(records, raw_records, raw_summary)}
        for path in (args.jsonl, args.summary):
            path.parent.mkdir(parents=True, exist_ok=True)
        with args.jsonl.open('x', encoding='utf-8') as stream:
            for row in rows:
                stream.write(json.dumps(row, ensure_ascii=True) + '\n')
        with args.summary.open('x', encoding='utf-8') as stream:
            json.dump(summary, stream, ensure_ascii=True, indent=2)
            stream.write('\n')
        print(json.dumps(summary, ensure_ascii=True, indent=2))
        return 0
    except (OSError, ValueError, struct.error) as error:
        print(f'FAIL: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
