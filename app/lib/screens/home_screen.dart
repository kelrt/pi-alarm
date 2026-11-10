import 'package:flutter/material.dart';

import '../ble/pi_connection.dart';
import '../data/alarm_repository.dart';
import '../models/alarm.dart';
import '../widgets/manual_buttons.dart';
import '../widgets/status_indicator.dart';
import 'alarm_editor_screen.dart';

/// The one main screen: manual buttons on top, the alarm list below.
class HomeScreen extends StatelessWidget {
  const HomeScreen({
    super.key,
    required this.connection,
    required this.repository,
  });

  final PiConnection connection;
  final AlarmRepository repository;

  Future<void> _openEditor(BuildContext context, Alarm alarm,
      {required bool isNew}) async {
    await Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => AlarmEditorScreen(
          alarm: alarm,
          isNew: isNew,
          repository: repository,
          actions: connection.actions,
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Pi Alarm'),
        actions: [
          StatusIndicator(connection: connection, repository: repository),
        ],
      ),
      floatingActionButton: FloatingActionButton(
        tooltip: 'New alarm',
        onPressed: () => _openEditor(context, Alarm.blank(), isNew: true),
        child: const Icon(Icons.add),
      ),
      body: ListenableBuilder(
        listenable: Listenable.merge([connection, repository]),
        builder: (context, _) {
          _showRejectionOnce(context);
          final alarms = repository.alarms;
          return ListView(
            padding: const EdgeInsets.only(bottom: 88),
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
                child: ManualButtons(connection: connection),
              ),
              const Divider(),
              if (alarms.isEmpty)
                const Padding(
                  padding: EdgeInsets.all(32),
                  child: Center(child: Text('No alarms yet. Tap + to add one.')),
                ),
              for (final alarm in alarms)
                _AlarmTile(
                  alarm: alarm,
                  pending: repository.isPending(alarm.id),
                  onTap: () => _openEditor(context, alarm, isNew: false),
                  onToggle: (enabled) =>
                      repository.save(alarm.copyWith(enabled: enabled)),
                ),
            ],
          );
        },
      ),
    );
  }

  /// If the Pi refused a change, say so in a snackbar, once.
  void _showRejectionOnce(BuildContext context) {
    final message = repository.lastRejection;
    if (message == null) return;
    repository.lastRejection = null;
    // We are in the middle of building; show the snackbar afterwards.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!context.mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('The Pi refused a change: $message')),
      );
    });
  }
}

class _AlarmTile extends StatelessWidget {
  const _AlarmTile({
    required this.alarm,
    required this.pending,
    required this.onTap,
    required this.onToggle,
  });

  final Alarm alarm;

  /// True while this alarm has a change the Pi has not confirmed.
  final bool pending;
  final VoidCallback onTap;
  final ValueChanged<bool> onToggle;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final faded = alarm.enabled ? null : theme.disabledColor;
    final details = [
      alarm.daysText,
      if (alarm.label.isNotEmpty) alarm.label,
    ].join('  ·  ');

    return ListTile(
      onTap: onTap,
      title: Text(
        alarm.timeText,
        style: theme.textTheme.headlineMedium?.copyWith(color: faded),
      ),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(details),
          // Says plainly whether this alarm has reached the Pi.
          Row(
            children: [
              Icon(
                pending ? Icons.cloud_upload_outlined : Icons.cloud_done_outlined,
                size: 14,
                color: pending ? Colors.orange : Colors.green,
              ),
              const SizedBox(width: 4),
              Text(
                pending ? 'Not on the Pi yet' : 'On the Pi',
                style: theme.textTheme.bodySmall,
              ),
            ],
          ),
        ],
      ),
      trailing: Switch(value: alarm.enabled, onChanged: onToggle),
    );
  }
}
