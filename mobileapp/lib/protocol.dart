// Messages between the phone and StageWeave, as JSON over a WebSocket.
// Keep in step with software/src/remote/protocol.rs.

/// Bumped by StageWeave when a change would break older phone apps.
const protocolVersion = 1;

/// The Bonjour/mDNS service StageWeave advertises.
const serviceType = '_stageweave._tcp';

/// First port StageWeave tries (it uses the next few if this one is taken).
const defaultPort = 47800;

double _d(Object? v) => v is num ? v.toDouble() : 0.0;
int _i(Object? v) => v is num ? v.toInt() : 0;
String _s(Object? v) => v is String ? v : '';
bool _b(Object? v) => v == true;
int? _oi(Object? v) => v is num ? v.toInt() : null;
List<Map<String, dynamic>> _list(Object? v) =>
    v is List ? v.whereType<Map>().map((m) => Map<String, dynamic>.from(m)).toList() : const [];

class ServerInfo {
  final String name;
  final String host;
  final String id;
  final String version;
  final int protocol;

  const ServerInfo({required this.name, required this.host, required this.id, required this.version, required this.protocol});

  factory ServerInfo.fromJson(Map<String, dynamic> j) => ServerInfo(
        name: _s(j['name']),
        host: _s(j['host']),
        id: _s(j['id']),
        version: _s(j['version']),
        protocol: _i(j['protocol']),
      );
}

class ProjectInfo {
  final int id;
  final String name;
  final int items;

  const ProjectInfo({required this.id, required this.name, required this.items});

  factory ProjectInfo.fromJson(Map<String, dynamic> j) => ProjectInfo(id: _i(j['id']), name: _s(j['name']), items: _i(j['items']));
}

enum ItemKind { song, youtube, deck }

ItemKind _kind(Object? v) => switch (v) {
      'youtube' => ItemKind.youtube,
      'deck' => ItemKind.deck,
      _ => ItemKind.song,
    };

class ItemInfo {
  final int id;
  final String title;
  final String artist;
  final ItemKind kind;
  final double duration;
  final bool ready;

  const ItemInfo({required this.id, required this.title, required this.artist, required this.kind, required this.duration, required this.ready});

  factory ItemInfo.fromJson(Map<String, dynamic> j) => ItemInfo(
        id: _i(j['id']),
        title: _s(j['title']),
        artist: _s(j['artist']),
        kind: _kind(j['kind']),
        duration: _d(j['duration']),
        ready: _b(j['ready']),
      );
}

class Marker {
  final String name;
  final double start;
  final double end;

  const Marker({required this.name, required this.start, required this.end});

  factory Marker.fromJson(Map<String, dynamic> j) => Marker(name: _s(j['name']), start: _d(j['start']), end: _d(j['end']));
}

class TrackInfo {
  final String name;
  final String kind;
  final bool muted;
  final bool solo;
  final double gainDb;

  const TrackInfo({required this.name, required this.kind, required this.muted, required this.solo, required this.gainDb});

  factory TrackInfo.fromJson(Map<String, dynamic> j) =>
      TrackInfo(name: _s(j['name']), kind: _s(j['kind']), muted: _b(j['muted']), solo: _b(j['solo']), gainDb: _d(j['gain_db']));
}

class DeckInfo {
  final int index;
  final int count;

  const DeckInfo({required this.index, required this.count});

  factory DeckInfo.fromJson(Map<String, dynamic> j) => DeckInfo(index: _i(j['index']), count: _i(j['count']));
}

/// The open setlist item.
class Current {
  final int id;
  final String title;
  final String artist;
  final ItemKind kind;
  final bool playing;
  final bool loading;
  final double position;
  final double duration;
  final bool looping;
  final List<Marker> cues;
  final int? activeCue;
  final List<Marker> sections;
  final int? currentSection;
  final List<TrackInfo> tracks;
  final double masterDb;
  final DeckInfo? deck;

