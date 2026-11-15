import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:stageweave_remote/protocol.dart';
import 'package:stageweave_remote/screens/remote_screen.dart';

void main() {
  // The same file is checked by StageWeave's Rust tests (software/src/remote/mod.rs).
  test('parses the state StageWeave sends', () {
    final msg = jsonDecode(File('test/fixtures/state.json').readAsStringSync()) as Map<String, dynamic>;
    expect(msg['type'], 'state');
    final s = RemoteState.fromJson(msg['state'] as Map<String, dynamic>);
    expect(s.projects.map((p) => p.name), ['Sunday 15 Nov', 'Youth Night']);
    expect(s.project!.id, 1);
    expect(s.items.map((i) => i.kind), [ItemKind.song, ItemKind.youtube, ItemKind.deck]);
    expect(s.items.last.ready, isFalse);
    final c = s.current!;
    expect(c.title, 'Way Maker');
    expect(c.playing && c.looping, isTrue);
    expect(c.position, 42.3);
    expect(c.cues.single.name, 'Chorus');
    expect(c.activeCue, isNull);
    expect(c.currentSection, 0);
    expect(c.sections[1].start, 180.0);
    expect(c.tracks[1].muted, isTrue);
    expect(c.tracks[1].gainDb, -6.0);
    expect(c.deck, isNull);
  });

  test('commands use the names StageWeave expects', () {
    expect(Cmd.track(1, muted: true), {'type': 'track', 'i': 1, 'muted': true});
    expect(Cmd.exitCue(), {'type': 'exit_cue'});
    expect(Cmd.openItem(7), {'type': 'open_item', 'id': 7});
    expect(Cmd.hello('iPhone', null)['v'], protocolVersion);
  });

  test('faders use the desktop curve', () {
    for (final db in [-60.0, -12.0, 0.0, 6.0]) {
      expect(fracToDb(dbToFrac(db)), closeTo(db, 1e-9));
    }
    expect(mmss(187.9), '3:07');
  });
}
