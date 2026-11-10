import 'package:flutter/material.dart';

import '../ble/pi_connection.dart';
import '../data/alarm_repository.dart';

/// The small dot-and-word in the app bar that says how things stand with
/// the Pi. Tapping it shows details, and offers "Find Pi" during first-time
/// setup.
class StatusIndicator extends StatelessWidget {
  const StatusIndicator({
    super.key,
    required this.connection,
    required this.repository,
  });

  final PiConnection connection;
  final AlarmRepository repository;

  @override
  Widget build(BuildContext context) {
    // Rebuilds whenever either object calls notifyListeners().
    return ListenableBuilder(
      listenable: Listenable.merge([connection, repository]),
      builder: (context, _) {
        final status = _describe();
        return Tooltip(
          message: status.detail,
          child: InkWell(
            borderRadius: BorderRadius.circular(16),
            onTap: () => _showDetails(context, status),
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(Icons.circle, size: 10, color: status.color),
                  const SizedBox(width: 6),
                  Text(status.label,
                      style: Theme.of(context).textTheme.labelMedium),
                ],
              ),
            ),
          ),
        );
      },
    );
  }

  _Status _describe() {
    final waiting = repository.pendingCount;
    switch (connection.state) {
      case PiLinkState.connected:
        if (repository.isSyncing || waiting > 0) {
          return const _Status('Syncing', Colors.amber, 'Sending changes');
        }
        if (connection.warning != null) {
          return _Status('Check', Colors.amber, connection.warning!);
        }
        return const _Status(
            'Synced', Colors.green, 'Connected. The Pi has every change.');
      case PiLinkState.connecting:
        return _Status(
          waiting > 0 ? '$waiting unsent' : 'Connecting',
          waiting > 0 ? Colors.orange : Colors.grey,
          waiting > 0
              ? '$waiting change(s) will be sent when the Pi is in range.'
              : 'Looking for the Pi. This retries by itself.',
        );
      case PiLinkState.searching:
        return const _Status('Searching', Colors.grey, 'Scanning for a Pi');
      case PiLinkState.noDevice:
        return const _Status(
            'Set up', Colors.orange, 'No Pi remembered yet. Tap to find it.');
      case PiLinkState.bluetoothOff:
        return const _Status(
            'Bluetooth off', Colors.red, 'Turn on Bluetooth to reach the Pi.');
      case PiLinkState.unsupported:
        return const _Status('No Bluetooth', Colors.red,
            'Bluetooth is unavailable or the app lacks permission.');
    }
  }

  void _showDetails(BuildContext context, _Status status) {
    showModalBottomSheet<void>(
      context: context,
      builder: (sheetContext) => SafeArea(
        child: Padding(
          padding: const EdgeInsets.all(20),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(status.label,
                  style: Theme.of(sheetContext).textTheme.titleMedium),
              const SizedBox(height: 8),
              Text(status.detail),
              if (connection.lastError != null) ...[
                const SizedBox(height: 8),
                Text(connection.lastError!,
                    style: Theme.of(sheetContext).textTheme.bodySmall),
              ],
              const SizedBox(height: 16),
              if (connection.state == PiLinkState.noDevice)
                FilledButton.icon(
                  icon: const Icon(Icons.bluetooth_searching),
                  label: const Text('Find Pi'),
                  onPressed: () {
                    Navigator.of(sheetContext).pop();
                    connection.findPi();
                  },
                )
              else if (connection.state != PiLinkState.unsupported)
                TextButton(
                  onPressed: () {
                    Navigator.of(sheetContext).pop();
                    connection.forgetPi();
                  },
                  child: const Text('Forget this Pi'),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

class _Status {
  const _Status(this.label, this.color, this.detail);
  final String label;
  final Color color;
  final String detail;
}
