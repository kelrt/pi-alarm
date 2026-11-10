import 'package:flutter/material.dart';

import '../ble/pi_client.dart';
import '../ble/pi_connection.dart';

/// One button per action the Pi offers (fan off, fan 3, light, ...).
/// Pressing one transmits immediately. Disabled while the Pi is out of
/// range: a manual press that arrives minutes later would be worse than
/// none.
class ManualButtons extends StatefulWidget {
  const ManualButtons({super.key, required this.connection});

  final PiConnection connection;

  @override
  State<ManualButtons> createState() => _ManualButtonsState();
}

class _ManualButtonsState extends State<ManualButtons> {
  /// The action being sent right now, to show progress on its button.
  String? _busy;

  Future<void> _press(PiAction action) async {
    setState(() => _busy = action.name);
    String? problem;
    try {
      await widget.connection.request('action', {'name': action.name});
    } on PiRejected catch (error) {
      problem = error.message;
    } on PiUnavailable catch (error) {
      problem = error.message;
    }
    // The widget may have been removed while we were waiting.
    if (!mounted) return;
    setState(() => _busy = null);
    if (problem != null) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('${action.label}: $problem')),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: widget.connection,
      builder: (context, _) {
        final connected = widget.connection.isConnected;
        return Wrap(
          spacing: 8,
          runSpacing: 8,
          children: [
            for (final action in widget.connection.actions)
              FilledButton.tonal(
                onPressed: connected && _busy == null
                    ? () => _press(action)
                    : null,
                child: _busy == action.name
                    ? const SizedBox(
                        width: 16,
                        height: 16,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : Text(action.label),
              ),
          ],
        );
      },
    );
  }
}
