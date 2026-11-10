// Tests for the chunk framing. The same cases exist on the Pi side in
// src/protocol.rs; if both pass, the two ends agree byte for byte.
import 'package:flutter_test/flutter_test.dart';
import 'package:pi_alarm/ble/framing.dart';

void main() {
  test('a short message is one chunk with both flags', () {
    final chunks = chunkMessage('hello'.codeUnits, 20);
    expect(chunks, [
      [0x03, ...'hello'.codeUnits]
    ]);
  });

  test('a long message is split and flagged', () {
    final message = List<int>.generate(50, (i) => i);
    final chunks = chunkMessage(message, 20);
    // 19 data bytes per chunk: 19 + 19 + 12.
    expect(chunks.length, 3);
    expect(chunks[0][0], chunkFirst);
    expect(chunks[1][0], 0);
    expect(chunks[2][0], chunkLast);
    expect(chunks.map((c) => c.length), [20, 20, 13]);
  });

  test('a message that exactly fills chunks has no empty tail', () {
    final chunks = chunkMessage(List.filled(38, 7), 20);
    expect(chunks.length, 2);
    expect(chunks[1][0], chunkLast);
    expect(chunks[1].length, 20);
  });

  test('an empty message is one header-only chunk', () {
    expect(chunkMessage([], 20), [
      [chunkFirst | chunkLast]
    ]);
  });

  test('chunks reassemble at every size', () {
    final message = List<int>.generate(1000, (i) => i % 251);
    for (final size in [2, 3, 20, 23, 185, 244, 512, 2000]) {
      final reassembler = Reassembler();
      List<int>? result;
      for (final chunk in chunkMessage(message, size)) {
        expect(chunk.length <= size, isTrue);
        result = reassembler.push(chunk);
      }
      expect(result, message, reason: 'chunk size $size');
    }
  });

  test('a new first chunk discards a half-received message', () {
    final reassembler = Reassembler();
    expect(reassembler.push([chunkFirst, 1, 2]), isNull);
    expect(reassembler.push([chunkFirst | chunkLast, 8, 9]), [8, 9]);
  });

  test('bad chunks are rejected', () {
    final reassembler = Reassembler();
    expect(() => reassembler.push([]), throwsA(isA<FramingException>()));
    expect(() => reassembler.push([0x80, 1]), throwsA(isA<FramingException>()));
    // Middle and last chunks with no first chunk before them.
    expect(() => reassembler.push([0x00, 1]), throwsA(isA<FramingException>()));
    expect(
        () => reassembler.push([chunkLast, 1]), throwsA(isA<FramingException>()));
    // Still usable afterwards.
    expect(reassembler.push([chunkFirst | chunkLast, 9]), [9]);
  });

  test('oversized messages are rejected', () {
    final reassembler = Reassembler();
    reassembler.push([chunkFirst, ...List.filled(1000, 0)]);
    expect(() {
      for (var i = 0; i < 20; i++) {
        reassembler.push([0x00, ...List.filled(1000, 0)]);
      }
    }, throwsA(isA<FramingException>()));
  });
}
