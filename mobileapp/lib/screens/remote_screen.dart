import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../connection.dart';
import '../dev.dart';
import '../protocol.dart';
import '../theme.dart';

/// Controls for one StageWeave: pairing, the setlist and what's playing.
class RemoteScreen extends StatefulWidget {
  final RemoteConnection connection;

  const RemoteScreen({super.key, required this.connection});

  @override
  State<RemoteScreen> createState() => _RemoteScreenState();
}

class _RemoteScreenState extends State<RemoteScreen> {
  RemoteConnection get connection => widget.connection;
  bool _shownNowPlaying = false;

  @override
  void initState() {
    super.initState();
    connection.addListener(_devScreen);
  }

  @override
  void dispose() {
    connection.removeListener(_devScreen);
    super.dispose();
  }

  /// Screenshot hook: open the full controls once something is open.
  void _devScreen() {
    if (Dev.screen == 'now_playing' && !_shownNowPlaying && connection.state.current != null && mounted) {
      _shownNowPlaying = true;
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) _openNowPlaying(context);
      });
    }
  }

  Future<void> _forget() async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Forget this computer?'),
        content: const Text('You\'ll need to enter the code shown on the computer to connect again.'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(context, true), child: const Text('Forget')),
        ],
      ),
    );
    if (ok != true) return;
    await connection.forget();
    if (mounted) Navigator.pop(context);
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: connection,
      builder: (context, _) {
        final c = connection;
        final state = c.state;
        final title = switch (c.status) {
          LinkStatus.needsCode => 'Pair with ${c.info?.host ?? c.server.host}',
          _ when state.project != null => state.project!.name,
          _ => c.info?.host ?? c.server.host,
        };
        return Scaffold(
          appBar: AppBar(
            leading: IconButton(
              tooltip: 'Disconnect',
              icon: const Icon(Icons.close_rounded),
              onPressed: () => Navigator.pop(context),
            ),
            title: Text(title, overflow: TextOverflow.ellipsis),
            actions: [
              if (c.status == LinkStatus.connected && state.projects.isNotEmpty)
                IconButton(
                  tooltip: 'Setlists',
                  icon: const Icon(Icons.library_music_rounded),
                  onPressed: () => _pickProject(context),
                ),
              PopupMenuButton<String>(
                tooltip: 'More',
                onSelected: (v) => v == 'forget' ? _forget() : Navigator.pop(context),
                itemBuilder: (context) => [
                  PopupMenuItem(
                    value: 'disconnect',
                    child: ListTile(
                      leading: const Icon(Icons.link_off_rounded),
                      title: const Text('Disconnect'),
                      subtitle: Text('${c.server.name} · ${c.server.endpoint}', style: const TextStyle(color: textFaint, fontSize: 12)),
                      contentPadding: EdgeInsets.zero,
                    ),
                  ),
                  const PopupMenuItem(
                    value: 'forget',
                    child: ListTile(leading: Icon(Icons.delete_outline_rounded), title: Text('Forget this computer'), contentPadding: EdgeInsets.zero),
                  ),
                ],
              ),
            ],
          ),
          body: Column(children: [
            if (c.status == LinkStatus.offline) const _Banner(text: 'Reconnecting to StageWeave…'),
            if (c.status == LinkStatus.connected && c.error != null) _Banner(text: c.error!),
            Expanded(child: _body(context)),
          ]),
          bottomNavigationBar: c.status == LinkStatus.connected && state.current != null
              ? NowPlayingBar(connection: c, current: state.current!, onOpen: () => _openNowPlaying(context))
              : null,
        );
      },
    );
  }

  Widget _body(BuildContext context) {
    final c = connection;
    switch (c.status) {
      case LinkStatus.needsCode:
        return PairView(connection: c);
      case LinkStatus.connecting:
        return const Center(child: CircularProgressIndicator());
      case LinkStatus.offline:
      case LinkStatus.connected:
        final state = c.state;
        if (c.status == LinkStatus.offline && state.projects.isEmpty) {
          return const Center(child: Padding(padding: EdgeInsets.all(32), child: Text('Can\'t reach StageWeave. Check that this phone is on the same Wi-Fi.', textAlign: TextAlign.center, style: TextStyle(color: textDim))));
        }
        if (state.project == null) return ProjectList(connection: c);
        return SetlistView(connection: c);
    }
  }

  void _pickProject(BuildContext context) {
    showModalBottomSheet<void>(
      context: context,
      backgroundColor: bgPanel,
      showDragHandle: true,
      builder: (context) => SafeArea(child: ProjectList(connection: connection, onPicked: () => Navigator.pop(context))),
    );
  }

  void _openNowPlaying(BuildContext context) {
    showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      backgroundColor: bgPanel,
      showDragHandle: true,
      builder: (context) => DraggableScrollableSheet(
        expand: false,
        initialChildSize: 0.92,
        minChildSize: 0.5,
        maxChildSize: 1,
        builder: (context, scroll) => ListenableBuilder(
          listenable: connection,
          builder: (context, _) {
            final cur = connection.state.current;
            if (cur == null) return const Center(child: Text('Nothing open', style: TextStyle(color: textDim)));
            return NowPlaying(connection: connection, current: cur, scroll: scroll);
          },
        ),
      ),
    );
  }
}

