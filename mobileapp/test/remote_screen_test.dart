import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:stageweave_remote/connection.dart';
import 'package:stageweave_remote/screens/remote_screen.dart';
import 'package:stageweave_remote/theme.dart';

/// Plays StageWeave's side of the conversation.
class FakeTransport implements Transport {
  final _in = StreamController<String>();
  final sent = <Map<String, dynamic>>[];

  @override
  Stream<String> get incoming => _in.stream;

  @override
  void send(String text) => sent.add(jsonDecode(text) as Map<String, dynamic>);

  void reply(Map<String, dynamic> msg) => _in.add(jsonEncode(msg));

  @override
  Future<void> close() => _in.close();
}

const server = {'name': 'StageWeave on macOS', 'host': 'Studio', 'id': 'abc', 'version': '0.1.4', 'protocol': 1};
const entry = ServerEntry(name: 'StageWeave on macOS', host: 'Studio', address: '10.3.2.4', port: 47800, id: 'abc');

void main() {
  late FakeTransport fake;
  late MemoryTokenStore tokens;
  late RemoteConnection conn;

  setUp(() {
    fake = FakeTransport();
    tokens = MemoryTokenStore();
  });

  // Close without waiting on the transport (the fake clock wouldn't finish it).
  Future<void> done(WidgetTester t) async {
    conn.close();
    await t.pumpWidget(const SizedBox());
  }

  Future<void> show(WidgetTester t) async {
    conn = RemoteConnection(server: entry, deviceName: 'iPhone', tokens: tokens, connect: (_) => fake)..start();
    await t.pumpWidget(MaterialApp(theme: stageTheme(), home: RemoteScreen(connection: conn)));
    await t.pump();
  }

  testWidgets('pairs with the code, then remembers the token', (t) async {
    await show(t);
    expect(fake.sent.single, {'type': 'hello', 'device': 'iPhone', 'token': null, 'v': 1});
    fake.reply({'type': 'pair_required', 'server': server});
    await t.pump();
    expect(find.text('Enter the code shown on the computer'), findsOneWidget);

    await t.enterText(find.byKey(const Key('pin')), '12');
    await t.pump();
    expect(fake.sent.length, 1, reason: 'waits for 4 digits');
    await t.enterText(find.byKey(const Key('pin')), '4821');
    await t.pump();
    expect(fake.sent.last, {'type': 'pair', 'device': 'iPhone', 'pin': '4821'});

    fake.reply({'type': 'pair_failed', 'message': 'That code isn\'t right.'});
    await t.pump();
    expect(find.text('That code isn\'t right.'), findsOneWidget);

    fake.reply({'type': 'paired', 'token': 'tok', 'server': server});
    await t.pump();
    expect(conn.status, LinkStatus.connected);
    expect(await tokens.read('abc'), 'tok');
    await done(t);
  });

  testWidgets('shows the setlist and sends transport, cue and mixer commands', (t) async {
    // A tall phone, so the whole mixer fits.
    t.view.physicalSize = const Size(430, 1600);
    t.view.devicePixelRatio = 1;
    addTearDown(t.view.reset);
    tokens.tokens['abc'] = 'tok';
    await show(t);
    expect(fake.sent.single['token'], 'tok');
    fake.reply({'type': 'welcome', 'server': server});
    fake.reply(jsonDecode(File('test/fixtures/state.json').readAsStringSync()) as Map<String, dynamic>);
    await t.pump();

    // The setlist, with the playing item highlighted.
    expect(find.text('Sunday 15 Nov'), findsOneWidget);
    expect(find.text('Timeless Gospel'), findsOneWidget);
    expect(find.text('Way Maker'), findsNWidgets(2), reason: 'in the list and the now playing bar');
    await t.tap(find.text('Timeless Gospel'));
    expect(fake.sent.last, {'type': 'open_item', 'id': 11});

    // The bar's pause button.
    await t.tap(find.byTooltip('Pause'));
    expect(fake.sent.last, {'type': 'pause'});

    // Full controls.
    await t.tap(find.text('0:42 / 5:01'));
    await t.pumpAndSettle();
    expect(find.text('Chorus'), findsOneWidget);
    await t.tap(find.text('Chorus'));
    expect(fake.sent.last, {'type': 'cue', 'i': 0});
    await t.tap(find.text('Bridge'));
    expect(fake.sent.last, {'type': 'section', 'i': 1});
    await t.tap(find.byTooltip('Mute Drums'));
    expect(fake.sent.last, {'type': 'track', 'i': 1, 'muted': false});
    await t.tap(find.byTooltip('Solo Vocals'));
    expect(fake.sent.last, {'type': 'track', 'i': 0, 'solo': true});
    await t.tap(find.byTooltip('Loop on'));
    expect(fake.sent.last, {'type': 'loop', 'on': false});
    await done(t);
  });

  testWidgets('a slide deck shows the slide and next / previous', (t) async {
    tokens.tokens['abc'] = 'tok';
    await show(t);
    fake.reply({'type': 'welcome', 'server': server});
    fake.reply({
      'type': 'state',
      'state': {
        'projects': [],
        'project': {'id': 1, 'name': 'Sunday', 'items': 1},
        'items': [
          {'id': 5, 'title': 'Welcome', 'artist': '', 'kind': 'deck', 'duration': 0.0, 'ready': true},
        ],
        'current': {
          'id': 5, 'title': 'Welcome', 'artist': '', 'kind': 'deck', 'playing': false, 'loading': false, 'position': 0.0,
          'duration': 0.0, 'loop': false, 'cues': [], 'active_cue': null, 'sections': [], 'current_section': null,
          'tracks': [], 'master_db': 0.0, 'deck': {'index': 2, 'count': 12},
        },
      },
    });
    await t.pump();
    expect(find.text('Slide 3 of 12'), findsOneWidget);
    await t.tap(find.byTooltip('Next'));
    expect(fake.sent.last, {'type': 'next'});
    await done(t);
  });

  testWidgets('Disconnect goes back; Forget also drops the saved code', (t) async {
    tokens.tokens['abc'] = 'tok';
    conn = RemoteConnection(server: entry, deviceName: 'iPhone', tokens: tokens, connect: (_) => fake)..start();
    await t.pumpWidget(MaterialApp(
      theme: stageTheme(),
      home: Builder(
        builder: (context) => TextButton(
          onPressed: () => Navigator.push(context, MaterialPageRoute<void>(builder: (_) => RemoteScreen(connection: conn))),
          child: const Text('Computers'),
        ),
      ),
    ));
    fake.reply({'type': 'welcome', 'server': server});
    await t.pump();
    await t.tap(find.text('Computers'));
    await t.pumpAndSettle();
    await t.tap(find.byTooltip('Disconnect'));
    await t.pumpAndSettle();
    expect(find.text('Computers'), findsOneWidget);
    expect(tokens.tokens['abc'], 'tok', reason: 'still paired');

    await t.tap(find.text('Computers'));
    await t.pumpAndSettle();
    await t.tap(find.byTooltip('More'));
    await t.pumpAndSettle();
    await t.tap(find.text('Forget this computer'));
    await t.pumpAndSettle();
    await t.tap(find.text('Forget'));
    await t.pumpAndSettle();
    expect(find.text('Computers'), findsOneWidget);
    expect(tokens.tokens.containsKey('abc'), isFalse);
    await done(t);
  });
}
