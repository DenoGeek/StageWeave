import 'dart:async';

import 'package:bonsoir/bonsoir.dart';
import 'package:flutter/foundation.dart';

import 'connection.dart';
import 'protocol.dart';

/// Finds StageWeave computers on this Wi-Fi over Bonjour/mDNS.
class Discovery extends ChangeNotifier {
  BonsoirDiscovery? _discovery;
  StreamSubscription<BonsoirDiscoveryEvent>? _sub;
  final Map<String, ServerEntry> _found = {};
  bool searching = false;
  String? error;

  List<ServerEntry> get servers => _found.values.toList()..sort((a, b) => a.host.toLowerCase().compareTo(b.host.toLowerCase()));

  Future<void> start() async {
    await stop();
    _found.clear();
    error = null;
    searching = true;
    notifyListeners();
    try {
      final d = BonsoirDiscovery(type: serviceType);
      _discovery = d;
      await d.initialize();
      _sub = d.eventStream?.listen((event) {
        switch (event) {
          case BonsoirDiscoveryServiceFoundEvent():
            // The address and port come with the resolved service.
            d.serviceResolver.resolveService(event.service);
          case BonsoirDiscoveryServiceResolvedEvent():
          case BonsoirDiscoveryServiceUpdatedEvent():
            final s = event.service;
            if (s != null) _add(s);
          case BonsoirDiscoveryServiceLostEvent():
            _found.remove(event.service.name);
            notifyListeners();
          default:
            break;
        }
      });
      await d.start();
    } catch (e) {
      error = 'Couldn\'t search this network ($e)';
      searching = false;
      notifyListeners();
    }
  }

  void _add(BonsoirService s) {
    // Prefer a LAN IPv4 address: easier to read, and what StageWeave shows in
    // Settings. Loopback only works when the phone is the computer (simulator).
    int rank(String a) => (a.startsWith('127.') || a == '::1' ? 4 : 0) + (a.startsWith('169.254.') || a.toLowerCase().startsWith('fe80') ? 2 : 0) + (a.contains(':') ? 1 : 0);
    final addresses = [...s.hostAddresses.where((a) => !a.contains('%') && a != '::1')]..sort((a, b) => rank(a) - rank(b));
    final address = addresses.firstOrNull;
    if (address == null) return;
    final a = s.attributes;
    _found[s.name] = ServerEntry(
      name: a['name']?.isNotEmpty == true ? a['name']! : s.name,
      host: a['host']?.isNotEmpty == true ? a['host']! : (s.hostname ?? '').replaceAll('.local.', '').replaceAll('.local', ''),
      address: address.contains(':') ? '[$address]' : address,
      port: s.port,
      id: a['id']?.isNotEmpty == true ? a['id'] : null,
    );
    notifyListeners();
  }

  Future<void> stop() async {
    await _sub?.cancel();
    _sub = null;
    final d = _discovery;
    _discovery = null;
    if (d != null) {
      try {
        await d.stop();
      } catch (_) {}
    }
    searching = false;
  }

  @override
  void dispose() {
    stop();
    super.dispose();
  }
}