class _Banner extends StatelessWidget {
  final String text;

  const _Banner({required this.text});

  @override
  Widget build(BuildContext context) => Container(
        width: double.infinity,
        color: bgWidget,
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
        child: Text(text, style: const TextStyle(color: textDim, fontSize: 13)),
      );
}

/// Enter the 4-digit code shown on the computer.
class PairView extends StatefulWidget {
  final RemoteConnection connection;

  const PairView({super.key, required this.connection});

  @override
  State<PairView> createState() => _PairViewState();
}

class _PairViewState extends State<PairView> {
  final _controller = TextEditingController();

  @override
  void initState() {
    super.initState();
    // Screenshot hook: enter the code automatically.
    final pin = Dev.pin;
    if (pin != null) {
      Future.delayed(const Duration(milliseconds: 800), () {
        if (!mounted) return;
        _controller.text = pin;
        _submit();
      });
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _submit() {
    final pin = _controller.text.trim();
    if (pin.length == 4) widget.connection.pair(pin);
  }

  @override
  Widget build(BuildContext context) {
    return ListView(padding: const EdgeInsets.all(24), children: [
      const Icon(Icons.phonelink_lock_rounded, size: 56, color: accentBright),
      const SizedBox(height: 16),
      const Text('Enter the code shown on the computer', textAlign: TextAlign.center, style: TextStyle(fontSize: 18, fontWeight: FontWeight.w600)),
      const SizedBox(height: 8),
      const Text('It\'s on the StageWeave screen, and in Settings → Phone remote. You only need to do this once.', textAlign: TextAlign.center, style: TextStyle(color: textDim)),
      const SizedBox(height: 24),
      Center(
        child: SizedBox(
          width: 200,
          child: TextField(
            key: const Key('pin'),
            controller: _controller,
            autofocus: true,
            keyboardType: TextInputType.number,
            textAlign: TextAlign.center,
            maxLength: 4,
            inputFormatters: [FilteringTextInputFormatter.digitsOnly],
            style: const TextStyle(fontSize: 34, letterSpacing: 14, fontFeatures: [FontFeature.tabularFigures()]),
            decoration: const InputDecoration(counterText: '', hintText: '••••', border: OutlineInputBorder()),
            onChanged: (v) {
              if (v.length == 4) _submit();
            },
            onSubmitted: (_) => _submit(),
          ),
        ),
      ),
      if (widget.connection.error != null) ...[
        const SizedBox(height: 12),
        Text(widget.connection.error!, textAlign: TextAlign.center, style: const TextStyle(color: danger)),
      ],
      const SizedBox(height: 16),
      Center(child: FilledButton(onPressed: _submit, child: const Text('Pair'))),
    ]);
  }
}

/// The computer's setlists.
class ProjectList extends StatelessWidget {
  final RemoteConnection connection;
  final VoidCallback? onPicked;

  const ProjectList({super.key, required this.connection, this.onPicked});

  @override
  Widget build(BuildContext context) {
    final state = connection.state;
    if (state.projects.isEmpty) {
      return const Center(child: Text('No setlists yet. Make one in StageWeave.', style: TextStyle(color: textDim)));
    }
    return ListView(shrinkWrap: onPicked != null, children: [
      const Padding(
        padding: EdgeInsets.fromLTRB(20, 12, 20, 4),
        child: Text('Setlists', style: TextStyle(color: textDim, fontWeight: FontWeight.w600)),
      ),
      for (final p in state.projects)
        ListTile(
          leading: const Icon(Icons.queue_music_rounded),
          title: Text(p.name),
          subtitle: Text('${p.items} item${p.items == 1 ? '' : 's'}', style: const TextStyle(color: textFaint)),
          selected: state.project?.id == p.id,
          trailing: const Icon(Icons.chevron_right_rounded),
          onTap: () {
            connection.send(Cmd.openProject(p.id));
            onPicked?.call();
          },
        ),
    ]);
  }
}

/// The open setlist; tap an item to load it.
class SetlistView extends StatelessWidget {
  final RemoteConnection connection;

  const SetlistView({super.key, required this.connection});

  @override
  Widget build(BuildContext context) {
    final state = connection.state;
    if (state.items.isEmpty) {
      return const Center(child: Text('This setlist is empty.', style: TextStyle(color: textDim)));
    }
    final current = state.current?.id;
    return ListView.separated(
      itemCount: state.items.length,
      separatorBuilder: (_, _) => const Divider(indent: 72),
      itemBuilder: (context, i) {
        final item = state.items[i];
        final playing = item.id == current;
        return ListTile(
          key: ValueKey('item-${item.id}'),
          enabled: item.ready,
          selected: playing,
          selectedTileColor: accent.withValues(alpha: 0.12),
          leading: CircleAvatar(
            backgroundColor: bgWidget,
            child: playing
                ? Icon(state.current!.playing ? Icons.graphic_eq_rounded : Icons.pause_rounded, color: accentBright)
                : Text('${i + 1}', style: const TextStyle(color: textDim, fontWeight: FontWeight.w600)),
          ),
          title: Text(item.title, maxLines: 1, overflow: TextOverflow.ellipsis, style: TextStyle(fontWeight: playing ? FontWeight.w700 : FontWeight.w500)),
          subtitle: Row(children: [
            Icon(kindIcon(item.kind), size: 14, color: kindColor(item.kind)),
            const SizedBox(width: 6),
            Expanded(
              child: Text(
                [
                  if (item.artist.isNotEmpty) item.artist,
                  if (!item.ready) 'Not ready',
                ].join(' · '),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(color: textDim),
              ),
            ),
          ]),
          trailing: item.kind == ItemKind.deck || item.duration <= 0 ? null : Text(mmss(item.duration), style: const TextStyle(color: textFaint, fontFeatures: [FontFeature.tabularFigures()])),
          onTap: playing ? null : () => connection.send(Cmd.openItem(item.id)),
        );
      },
    );
  }
}

/// Compact transport at the bottom of the setlist.
class NowPlayingBar extends StatelessWidget {
  final RemoteConnection connection;
  final Current current;
  final VoidCallback onOpen;

  const NowPlayingBar({super.key, required this.connection, required this.current, required this.onOpen});

  @override
  Widget build(BuildContext context) {
    final deck = current.deck;
    final progress = current.duration > 0 ? (current.position / current.duration).clamp(0.0, 1.0) : 0.0;
    return Material(
      color: bgPanel,
      child: SafeArea(
        top: false,
        child: InkWell(
          onTap: onOpen,
          child: Column(mainAxisSize: MainAxisSize.min, children: [
            LinearProgressIndicator(value: deck == null ? progress : (deck.count > 0 ? (deck.index + 1) / deck.count : 0), minHeight: 2, backgroundColor: bgWidget),
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 8, 8, 8),
              child: Row(children: [
                Icon(kindIcon(current.kind), color: kindColor(current.kind)),
                const SizedBox(width: 12),
                Expanded(
                  child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
                    Text(current.title, maxLines: 1, overflow: TextOverflow.ellipsis, style: const TextStyle(fontWeight: FontWeight.w600)),
                    Text(
                      deck != null ? 'Slide ${deck.index + 1} of ${deck.count}' : (current.loading ? 'Loading…' : '${mmss(current.position)} / ${mmss(current.duration)}'),
                      style: const TextStyle(color: textDim, fontSize: 12, fontFeatures: [FontFeature.tabularFigures()]),
                    ),
                  ]),
                ),
                IconButton(tooltip: 'Previous', icon: const Icon(Icons.skip_previous_rounded), onPressed: () => connection.send(Cmd.prev())),
                if (deck == null)
                  IconButton(
                    tooltip: current.playing ? 'Pause' : 'Play',
                    iconSize: 36,
                    icon: Icon(current.playing ? Icons.pause_circle_filled_rounded : Icons.play_circle_filled_rounded, color: accentBright),
                    onPressed: () => connection.send(current.playing ? Cmd.pause() : Cmd.play()),
                  ),
                IconButton(tooltip: 'Next', icon: const Icon(Icons.skip_next_rounded), onPressed: () => connection.send(Cmd.next())),
              ]),
            ),
          ]),
        ),
      ),
    );
  }
}

