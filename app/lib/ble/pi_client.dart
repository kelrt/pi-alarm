import 'package:flutter/foundation.dart';

/// The Pi answered, and the answer was "no" (`"ok": false`).
///
/// Retrying the same request will fail the same way, so a queued change that
/// gets this is dropped and the message shown to the user.
class PiRejected implements Exception {
  PiRejected(this.code, this.message);

  /// Machine-readable code from PROTOCOL.md, e.g. `invalid_alarm`.
  final String code;
  final String message;

  @override
  String toString() => message;
}

/// The request could not be delivered or no answer came back: out of range,
/// Bluetooth off, timeout. Worth retrying later.
class PiUnavailable implements Exception {
  PiUnavailable(this.message);
  final String message;

  @override
  String toString() => message;
}

/// What the rest of the app needs from the connection to the Pi.
///
/// The real implementation is `PiConnection` (BLE). Keeping this small
/// interface separate lets the sync logic be tested with a fake.
abstract class PiClient implements Listenable {
  /// True when requests can be sent right now.
  bool get isConnected;

  /// Send one request and wait for its response.
  ///
  /// [op] and [params] are described in PROTOCOL.md. Returns the response
  /// object on success. Throws [PiRejected] if the Pi refused the request and
  /// [PiUnavailable] if it could not be reached.
  Future<Map<String, dynamic>> request(
    String op, [
    Map<String, dynamic> params = const {},
  ]);
}
