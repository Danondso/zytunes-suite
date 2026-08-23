import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:zytunes_mobile/app.dart';
import 'package:zytunes_mobile/playback.dart';
import 'package:zytunes_mobile/session.dart';
import 'package:zytunes_mobile/storage.dart';
import 'package:zytunes_mobile/theme.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

void main() {
  testWidgets('app uses Bedfellow teal, not Zune orange', (tester) async {
    final session = Session(
      httpClient: MockClient((_) async => http.Response('', 500)),
      playback: FakePlayback(),
      store: MemoryCredentialsStore(),
    );
    await tester.pumpWidget(ZytunesApp(session: session));

    final theme = Theme.of(tester.element(find.byType(Scaffold)));
    expect(theme.colorScheme.primary, BedfellowColors.tealLight);
    expect(theme.colorScheme.surface, BedfellowColors.darkBg);
    expect(theme.colorScheme.secondary, BedfellowColors.sageLight);
    expect(theme.brightness, Brightness.dark);
  });
}
