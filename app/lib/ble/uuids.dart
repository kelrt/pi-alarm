import 'package:flutter_blue_plus/flutter_blue_plus.dart';

/// The Pi's GATT service and characteristics.
///
/// These must match `src/ble.rs` in the daemon and PROTOCOL.md.
class PiUuids {
  static final Guid service = Guid('7a1e0001-5c9b-4f6a-9d2e-b1a2c3d4e5f6');

  /// The phone writes request chunks here.
  static final Guid request = Guid('7a1e0002-5c9b-4f6a-9d2e-b1a2c3d4e5f6');

  /// The Pi notifies response chunks here.
  static final Guid response = Guid('7a1e0003-5c9b-4f6a-9d2e-b1a2c3d4e5f6');

  /// Readable JSON with the protocol version (handy when debugging).
  static final Guid info = Guid('7a1e0004-5c9b-4f6a-9d2e-b1a2c3d4e5f6');
}

/// Version of the message format this app speaks (the `"v"` field).
const int protocolVersion = 1;
