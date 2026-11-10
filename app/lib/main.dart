import 'package:flutter/material.dart';

import 'ble/pi_connection.dart';
import 'data/alarm_repository.dart';
import 'data/local_store.dart';
import 'screens/home_screen.dart';

/// App start-up.
///
/// Three long-lived objects are created here and handed down to the screens:
///
/// * [PrefsStore]      what the phone remembers between launches
/// * [PiConnection]    the BLE link; connects and retries by itself
/// * [AlarmRepository] the alarm list plus the queue of unsent changes
Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();

  final store = await PrefsStore.open();
  final connection = PiConnection(store);
  final repository = AlarmRepository(connection, store);

  // Not awaited: the UI appears immediately and the status indicator shows
  // the connection coming up in the background.
  connection.start();

  runApp(PiAlarmApp(connection: connection, repository: repository));
}

class PiAlarmApp extends StatelessWidget {
  const PiAlarmApp({
    super.key,
    required this.connection,
    required this.repository,
  });

  final PiConnection connection;
  final AlarmRepository repository;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Pi Alarm',
      theme: ThemeData(
        colorSchemeSeed: Colors.indigo,
        useMaterial3: true,
      ),
      darkTheme: ThemeData(
        colorSchemeSeed: Colors.indigo,
        brightness: Brightness.dark,
        useMaterial3: true,
      ),
      // A bedroom app: follow the phone's dark mode.
      themeMode: ThemeMode.system,
      home: HomeScreen(connection: connection, repository: repository),
    );
  }
}
