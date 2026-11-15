import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:shared_preferences/shared_preferences.dart';
import 'package:web_socket_channel/web_socket_channel.dart';

import 'protocol.dart';

/// A StageWeave found on the network (or typed in).
class ServerEntry {
  /// "StageWeave on macOS"
  final String name;

  /// The computer's name.
  final String host;
  final String address;
  final int port;

  /// StageWeave's id, when known (from Bonjour); used to remember the pairing.
  final String? id;

  const ServerEntry({required this.name, required this.host, required this.address, required this.port, this.id});

  String get endpoint => '$address:$port';

  /// Key the pairing token is saved under.
  String get key => id ?? endpoint;
}

/// A text message pipe; a WebSocket in the app, a fake in tests.
abstract class Transport {
  Stream<String> get incoming;
  void send(String text);
  Future<void> close();
}

class WebSocketTransport implements Transport {
  final WebSocketChannel _channel;

  WebSocketTransport(Uri uri) : _channel = WebSocketChannel.connect(uri);

  @override
  Stream<String> get incoming => _channel.ready.asStream().asyncExpand((_) => _channel.stream).map((m) => m is String ? m : utf8.decode(m as List<int>));

  @override
  void send(String text) => _channel.sink.add(text);

  @override
  Future<void> close() => _channel.sink.close();
}

typedef TransportFactory = Transport Function(ServerEntry server);

/// Where pairing tokens are kept (shared preferences in the app).
abstract class TokenStore {
  Future<String?> read(String key);
  Future<void> write(String key, String? token);
}

class PrefsTokenStore implements TokenStore {
  @override
  Future<String?> read(String key) async => (await SharedPreferences.getInstance()).getString('token:$key');

  @override
  Future<void> write(String key, String? token) async {
    final p = await SharedPreferences.getInstance();
    token == null ? await p.remove('token:$key') : await p.setString('token:$key', token);
  }
}

class MemoryTokenStore implements TokenStore {
  final Map<String, String> tokens = {};

  @override
  Future<String?> read(String key) async => tokens[key];

  @override
  Future<void> write(String key, String? token) async => token == null ? tokens.remove(key) : tokens[key] = token;
}

enum LinkStatus { connecting, needsCode, connected, offline }

/// One connection to StageWeave: pairing, state updates and commands.
/// Reconnects by itself until [close] is called.
class RemoteConnection extends ChangeNotifier {
  final ServerEntry server;
  final String deviceName;
  final TokenStore tokens;
  final TransportFactory _connect;

  RemoteConnection({required this.server, required this.deviceName, required this.tokens, TransportFactory? connect})
      : _connect = connect ?? ((s) => WebSocketTransport(Uri.parse('ws://${s.endpoint}')));

  LinkStatus status = LinkStatus.connecting;
  RemoteState state = RemoteState.empty;
  ServerInfo? info;

  /// Last pairing or connection problem, for the screen.
  String? error;

  Transport? _transport;
  StreamSubscription<String>? _sub;
  Timer? _retry;
  Timer? _ping;
  int _attempt = 0;
  bool _closed = false;

  void start() {
    _closed = false;
    _open();
  }

  Future<void> _open() async {
    _retry?.cancel();
    if (_closed) return;
    status = _attempt == 0 ? LinkStatus.connecting : LinkStatus.offline;
    notifyListeners();
    final t = _connect(server);
    _transport = t;
    _sub = t.incoming.listen(_onMessage, onError: (_) => _lost(), onDone: _lost, cancelOnError: true);
    final token = await tokens.read(server.key);
    if (_transport != t) return;
    _send(Cmd.hello(deviceName, token));
  }

  void _lost() {
    _ping?.cancel();
    _sub?.cancel();
    _sub = null;
    _transport = null;
    if (_closed) return;
    status = LinkStatus.offline;
    notifyListeners();
    // Back off up to 8 s between attempts.
    final delay = Duration(milliseconds: (500 * (1 << _attempt.clamp(0, 4))));
    _attempt++;
    _retry = Timer(delay, _open);
  }

  void _onMessage(String text) {
    final Map<String, dynamic> m;
    try {
      m = Map<String, dynamic>.from(jsonDecode(text) as Map);
    } catch (_) {
      return;
    }
    switch (m['type']) {
      case 'welcome':
        info = ServerInfo.fromJson(Map<String, dynamic>.from(m['server'] as Map));
        _connected();
      case 'pair_required':
        info = ServerInfo.fromJson(Map<String, dynamic>.from(m['server'] as Map));
        _attempt = 0;
        status = LinkStatus.needsCode;
        notifyListeners();
      case 'paired':
        info = ServerInfo.fromJson(Map<String, dynamic>.from(m['server'] as Map));
        final token = m['token'] as String;
        tokens.write(server.key, token);
        if (info!.id.isNotEmpty) tokens.write(info!.id, token);
        _connected();
      case 'pair_failed':
        error = m['message'] as String?;
        notifyListeners();
      case 'state':
        state = RemoteState.fromJson(Map<String, dynamic>.from(m['state'] as Map));
        notifyListeners();
      case 'error':
        error = m['message'] as String?;
        notifyListeners();
    }
  }

  void _connected() {
    _attempt = 0;
    error = null;
    status = LinkStatus.connected;
    // Keeps the connection (and Wi-Fi) awake, and finds a dead link sooner.
    _ping?.cancel();
    _ping = Timer.periodic(const Duration(seconds: 10), (_) => _send(Cmd.ping()));
    notifyListeners();
  }

  void _send(Map<String, dynamic> msg) => _transport?.send(jsonEncode(msg));

  /// Enter the code shown on the computer.
  void pair(String pin) {
    error = null;
    notifyListeners();
    _send(Cmd.pair(deviceName, pin));
  }

  /// Forget the pairing, so this phone needs the code next time.
  Future<void> forget() async {
    await tokens.write(server.key, null);
    final id = info?.id;
    if (id != null && id.isNotEmpty) await tokens.write(id, null);
  }

  /// Send a command (ignored until connected).
  void send(Map<String, dynamic> cmd) {
    if (status == LinkStatus.connected) _send(cmd);
  }

  Future<void> close() async {
    _closed = true;
    _retry?.cancel();
    _ping?.cancel();
    await _sub?.cancel();
    await _transport?.close();
    _transport = null;
  }

  @override
  void dispose() {
    close();
    super.dispose();
  }
}