/// Full controls for the open item: transport, cues, sections and the stem mixer.
class NowPlaying extends StatelessWidget {
  final RemoteConnection connection;
  final Current current;
  final ScrollController? scroll;

  const NowPlaying({super.key, required this.connection, required this.current, this.scroll});

  @override
  Widget build(BuildContext context) {
    final c = current;
    final send = connection.send;
    return ListView(controller: scroll, padding: const EdgeInsets.fromLTRB(20, 0, 20, 32), children: [
      Row(children: [
        Icon(kindIcon(c.kind), color: kindColor(c.kind)),
        const SizedBox(width: 10),
        Expanded(
          child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
            Text(c.title, style: const TextStyle(fontSize: 20, fontWeight: FontWeight.w700)),
            if (c.artist.isNotEmpty) Text(c.artist, style: const TextStyle(color: textDim)),
          ]),
        ),
      ]),
      const SizedBox(height: 16),
      if (c.deck != null) ..._deck(c.deck!, send) else ..._transport(c, send),
      if (c.cues.isNotEmpty) ...[
        const _Heading('Cues', hint: 'Tap to loop'),
        Wrap(spacing: 8, runSpacing: 8, children: [
          for (var i = 0; i < c.cues.length; i++)
            ChoiceChip(
              label: Text(c.cues[i].name.isEmpty ? 'Cue ${i + 1}' : c.cues[i].name),
              avatar: const Icon(Icons.repeat_rounded, size: 16),
              selected: c.activeCue == i,
              onSelected: (_) => send(c.activeCue == i ? Cmd.exitCue() : Cmd.cue(i)),
            ),
          if (c.activeCue != null)
            ActionChip(label: const Text('Exit Cue'), avatar: const Icon(Icons.close_rounded, size: 16), onPressed: () => send(Cmd.exitCue())),
        ]),
      ],
      if (c.sections.isNotEmpty) ...[
        const _Heading('Sections', hint: 'Tap to jump'),
        Wrap(spacing: 8, runSpacing: 8, children: [
          for (var i = 0; i < c.sections.length; i++)
            ChoiceChip(
              label: Text(c.sections[i].name.isEmpty ? 'Section ${i + 1}' : c.sections[i].name),
              selected: c.currentSection == i,
              onSelected: (_) => send(Cmd.section(i)),
            ),
        ]),
      ],
      if (c.deck == null) ...[
        const _Heading('Mix'),
        if (c.tracks.isEmpty)
          const Padding(
            padding: EdgeInsets.only(bottom: 8),
            child: Text('Not split: the original mix is playing. Split it in StageWeave to mix the stems here.', style: TextStyle(color: textFaint)),
          ),
        for (var i = 0; i < c.tracks.length; i++) TrackRow(index: i, track: c.tracks[i], connection: connection),
        GainRow(
          label: 'Master',
          color: accentBright,
          gainDb: c.masterDb,
          onChanged: (db) => send(Cmd.master(db)),
        ),
      ],
    ]);
  }

  List<Widget> _deck(DeckInfo deck, void Function(Map<String, dynamic>) send) => [
        Center(child: Text('Slide ${deck.index + 1} of ${deck.count}', style: const TextStyle(fontSize: 28, fontWeight: FontWeight.w700))),
        const SizedBox(height: 16),
        Row(children: [
          Expanded(
            child: FilledButton.tonalIcon(
              style: FilledButton.styleFrom(minimumSize: const Size.fromHeight(72)),
              onPressed: () => send(Cmd.prev()),
              icon: const Icon(Icons.arrow_back_rounded),
              label: const Text('Previous'),
            ),
          ),
          const SizedBox(width: 12),
          Expanded(
            child: FilledButton.icon(
              style: FilledButton.styleFrom(minimumSize: const Size.fromHeight(72)),
              onPressed: () => send(Cmd.next()),
              icon: const Icon(Icons.arrow_forward_rounded),
              label: const Text('Next'),
            ),
          ),
        ]),
        const SizedBox(height: 8),
        Center(
          child: TextButton.icon(
            onPressed: () => send(Cmd.toggle()),
            icon: const Icon(Icons.play_arrow_rounded),
            label: const Text('Play / pause the slide\'s video'),
          ),
        ),
      ];

  List<Widget> _transport(Current c, void Function(Map<String, dynamic>) send) => [
        SeekBar(position: c.position, duration: c.duration, onSeek: (t) => send(Cmd.seek(t))),
        const SizedBox(height: 8),
        Row(mainAxisAlignment: MainAxisAlignment.spaceEvenly, children: [
          IconButton(
            tooltip: c.looping ? 'Loop on' : 'Loop off',
            icon: Icon(Icons.repeat_rounded, color: c.looping ? accentBright : textFaint),
            onPressed: () => send(Cmd.loop(!c.looping)),
          ),
          IconButton(tooltip: 'Previous', iconSize: 36, icon: const Icon(Icons.skip_previous_rounded), onPressed: () => send(Cmd.prev())),
          IconButton(
            tooltip: c.playing ? 'Pause' : 'Play',
            iconSize: 72,
            icon: c.loading
                ? const SizedBox(width: 48, height: 48, child: CircularProgressIndicator())
                : Icon(c.playing ? Icons.pause_circle_filled_rounded : Icons.play_circle_filled_rounded, color: accentBright),
            onPressed: c.loading ? null : () => send(c.playing ? Cmd.pause() : Cmd.play()),
          ),
          IconButton(tooltip: 'Next', iconSize: 36, icon: const Icon(Icons.skip_next_rounded), onPressed: () => send(Cmd.next())),
          IconButton(tooltip: 'Back to start', icon: const Icon(Icons.stop_rounded), onPressed: () => send(Cmd.stop())),
        ]),
      ];
}

