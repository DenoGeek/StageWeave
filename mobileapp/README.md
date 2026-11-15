# StageWeave Remote

A phone remote for the StageWeave desktop app ([`../software`](../software)). It finds StageWeave on the same Wi-Fi network and controls it: open a setlist, load an item, play, pause, go to the next or previous item or slide, loop cues, jump to sections, and mix the stems. It shows titles and controls only; the media plays on the computer.

## Using it

1. In StageWeave, open **Settings → Phone remote** and tick **Let phones on this Wi-Fi control StageWeave**.
2. Open StageWeave Remote on a phone on the same Wi-Fi. The computer shows up as **StageWeave on macOS** (or Windows / Linux) with its address, for example `192.168.1.20:47800`.
3. Tap it and enter the 4-digit code shown on the computer. The phone is remembered after that; remove it any time in Settings.

If the computer doesn't show up (some networks block Bonjour/mDNS), tap **Enter address** and type the address shown in StageWeave's Settings.

## How it works

- StageWeave advertises `_stageweave._tcp` over Bonjour/mDNS and listens for WebSocket connections on port 47800 (or the next free port).
- Messages are JSON; see `lib/protocol.dart` and `software/src/remote/protocol.rs`. `test/fixtures/state.json` is parsed by both the Dart and the Rust tests so the two stay in step.
- Only connections from private, link-local or loopback addresses are accepted.

## Development

```sh
flutter pub get
flutter analyze
flutter test
flutter run                     # on a connected phone or a simulator
flutter build apk --release     # Android
```

The iOS Simulator shares the Mac's network, so it can find a StageWeave running on the same Mac. A physical iPhone needs your Apple signing team set in Xcode (`ios/Runner.xcworkspace`).

### Android release signing

The release workflow signs the APK with a release key when these repository secrets are set; otherwise it uses a debug key (installable, but later updates must be signed with the same key):

| Secret | Value |
| --- | --- |
| `ANDROID_KEYSTORE_BASE64` | `base64 -i release.jks` |
| `ANDROID_KEYSTORE_PASSWORD` | Keystore password |
| `ANDROID_KEY_ALIAS` | Key alias |
| `ANDROID_KEY_PASSWORD` | Key password |