  const Current({
    required this.id,
    required this.title,
    required this.artist,
    required this.kind,
    required this.playing,
    required this.loading,
    required this.position,
    required this.duration,
    required this.looping,
    required this.cues,
    required this.activeCue,
    required this.sections,
    required this.currentSection,
    required this.tracks,
    required this.masterDb,
    required this.deck,
  });

  factory Current.fromJson(Map<String, dynamic> j) => Current(
        id: _i(j['id']),
        title: _s(j['title']),
        artist: _s(j['artist']),
        kind: _kind(j['kind']),
        playing: _b(j['playing']),
        loading: _b(j['loading']),
        position: _d(j['position']),
        duration: _d(j['duration']),
        looping: _b(j['loop']),
        cues: _list(j['cues']).map(Marker.fromJson).toList(),
        activeCue: _oi(j['active_cue']),
        sections: _list(j['sections']).map(Marker.fromJson).toList(),
        currentSection: _oi(j['current_section']),
        tracks: _list(j['tracks']).map(TrackInfo.fromJson).toList(),
        masterDb: _d(j['master_db']),
        deck: j['deck'] is Map ? DeckInfo.fromJson(Map<String, dynamic>.from(j['deck'] as Map)) : null,
      );
}

/// Everything the remote shows.
class RemoteState {
  final List<ProjectInfo> projects;
  final ProjectInfo? project;
  final List<ItemInfo> items;
  final Current? current;

  const RemoteState({required this.projects, required this.project, required this.items, required this.current});

  static const empty = RemoteState(projects: [], project: null, items: [], current: null);

  factory RemoteState.fromJson(Map<String, dynamic> j) => RemoteState(
        projects: _list(j['projects']).map(ProjectInfo.fromJson).toList(),
        project: j['project'] is Map ? ProjectInfo.fromJson(Map<String, dynamic>.from(j['project'] as Map)) : null,
        items: _list(j['items']).map(ItemInfo.fromJson).toList(),
        current: j['current'] is Map ? Current.fromJson(Map<String, dynamic>.from(j['current'] as Map)) : null,
      );
}

/// Phone → StageWeave commands (each is a JSON object with a `type`).
class Cmd {
  static Map<String, dynamic> hello(String device, String? token) => {'type': 'hello', 'device': device, 'token': token, 'v': protocolVersion};
  static Map<String, dynamic> pair(String device, String pin) => {'type': 'pair', 'device': device, 'pin': pin};
  static Map<String, dynamic> openProject(int id) => {'type': 'open_project', 'id': id};
  static Map<String, dynamic> openItem(int id) => {'type': 'open_item', 'id': id};
  static Map<String, dynamic> back() => {'type': 'back'};
  static Map<String, dynamic> play() => {'type': 'play'};
  static Map<String, dynamic> pause() => {'type': 'pause'};
  static Map<String, dynamic> toggle() => {'type': 'toggle'};
  static Map<String, dynamic> stop() => {'type': 'stop'};
  static Map<String, dynamic> seek(double t) => {'type': 'seek', 't': t};
  static Map<String, dynamic> next() => {'type': 'next'};
  static Map<String, dynamic> prev() => {'type': 'prev'};
  static Map<String, dynamic> loop(bool on) => {'type': 'loop', 'on': on};
  static Map<String, dynamic> cue(int i) => {'type': 'cue', 'i': i};
  static Map<String, dynamic> exitCue() => {'type': 'exit_cue'};
  static Map<String, dynamic> section(int i) => {'type': 'section', 'i': i};
  static Map<String, dynamic> track(int i, {bool? muted, bool? solo, double? gainDb}) => {
        'type': 'track',
        'i': i,
        'muted': ?muted,
        'solo': ?solo,
        'gain_db': ?gainDb,
      };
  static Map<String, dynamic> master(double gainDb) => {'type': 'master', 'gain_db': gainDb};
  static Map<String, dynamic> ping() => {'type': 'ping'};
}

/// "3:07"
String mmss(double seconds) {
  final s = seconds.isFinite && seconds > 0 ? seconds.floor() : 0;
  return '${s ~/ 60}:${(s % 60).toString().padLeft(2, '0')}';
}
