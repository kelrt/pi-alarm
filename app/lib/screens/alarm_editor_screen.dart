import 'package:flutter/material.dart';

import '../ble/pi_connection.dart';
import '../data/alarm_repository.dart';
import '../models/alarm.dart';

/// Edit one alarm: time, days, label and the list of steps.
///
/// Nothing is sent while editing. "Save" hands the alarm to the repository,
/// which stores it on the phone and syncs it to the Pi (now, or when the Pi
/// is next in range).
class AlarmEditorScreen extends StatefulWidget {
  const AlarmEditorScreen({
    super.key,
    required this.alarm,
    required this.isNew,
    required this.repository,
    required this.actions,
  });

  final Alarm alarm;
  final bool isNew;
  final AlarmRepository repository;

  /// The actions a step can use.
  final List<PiAction> actions;

  @override
  State<AlarmEditorScreen> createState() => _AlarmEditorScreenState();
}

class _AlarmEditorScreenState extends State<AlarmEditorScreen> {
  /// The alarm being edited. Replaced by a changed copy on every edit.
  late Alarm _draft;
  late final TextEditingController _labelController;

  @override
  void initState() {
    super.initState();
    _draft = widget.alarm;
    _labelController = TextEditingController(text: widget.alarm.label);
  }

  @override
  void dispose() {
    _labelController.dispose();
    super.dispose();
  }

  Future<void> _pickTime() async {
    final picked = await showTimePicker(
      context: context,
      initialTime: TimeOfDay(hour: _draft.hour, minute: _draft.minute),
    );
    if (picked == null) return;
    setState(() {
      _draft = _draft.copyWith(hour: picked.hour, minute: picked.minute);
    });
  }

  void _toggleDay(String day) {
    final days = List.of(_draft.days);
    if (days.contains(day)) {
      days.remove(day);
    } else {
      days.add(day);
    }
    // Keep them in Monday-first order, as the Pi stores them.
    days.sort((a, b) => dayNames.indexOf(a).compareTo(dayNames.indexOf(b)));
    setState(() => _draft = _draft.copyWith(days: days));
  }

  void _changeStep(int index, AlarmStep step) {
    final steps = List.of(_draft.steps);
    steps[index] = step;
    setState(() => _draft = _draft.copyWith(steps: steps));
  }

  void _removeStep(int index) {
    final steps = List.of(_draft.steps)..removeAt(index);
    setState(() => _draft = _draft.copyWith(steps: steps));
  }

  void _addStep() {
    final steps = List.of(_draft.steps)
      ..add(AlarmStep(offsetS: 0, action: widget.actions.first.name));
    setState(() => _draft = _draft.copyWith(steps: steps));
  }

  Future<void> _save() async {
    // Steps run in time order; store them that way, as the Pi does.
    final steps = List.of(_draft.steps);
    // A stable sort is needed so steps at the same offset keep their order;
    // Dart's List.sort is not guaranteed stable, so sort by (offset, index).
    final indexed = [
      for (var i = 0; i < steps.length; i++) (i, steps[i]),
    ];
    indexed.sort((a, b) {
      final byOffset = a.$2.offsetS.compareTo(b.$2.offsetS);
      return byOffset != 0 ? byOffset : a.$1.compareTo(b.$1);
    });
    final alarm = _draft.copyWith(
      label: _labelController.text.trim(),
      steps: [for (final entry in indexed) entry.$2],
    );

    final problem = alarm.problem;
    if (problem != null) {
      ScaffoldMessenger.of(context)
          .showSnackBar(SnackBar(content: Text(problem)));
      return;
    }
    await widget.repository.save(alarm);
    if (mounted) Navigator.of(context).pop();
  }

