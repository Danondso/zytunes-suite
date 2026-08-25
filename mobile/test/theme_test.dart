import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:zytunes_mobile/app.dart';
import 'package:zytunes_mobile/playback.dart';
import 'package:zytunes_mobile/session.dart';
import 'package:zytunes_mobile/storage.dart';
import 'package:zytunes_mobile/theme.dart';

void main() {
  const tuiNames = [
    'iTunes 2004',
    'Gruvbox Dark',
    'Gruvbox Light',
    'Everforest Dark',
    'Everforest Light',
    'Tokyo Night',
    'IBM Mainframe',
    'Amber CRT',
    'Windows 95',
    'System 7',
    'BIOS',
    'Red Sands',
    'Newport Lights',
    'NeXTSTEP',
    'WinAmp Classic',
    'Zune Original',
  ];

  test('catalog includes Bedfellow and every TUI theme', () {
    final names = appThemes.map((theme) => theme.name).toList();
    expect(names, containsAll(['Bedfellow Light', 'Bedfellow Dark', ...tuiNames]));
    expect(appThemes.map((theme) => theme.id).toSet().length, appThemes.length);
  });

  test('unknown theme id falls back to Bedfellow Light', () {
    expect(resolveThemeId(null), defaultThemeId);
    expect(resolveThemeId('not-a-theme'), defaultThemeId);
    expect(themeDataFor('not-a-theme').colorScheme.primary, BedfellowColors.teal);
  });

  testWidgets('app defaults to Bedfellow Light', (tester) async {
    final session = Session(
      httpClient: MockClient((_) async => http.Response('', 500)),
      playback: FakePlayback(),
      store: MemoryCredentialsStore(),
    );
    await tester.pumpWidget(ZytunesApp(session: session));

    final theme = Theme.of(tester.element(find.byType(Scaffold)));
    expect(theme.colorScheme.primary, BedfellowColors.teal);
    expect(theme.colorScheme.surface, BedfellowColors.sand50);
    expect(theme.brightness, Brightness.light);
  });

  testWidgets('theme picker applies a TUI theme', (tester) async {
    final session = Session(
      httpClient: MockClient((_) async => http.Response('', 500)),
      playback: FakePlayback(),
      store: MemoryCredentialsStore(),
    );
    await tester.pumpWidget(ZytunesApp(session: session));

    await tester.tap(find.byKey(const Key('themePicker')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Gruvbox Dark').last);
    await tester.pumpAndSettle();

    expect(session.themeId, 'gruvbox-dark');
    final theme = Theme.of(tester.element(find.byType(Scaffold)));
    expect(theme.colorScheme.primary, const Color.fromARGB(255, 214, 93, 14));
    expect(theme.colorScheme.surface, const Color.fromARGB(255, 40, 40, 40));
  });
}
