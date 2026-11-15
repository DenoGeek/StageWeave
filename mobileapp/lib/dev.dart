/// Hooks for automated screenshots, set at build time
/// (`flutter build ios --simulator --dart-define=SW_AUTO_OPEN=1`). They do
/// nothing in normal builds.
class Dev {
  static String? _get(String v) => v.isEmpty ? null : v;

  /// Open the first computer found ("1"), or the one on this port ("47801").
  static String? get autoOpen => _get(const String.fromEnvironment('SW_AUTO_OPEN'));

  /// Enter this pairing code when asked.
  static String? get pin => _get(const String.fromEnvironment('SW_PIN'));

  /// "now_playing": open the full controls once something is open.
  static String? get screen => _get(const String.fromEnvironment('SW_SCREEN'));
}
