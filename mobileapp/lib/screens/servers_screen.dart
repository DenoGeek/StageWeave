import 'package:flutter/material.dart';
import 'package:shared_preferences/shared_preferences.dart';

import '../connection.dart';
import '../dev.dart';
import '../discovery.dart';
import '../protocol.dart';
import '../theme.dart';
import 'remote_screen.dart';

/// Lists StageWeave computers on this Wi-Fi.
class ServersScreen extends StatefulWidget {
  final Discovery discovery;
  final TokenStore tokens;
  final String deviceName;

  const ServersScreen({super.key, required this.discovery, required this.tokens, required this.deviceName});

  @override
  State<ServersScreen> createState() => _ServersScreenState();
}

class _ServersScreenState extends State<ServersScreen> {
  List<ServerEntry> _manual = [];
  bool _autoOpened = false;

  @override
  void initState() {
    super.initState();
    widget.discovery.start();
    widget.discovery.addListener(_autoOpen);
    _loadManual();
  }

  @override
  void dispose() {
    widget.discovery.removeListener(_autoOpen);
    super.dispose();
  }

  void _autoOpen() {
    final want = Dev.autoOpen;
    if (want == null || _autoOpened || !mounted) return;
    final port = int.tryParse(want);
    final match = widget.discovery.servers.where((s) => port == null || port < 1000 || s.port == port).firstOrNull;
    if (match != null) {
      _autoOpened = true;
      _open(match);
    }
  }

  Future<void> _loadManual() async {
    try {
      final p = await SharedPreferences.getInstance();
      final saved = p.getStringList('manual_servers') ?? [];
      setState(() => _manual = saved.map(_parse).whereType<ServerEntry>().toList());
    } catch (_) {}
  }

  static ServerEntry? _parse(String text) {
    final t = text.trim();
    if (t.isEmpty) return null;
    final i = t.lastIndexOf(':');
    final hasPort = i > 0 && !t.endsWith(']') && int.tryParse(t.substring(i + 1)) != null;
    final address = hasPort ? t.substring(0, i) : t;
    final port = hasPort ? int.parse(t.substring(i + 1)) : defaultPort;
    return ServerEntry(name: 'StageWeave', host: address, address: address, port: port);
  }

  Future<void> _addManual() async {
    final controller = TextEditingController();
    final entry = await showDialog<ServerEntry>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Enter address'),
        content: Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
          const Text('Shown in StageWeave → Settings → Phone remote.', style: TextStyle(color: textDim, fontSize: 13)),
          const SizedBox(height: 12),
          TextField(
            controller: controller,
            autofocus: true,
            keyboardType: TextInputType.url,
            decoration: const InputDecoration(hintText: '192.168.1.20:47800', border: OutlineInputBorder()),
            onSubmitted: (v) => Navigator.pop(context, _parse(v)),
          ),
        ]),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(context, _parse(controller.text)), child: const Text('Connect')),
        ],
      ),
    );
    if (entry == null || !mounted) return;
    setState(() => _manual = [entry, ..._manual.where((m) => m.endpoint != entry.endpoint)].take(5).toList());
    try {
      final p = await SharedPreferences.getInstance();
      await p.setStringList('manual_servers', _manual.map((m) => m.endpoint).toList());
    } catch (_) {}
    if (mounted) _open(entry);
  }

  void _open(ServerEntry server) {
    final conn = RemoteConnection(server: server, deviceName: widget.deviceName, tokens: widget.tokens)..start();
    widget.discovery.stop();
    Navigator.of(context).push(MaterialPageRoute(builder: (_) => RemoteScreen(connection: conn))).then((_) {
      // Disconnected: look for computers again.
      conn.dispose();
      if (mounted) widget.discovery.start();
    });
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('StageWeave Remote'),
        actions: [
          ListenableBuilder(
            listenable: widget.discovery,
            builder: (context, _) => IconButton(
              tooltip: 'Scan again',
              icon: const Icon(Icons.refresh_rounded),
              onPressed: widget.discovery.start,
            ),
          ),
        ],
      ),
      body: ListenableBuilder(
        listenable: widget.discovery,
        builder: (context, _) {
          final found = widget.discovery.servers;
          final manual = _manual.where((m) => !found.any((f) => f.endpoint == m.endpoint)).toList();
          return RefreshIndicator(
            onRefresh: widget.discovery.start,
            child: ListView(
              children: [
                const Padding(
                  padding: EdgeInsets.fromLTRB(20, 8, 20, 4),
                  child: Text('On this Wi-Fi', style: TextStyle(color: textDim, fontWeight: FontWeight.w600)),
                ),
                for (final s in found) _ServerTile(server: s, onTap: () => _open(s)),
                if (found.isNotEmpty && widget.discovery.searching)
                  const Padding(
                    padding: EdgeInsets.fromLTRB(20, 4, 20, 0),
                    child: Text('Still looking for others… pull down or tap ⟳ to scan again.', style: TextStyle(color: textFaint, fontSize: 12)),
                  ),
                if (found.isEmpty)
                  Padding(
                    padding: const EdgeInsets.fromLTRB(20, 12, 20, 12),
                    child: Row(children: [
                      if (widget.discovery.searching) const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2)),
                      if (widget.discovery.searching) const SizedBox(width: 12),
                      Expanded(
                        child: Text(
                          widget.discovery.error ??
                              'Looking for StageWeave… Make sure this phone is on the same Wi-Fi, and Phone remote is on in StageWeave\'s Settings.',
                          style: const TextStyle(color: textDim),
                        ),
                      ),
                    ]),
                  ),
                if (found.isEmpty)
                  Padding(
                    padding: const EdgeInsets.symmetric(horizontal: 20),
                    child: Align(
                      alignment: Alignment.centerLeft,
                      child: OutlinedButton.icon(onPressed: widget.discovery.start, icon: const Icon(Icons.refresh_rounded), label: const Text('Scan again')),
                    ),
                  ),
                if (manual.isNotEmpty) ...[
                  const Padding(
                    padding: EdgeInsets.fromLTRB(20, 16, 20, 4),
                    child: Text('Recent', style: TextStyle(color: textDim, fontWeight: FontWeight.w600)),
                  ),
                  for (final s in manual) _ServerTile(server: s, onTap: () => _open(s)),
                ],
                const SizedBox(height: 8),
                ListTile(
                  leading: const Icon(Icons.keyboard_rounded),
                  title: const Text('Enter address'),
                  subtitle: const Text('If StageWeave doesn\'t show up', style: TextStyle(color: textFaint)),
                  onTap: _addManual,
                ),
              ],
            ),
          );
        },
      ),
    );
  }
}

class _ServerTile extends StatelessWidget {
  final ServerEntry server;
  final VoidCallback onTap;

  const _ServerTile({required this.server, required this.onTap});

  @override
  Widget build(BuildContext context) {
    final mac = server.name.contains('macOS');
    return ListTile(
      leading: CircleAvatar(
        backgroundColor: bgWidget,
        child: Icon(mac ? Icons.laptop_mac_rounded : Icons.computer_rounded, color: accentBright),
      ),
      title: Text(server.name, style: const TextStyle(fontWeight: FontWeight.w600)),
      subtitle: Text(
        server.host.isNotEmpty && server.host != server.address ? '${server.endpoint} · ${server.host}' : server.endpoint,
        style: const TextStyle(color: textDim),
      ),
      trailing: const Icon(Icons.chevron_right_rounded),
      onTap: onTap,
    );
  }
}
