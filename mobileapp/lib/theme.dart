import 'package:flutter/material.dart';

import 'protocol.dart';

// Colours from StageWeave's desktop theme (software/src/theme.rs).
const bgApp = Color(0xFF0E0E0F);
const bgPanel = Color(0xFF1D1D1F);
const bgWidget = Color(0xFF2E2E32);
const border = Color(0xFF303034);
const textMain = Color(0xFFD6D6DA);
const textDim = Color(0xFF96969E);
const textFaint = Color(0xFF62626A);
const accent = Color(0xFF2680EB);
const accentBright = Color(0xFF54A0FF);
const muteOn = Color(0xFF2CA88C);
const soloOn = Color(0xFFE2BA34);
const danger = Color(0xFFE84848);
const youtube = Color(0xFFEB5046);
const slides = Color(0xFFEBAA3C);

/// Stem colours, as on the desktop.
Color stemColor(String kind) => switch (kind) {
      'vocals' => const Color(0xFFE85DA0),
      'drums' => const Color(0xFFF09238),
      'bass' => const Color(0xFF8C6EF5),
      'guitar' => const Color(0xFF5CC478),
      'piano' => const Color(0xFF40BED2),
      'other' => const Color(0xFFDEC446),
      _ => const Color(0xFF5AA0F0),
    };

IconData kindIcon(ItemKind k) => switch (k) {
      ItemKind.song => Icons.music_note_rounded,
      ItemKind.youtube => Icons.smart_display_rounded,
      ItemKind.deck => Icons.slideshow_rounded,
    };

Color kindColor(ItemKind k) => switch (k) {
      ItemKind.song => accentBright,
      ItemKind.youtube => youtube,
      ItemKind.deck => slides,
    };

ThemeData stageTheme() {
  final scheme = ColorScheme.fromSeed(seedColor: accent, brightness: Brightness.dark).copyWith(
    primary: accentBright,
    surface: bgPanel,
    onSurface: textMain,
    surfaceContainerHighest: bgWidget,
    outline: border,
    error: danger,
  );
  return ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
    scaffoldBackgroundColor: bgApp,
    appBarTheme: const AppBarTheme(backgroundColor: bgApp, foregroundColor: textMain, elevation: 0, scrolledUnderElevation: 0),
    listTileTheme: const ListTileThemeData(iconColor: textDim, textColor: textMain),
    dividerTheme: const DividerThemeData(color: border, space: 1),
    sliderTheme: const SliderThemeData(trackHeight: 3, activeTrackColor: accentBright, thumbColor: Colors.white, inactiveTrackColor: bgWidget),
    chipTheme: const ChipThemeData(backgroundColor: bgWidget, side: BorderSide(color: border), labelStyle: TextStyle(color: textMain)),
  );
}