class _Heading extends StatelessWidget {
  final String text;
  final String? hint;

  const _Heading(this.text, {this.hint});

  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.only(top: 24, bottom: 10),
        child: Row(children: [
          Text(text.toUpperCase(), style: const TextStyle(color: textDim, fontSize: 12, fontWeight: FontWeight.w700, letterSpacing: 1.1)),
          if (hint != null) ...[const SizedBox(width: 8), Text(hint!, style: const TextStyle(color: textFaint, fontSize: 12))],
        ]),
      );
}

/// Position slider; holds its own value while dragging.
class SeekBar extends StatefulWidget {
  final double position;
  final double duration;
  final ValueChanged<double> onSeek;

  const SeekBar({super.key, required this.position, required this.duration, required this.onSeek});

  @override
  State<SeekBar> createState() => _SeekBarState();
}

class _SeekBarState extends State<SeekBar> {
  double? _drag;

  @override
  Widget build(BuildContext context) {
    final max = math.max(widget.duration, 0.1);
    final value = (_drag ?? widget.position).clamp(0.0, max);
    return Column(children: [
      Slider(
        value: value,
        max: max,
        onChanged: widget.duration > 0 ? (v) => setState(() => _drag = v) : null,
        onChangeEnd: (v) {
          widget.onSeek(v);
          setState(() => _drag = null);
        },
      ),
      Padding(
        padding: const EdgeInsets.symmetric(horizontal: 24),
        child: Row(children: [
          Text(mmss(value), style: const TextStyle(color: textDim, fontFeatures: [FontFeature.tabularFigures()])),
          const Spacer(),
          Text(mmss(widget.duration), style: const TextStyle(color: textDim, fontFeatures: [FontFeature.tabularFigures()])),
        ]),
      ),
    ]);
  }
}

