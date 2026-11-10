/// Cutting messages into BLE-sized chunks and gluing them back together.
///
/// This is the Dart twin of the framing code in the daemon's
/// `src/protocol.rs`. See PROTOCOL.md, section "Framing".
///
/// Every chunk starts with a one-byte header:
///   bit 0 (0x01) = first chunk of a message
///   bit 1 (0x02) = last chunk of a message
library;

const int chunkFirst = 0x01;
const int chunkLast = 0x02;

/// Largest message either side accepts.
const int maxMessageLength = 16 * 1024;

/// Smallest chunk that fits every BLE connection.
const int minChunkLength = 20;

/// Largest value a characteristic can carry.
const int maxChunkLength = 512;

/// Cut [message] into chunks of at most [maxChunkLen] bytes, header included.
List<List<int>> chunkMessage(List<int> message, int maxChunkLen) {
  final dataLen = (maxChunkLen < 2 ? 2 : maxChunkLen) - 1;
  final chunks = <List<int>>[];

  if (message.isEmpty) {
    return [
      [chunkFirst | chunkLast]
    ];
  }

  for (var start = 0; start < message.length; start += dataLen) {
    final end =
        (start + dataLen < message.length) ? start + dataLen : message.length;
    var header = 0;
    if (start == 0) header |= chunkFirst;
    if (end == message.length) header |= chunkLast;
    chunks.add([header, ...message.sublist(start, end)]);
  }
  return chunks;
}

/// Thrown by [Reassembler.push] for a chunk that makes no sense.
class FramingException implements Exception {
  FramingException(this.message);
  final String message;

  @override
  String toString() => 'FramingException: $message';
}

/// Collects chunks until a whole message has arrived.
class Reassembler {
  final List<int> _buffer = [];
  bool _inMessage = false;

  /// Forget a half-received message (call on disconnect).
  void reset() {
    _buffer.clear();
    _inMessage = false;
  }

  /// Feed one chunk. Returns the complete message when [chunk] was the last
  /// one, otherwise `null`.
  List<int>? push(List<int> chunk) {
    if (chunk.isEmpty) {
      throw FramingException('empty chunk');
    }
    final header = chunk[0];
    if (header & ~(chunkFirst | chunkLast) != 0) {
      reset();
      throw FramingException('unknown chunk header $header');
    }

    if (header & chunkFirst != 0) {
      // A first chunk always starts afresh.
      reset();
      _inMessage = true;
    } else if (!_inMessage) {
      throw FramingException('chunk received before the start of a message');
    }

    if (_buffer.length + chunk.length - 1 > maxMessageLength) {
      reset();
      throw FramingException('message too large');
    }
    _buffer.addAll(chunk.skip(1));

    if (header & chunkLast != 0) {
      final message = List<int>.of(_buffer);
      reset();
      return message;
    }
    return null;
  }
}
