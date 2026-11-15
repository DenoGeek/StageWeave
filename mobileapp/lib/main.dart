import 'dart:io' show Platform;

import 'package:flutter/material.dart';

import 'connection.dart';
import 'discovery.dart';
import 'screens/servers_screen.dart';
import 'theme.dart';

/// StageWeave Remote: control StageWeave on a computer on the same Wi-Fi.
void main() {
  runApp(const RemoteApp());
}

class RemoteApp extends StatefulWidget {
  const RemoteApp({super.key});

  @override
  State<RemoteApp> createState() => _RemoteAppState();
}

class _RemoteAppState extends State<RemoteApp> {
  final _discovery = Discovery();
  final _tokens = PrefsTokenStore();

  @override
  void dispose() {
    _discovery.dispose();
    super.dispose();
  }

  /// Shown on the computer when pairing ("iPhone", "Android phone").
  static String get _deviceName {
    try {
      if (Platform.isIOS) return 'iPhone';
      if (Platform.isAndroid) return 'Android phone';
    } catch (_) {}
    return 'Phone';
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'StageWeave Remote',
      debugShowCheckedModeBanner: false,
      theme: stageTheme(),
      home: ServersScreen(discovery: _discovery, tokens: _tokens, deviceName: _deviceName),
    );
  }
}