/// One stem: mute, solo and volume.
class TrackRow extends StatelessWidget {
  final int index;
  final TrackInfo track;
  final RemoteConnection connection;

  const TrackRow({super.key, required this.index, required this.track, required this.connection});

  @override
  Widget build(BuildContext context) {
    final color = stemColor(track.kind);
    return GainRow(
      label: track.name,
      color: color,
      gainDb: track.gainDb,
      dimmed: track.muted,
      onChanged: (db) => connection.send(Cmd.track(index, gainDb: db)),
      leading: [
        _Toggle(label: 'M', tooltip: 'Mute ${track.name}', on: track.muted, color: muteOn, onTap: () => connection.send(Cmd.track(index, muted: !track.muted))),
        const SizedBox(width: 6),
        _Toggle(label: 'S', tooltip: 'Solo ${track.name}', on: track.solo, color: soloOn, onTap: () => connection.send(Cmd.track(index, solo: !track.solo))),
      ],
    );
  }
}

class _Toggle extends StatelessWidget {
  final String label;
  final String tooltip;
  final bool on;
  final Color color;
  final VoidCallback onTap;

  const _Toggle({required this.label, required this.tooltip, required this.on, required this.color, required this.onTap});

  @override
  Widget build(BuildContext context) => Tooltip(
        message: tooltip,
        child: Semantics(
          button: true,
          toggled: on,
          label: tooltip,
          child: InkWell(
            borderRadius: BorderRadius.circular(6),
            onTap: onTap,
            child: Container(
              width: 36,
              height: 36,
              alignment: Alignment.center,
              decoration: BoxDecoration(
                color: on ? color : bgWidget,
                borderRadius: BorderRadius.circular(6),
                border: Border.all(color: on ? color : border),
              ),
              child: Text(label, style: TextStyle(fontWeight: FontWeight.w800, color: on ? Colors.black : textDim)),
            ),
          ),
        ),
      );
}