  Future<void> _delete() async {
    await widget.repository.delete(_draft.id);
    if (mounted) Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(
        title: Text(widget.isNew ? 'New alarm' : 'Edit alarm'),
        actions: [
          if (!widget.isNew)
            IconButton(
              tooltip: 'Delete alarm',
              icon: const Icon(Icons.delete_outline),
              onPressed: _delete,
            ),
          TextButton(onPressed: _save, child: const Text('Save')),
        ],
      ),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          // --- Time ---
          Center(
            child: TextButton(
              onPressed: _pickTime,
              child: Text(_draft.timeText, style: theme.textTheme.displayMedium),
            ),
          ),
          const SizedBox(height: 8),

          // --- Days ---
          Wrap(
            alignment: WrapAlignment.center,
            spacing: 6,
            children: [
              for (var i = 0; i < dayNames.length; i++)
                FilterChip(
                  label: Text(dayLabels[i]),
                  selected: _draft.days.contains(dayNames[i]),
                  onSelected: (_) => _toggleDay(dayNames[i]),
                ),
            ],
          ),
          const SizedBox(height: 16),

          // --- Label ---
          TextField(
            controller: _labelController,
            maxLength: 40,
            decoration: const InputDecoration(
              labelText: 'Label (optional)',
              border: OutlineInputBorder(),
            ),
          ),
          const SizedBox(height: 8),

          // --- Steps ---
          Text('Steps', style: theme.textTheme.titleMedium),
          const SizedBox(height: 4),
          for (var i = 0; i < _draft.steps.length; i++)
            _StepRow(
              step: _draft.steps[i],
              actions: widget.actions,
              onChanged: (step) => _changeStep(i, step),
              onRemove: () => _removeStep(i),
            ),
          Align(
            alignment: Alignment.centerLeft,
            child: TextButton.icon(
              onPressed: _draft.steps.length < 8 ? _addStep : null,
              icon: const Icon(Icons.add),
              label: const Text('Add step'),
            ),
          ),
          const SizedBox(height: 8),
          SwitchListTile(
            contentPadding: EdgeInsets.zero,
            title: const Text('Enabled'),
            value: _draft.enabled,
            onChanged: (enabled) =>
                setState(() => _draft = _draft.copyWith(enabled: enabled)),
          ),
        ],
      ),
    );
  }
}

/// One step: which action, and when relative to the alarm time.
class _StepRow extends StatelessWidget {
  const _StepRow({
    required this.step,
    required this.actions,
    required this.onChanged,
    required this.onRemove,
  });

  final AlarmStep step;
  final List<PiAction> actions;
  final ValueChanged<AlarmStep> onChanged;
  final VoidCallback onRemove;

  /// Move the step by [minutes] (negative = earlier).
  void _shift(int minutes) {
    final next = step.offsetS + minutes * 60;
    if (next.abs() > 6 * 3600) return;
    onChanged(step.copyWith(offsetS: next));
  }

  @override
  Widget build(BuildContext context) {
    // If the alarm uses an action the Pi no longer lists, still show it.
    final known = actions.any((action) => action.name == step.action);
    return Card(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(12, 4, 4, 4),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Expanded(
                  child: DropdownButton<String>(
                    isExpanded: true,
                    value: step.action,
                    underline: const SizedBox.shrink(),
                    items: [
                      for (final action in actions)
                        DropdownMenuItem(
                          value: action.name,
                          child: Text(action.label),
                        ),
                      if (!known)
                        DropdownMenuItem(
                          value: step.action,
                          child: Text('${step.action} (unknown)'),
                        ),
                    ],
                    onChanged: (name) {
                      if (name != null) onChanged(step.copyWith(action: name));
                    },
                  ),
                ),
                IconButton(
                  tooltip: 'Remove step',
                  icon: const Icon(Icons.close),
                  onPressed: onRemove,
                ),
              ],
            ),
            Row(
              children: [
                IconButton(
                  tooltip: '5 minutes earlier',
                  icon: const Icon(Icons.keyboard_double_arrow_left),
                  onPressed: () => _shift(-5),
                ),
                IconButton(
                  tooltip: '1 minute earlier',
                  icon: const Icon(Icons.chevron_left),
                  onPressed: () => _shift(-1),
                ),
                Expanded(
                  child: Text(step.offsetText, textAlign: TextAlign.center),
                ),
                IconButton(
                  tooltip: '1 minute later',
                  icon: const Icon(Icons.chevron_right),
                  onPressed: () => _shift(1),
                ),
                IconButton(
                  tooltip: '5 minutes later',
                  icon: const Icon(Icons.keyboard_double_arrow_right),
                  onPressed: () => _shift(5),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}