const _minDb = -60.0;
const _maxDb = 6.0;

/// Fader position (0–1) ↔ dB, with the same curve as the desktop faders.
double dbToFrac(double db) => math.pow(((db - _minDb) / (_maxDb - _minDb)).clamp(0.0, 1.0), 1.6).toDouble();
double fracToDb(double f) => _minDb + math.pow(f.clamp(0.0, 1.0), 1 / 1.6) * (_maxDb - _minDb);

/// A labelled volume fader that sends changes while dragging (throttled).
class GainRow extends StatefulWidget {
  final String label;
  final Color color;
  final double gainDb;
  final bool dimmed;
  final ValueChanged<double> onChanged;
  final List<Widget> leading;

  const GainRow({super.key, required this.label, required this.color, required this.gainDb, required this.onChanged, this.dimmed = false, this.leading = const []});

  @override
  State<GainRow> createState() => _GainRowState();
}

class _GainRowState extends State<GainRow> {
  double? _drag;
  DateTime _lastSent = DateTime(0);
  Timer? _pending;

  void _send(double db, {bool now = false}) {
    _pending?.cancel();
    final since = DateTime.now().difference(_lastSent);
    if (now || since > const Duration(milliseconds: 80)) {
      _lastSent = DateTime.now();
      widget.onChanged(double.parse(db.toStringAsFixed(1)));
    } else {
      _pending = Timer(const Duration(milliseconds: 80) - since, () => _send(db, now: true));
    }
  }

  @override
  void dispose() {
    _pending?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final db = _drag ?? widget.gainDb;
    final label = db <= _minDb ? '-∞' : '${db >= 0 ? '+' : ''}${db.toStringAsFixed(1)}';
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 4),
      child: Row(children: [
        Container(width: 4, height: 32, decoration: BoxDecoration(color: widget.color, borderRadius: BorderRadius.circular(2))),
        const SizedBox(width: 10),
        SizedBox(
          width: 72,
          child: Text(widget.label, maxLines: 1, overflow: TextOverflow.ellipsis, style: TextStyle(fontWeight: FontWeight.w600, color: widget.dimmed ? textFaint : textMain)),
        ),
        ...widget.leading,
        Expanded(
          child: SliderTheme(
            data: SliderTheme.of(context).copyWith(activeTrackColor: widget.dimmed ? textFaint : widget.color),
            child: Slider(
              value: dbToFrac(db),
              onChanged: (f) {
                final v = fracToDb(f);
                setState(() => _drag = v);
                _send(v);
              },
              onChangeEnd: (f) {
                _send(fracToDb(f), now: true);
                setState(() => _drag = null);
              },
            ),
          ),
        ),
        SizedBox(
          width: 44,
          child: Text(label, textAlign: TextAlign.right, style: const TextStyle(color: textDim, fontSize: 12, fontFeatures: [FontFeature.tabularFigures()])),
        ),
      ]),
    );
  }
}
