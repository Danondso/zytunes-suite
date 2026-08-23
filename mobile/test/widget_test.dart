import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:zytunes_mobile/app.dart';
import 'package:zytunes_mobile/playback.dart';
import 'package:zytunes_mobile/session.dart';
import 'package:zytunes_mobile/storage.dart';

void main() {
  http.Response jsonOk(Object body) => http.Response(
    jsonEncode(body),
    200,
    headers: {'content-type': 'application/json'},
  );

  Session sessionFor(http.Response Function(http.Request) handler) {
    return Session(
      httpClient: MockClient((req) async => handler(req)),
      playback: FakePlayback(),
      store: MemoryCredentialsStore(),
    );
  }

  Widget app(Session session) => ZytunesApp(session: session);

  http.Response? trackDetail(http.Request req) {
    final parts = req.url.path.split('/');
    if (req.method != 'GET' || parts.length != 3 || parts[1] != 'tracks') {
      return null;
    }
    return jsonOk({
      'id': parts[2],
      'name': '',
      'artist': '',
      'album': '',
      'stream_url': '${req.url.path}/stream',
      'file_url': '${req.url.path}/file',
      'art_url': '${req.url.path}/art',
    });
  }

  testWidgets('connect success shows the artist list', (tester) async {
    final session = sessionFor((req) {
      if (req.url.path == '/health') return jsonOk({'ok': true});
      if (req.url.path == '/artists') return jsonOk(['Radiohead']);
      fail('unexpected ${req.url}');
    });

    await tester.pumpWidget(app(session));
    await tester.enterText(find.byKey(const Key('hostField')), '10.0.0.8');
    await tester.enterText(find.byKey(const Key('portField')), '9847');
    await tester.enterText(find.byKey(const Key('tokenField')), 'secret');
    await tester.tap(find.byKey(const Key('connectButton')));
    await tester.pumpAndSettle();

    expect(find.text('Radiohead'), findsOneWidget);
    expect(find.byKey(const Key('hostField')), findsNothing);
  });

  testWidgets('scrolling artists dismisses the search keyboard', (
    tester,
  ) async {
    final session = sessionFor((req) {
      if (req.url.path == '/health') return jsonOk({'ok': true});
      if (req.url.path == '/artists') {
        return jsonOk([for (var i = 0; i < 40; i++) 'Artist $i']);
      }
      fail('unexpected ${req.url}');
    });

    await tester.pumpWidget(app(session));
    await tester.enterText(find.byKey(const Key('hostField')), '10.0.0.8');
    await tester.tap(find.byKey(const Key('connectButton')));
    await tester.pumpAndSettle();

    await tester.showKeyboard(find.byKey(const Key('searchField')));
    expect(tester.testTextInput.isVisible, isTrue);

    await tester.drag(find.text('Artist 0'), const Offset(0, -240));
    await tester.pumpAndSettle();

    expect(tester.testTextInput.isVisible, isFalse);
  });

  testWidgets('failed restore prefills the connect form', (tester) async {
    final store = MemoryCredentialsStore()
      ..value = const SavedServer(
        host: '10.0.0.8',
        port: 9847,
        token: 'secret',
      );
    final session = Session(
      httpClient: MockClient(
        (_) async => throw http.ClientException('Connection refused'),
      ),
      playback: FakePlayback(),
      store: store,
    );

    await tester.pumpWidget(app(session));
    await session.restore();
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('hostField')), findsOneWidget);
    expect(find.text('10.0.0.8'), findsOneWidget);
    expect(find.text('9847'), findsOneWidget);
    expect(find.textContaining('reach'), findsOneWidget);
    expect(find.widgetWithText(FilledButton, 'Connect'), findsOneWidget);
  });

  testWidgets('cancel unsticks a hung connect so login can be retried', (
    tester,
  ) async {
    final delayed = Completer<http.Response>();
    final session = Session(
      httpClient: MockClient((req) async {
        if (req.url.path == '/health') return delayed.future;
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        fail('unexpected ${req.url}');
      }),
      playback: FakePlayback(),
      store: MemoryCredentialsStore(),
    );

    await tester.pumpWidget(app(session));
    await tester.enterText(find.byKey(const Key('hostField')), '10.0.0.8');
    await tester.tap(find.byKey(const Key('connectButton')));
    await tester.pump();

    expect(find.widgetWithText(FilledButton, 'Cancel'), findsOneWidget);
    await tester.tap(find.byKey(const Key('connectButton')));
    await tester.pump();

    expect(find.widgetWithText(FilledButton, 'Connect'), findsOneWidget);
    expect(find.byKey(const Key('hostField')), findsOneWidget);

    delayed.complete(jsonOk({'ok': true}));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('hostField')), findsOneWidget);
    expect(find.text('Radiohead'), findsNothing);
  });

  testWidgets('library gear edits the server and stays connected', (
    tester,
  ) async {
    final session = sessionFor((req) {
      if (req.url.path == '/health') return jsonOk({'ok': true});
      if (req.url.path == '/artists') {
        return jsonOk([req.url.host == '10.0.0.9' ? 'NIN' : 'Radiohead']);
      }
      fail('unexpected ${req.url}');
    });

    await tester.pumpWidget(app(session));
    await tester.enterText(find.byKey(const Key('hostField')), '10.0.0.8');
    await tester.tap(find.byKey(const Key('connectButton')));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('settingsButton')));
    await tester.pumpAndSettle();
    expect(find.text('Server'), findsOneWidget);
    expect(find.text('10.0.0.8'), findsOneWidget);

    await tester.enterText(find.byKey(const Key('hostField')), '10.0.0.9');
    await tester.tap(find.byKey(const Key('settingsSaveButton')));
    await tester.pumpAndSettle();

    expect(find.text('Server'), findsNothing);
    expect(find.text('NIN'), findsOneWidget);
    expect(find.byKey(const Key('settingsButton')), findsOneWidget);
  });

  testWidgets('wrong token stays on connect and shows an error', (
    tester,
  ) async {
    final session = sessionFor((_) => http.Response('', 401));

    await tester.pumpWidget(app(session));
    await tester.enterText(find.byKey(const Key('hostField')), '10.0.0.8');
    await tester.tap(find.byKey(const Key('connectButton')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('hostField')), findsOneWidget);
    expect(find.textContaining('token'), findsOneWidget);
  });

  testWidgets('browse artist → album → track starts playback', (tester) async {
    final playback = FakePlayback();
    final session = Session(
      httpClient: MockClient((req) async {
        switch (req.url.path) {
          case '/health':
            return jsonOk({'ok': true});
          case '/artists':
            return jsonOk(['Radiohead']);
          case '/albums':
            return jsonOk([
              {'artist': 'Radiohead', 'album': 'OK Computer'},
            ]);
          case '/tracks':
            return jsonOk([
              {
                'id': 42,
                'name': 'Karma Police',
                'artist': 'Radiohead',
                'album': 'OK Computer',
                'track_number': 1,
              },
            ]);
          default:
            return trackDetail(req) ??
                (throw TestFailure('unexpected ${req.url}'));
        }
      }),
      playback: playback,
      store: MemoryCredentialsStore(),
    );

    await tester.pumpWidget(app(session));
    await tester.enterText(find.byKey(const Key('hostField')), '10.0.0.8');
    await tester.tap(find.byKey(const Key('connectButton')));
    await tester.pumpAndSettle();

    await tester.tap(find.text('Radiohead'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('artistScreen')), findsOneWidget);
    expect(find.text('OK Computer'), findsOneWidget);
    expect(find.byKey(const Key('playArtistButton')), findsOneWidget);
    expect(find.byKey(const Key('artistAlbumCount')), findsOneWidget);

    await tester.tap(find.text('OK Computer'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('albumCover')), findsOneWidget);
    expect(find.text('Karma Police'), findsOneWidget);

    await tester.tap(find.text('Karma Police'));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('nowPlaying')), findsOneWidget);
    expect(find.text('Karma Police'), findsWidgets);
    expect(
      playback.lastUri,
      Uri.parse('http://10.0.0.8:9847/tracks/42/stream'),
    );
  });

  testWidgets('artist page Play queues the artist in album order', (
    tester,
  ) async {
    final playback = FakePlayback();
    final session = Session(
      httpClient: MockClient((req) async {
        switch (req.url.path) {
          case '/health':
            return jsonOk({'ok': true});
          case '/artists':
            return jsonOk(['Radiohead']);
          case '/albums':
            return jsonOk([
              {'artist': 'Radiohead', 'album': 'OK Computer', 'track_count': 2},
              {'artist': 'Radiohead', 'album': 'Pablo Honey', 'track_count': 1},
            ]);
          case '/tracks':
            return jsonOk([
              {
                'id': 3,
                'name': 'You',
                'artist': 'Radiohead',
                'album': 'Pablo Honey',
                'track_number': 1,
              },
              {
                'id': 2,
                'name': 'Let Down',
                'artist': 'Radiohead',
                'album': 'OK Computer',
                'track_number': 5,
              },
              {
                'id': 1,
                'name': 'Airbag',
                'artist': 'Radiohead',
                'album': 'OK Computer',
                'track_number': 1,
              },
            ]);
          default:
            return trackDetail(req) ??
                (throw TestFailure('unexpected ${req.url}'));
        }
      }),
      playback: playback,
      store: MemoryCredentialsStore(),
    );

    await tester.pumpWidget(app(session));
    await tester.enterText(find.byKey(const Key('hostField')), '10.0.0.8');
    await tester.tap(find.byKey(const Key('connectButton')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Radiohead'));
    await tester.pumpAndSettle();

    expect(find.text('2 albums'), findsOneWidget);
    await tester.tap(find.byKey(const Key('playArtistButton')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('nowPlaying')), findsOneWidget);
    expect(session.nowPlaying!.name, 'Airbag');
    expect(session.queue.map((t) => t.name), ['Airbag', 'Let Down', 'You']);
  });

  testWidgets('search lists hits and plays the selected track', (tester) async {
    final playback = FakePlayback();
    final session = Session(
      httpClient: MockClient((req) async {
        switch (req.url.path) {
          case '/health':
            return jsonOk({'ok': true});
          case '/artists':
            return jsonOk(['Radiohead']);
          case '/search':
            return jsonOk({
              'artists': ['Radiohead'],
              'albums': [
                {
                  'artist': 'Radiohead',
                  'album': 'OK Computer',
                  'track_count': 12,
                },
              ],
              'tracks': [
                {
                  'id': 42,
                  'name': 'Karma Police',
                  'artist': 'Radiohead',
                  'album': 'OK Computer',
                },
              ],
            });
          default:
            return trackDetail(req) ??
                (throw TestFailure('unexpected ${req.url}'));
        }
      }),
      playback: playback,
      store: MemoryCredentialsStore(),
    );

    await tester.pumpWidget(app(session));
    await tester.enterText(find.byKey(const Key('hostField')), '10.0.0.8');
    await tester.tap(find.byKey(const Key('connectButton')));
    await tester.pumpAndSettle();

    await tester.enterText(find.byKey(const Key('searchField')), 'karma');
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('searchArtist-Radiohead')), findsOneWidget);
    expect(
      find.byKey(const Key('searchAlbum-Radiohead-OK Computer')),
      findsOneWidget,
    );
    expect(find.text('Karma Police'), findsOneWidget);
    await tester.tap(find.text('Karma Police'));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('nowPlaying')), findsOneWidget);
    expect(
      playback.lastUri,
      Uri.parse('http://10.0.0.8:9847/tracks/42/stream'),
    );
  });

  Session searchSession(FakePlayback playback) {
    return Session(
      httpClient: MockClient((req) async {
        switch (req.url.path) {
          case '/health':
            return jsonOk({'ok': true});
          case '/artists':
            return jsonOk(['Radiohead']);
          case '/search':
            return jsonOk({
              'artists': ['Radiohead'],
              'albums': [
                {
                  'artist': 'Radiohead',
                  'album': 'OK Computer',
                  'track_count': 12,
                },
              ],
              'tracks': [
                {
                  'id': 42,
                  'name': 'Karma Police',
                  'artist': 'Radiohead',
                  'album': 'OK Computer',
                },
              ],
            });
          case '/albums':
            return jsonOk([
              {
                'artist': 'Radiohead',
                'album': 'OK Computer',
                'track_count': 12,
              },
            ]);
          case '/tracks':
            return jsonOk([
              {
                'id': 42,
                'name': 'Karma Police',
                'artist': 'Radiohead',
                'album': 'OK Computer',
                'track_number': 1,
              },
            ]);
          default:
            return trackDetail(req) ??
                (throw TestFailure('unexpected ${req.url}'));
        }
      }),
      playback: playback,
      store: MemoryCredentialsStore(),
    );
  }

  Future<void> connectAndSearch(WidgetTester tester, Session session) async {
    await tester.pumpWidget(app(session));
    await tester.enterText(find.byKey(const Key('hostField')), '10.0.0.8');
    await tester.tap(find.byKey(const Key('connectButton')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('searchField')), 'karma');
    await tester.pumpAndSettle();
  }

  testWidgets('search artist and album rows open those pages', (tester) async {
    final session = searchSession(FakePlayback());
    await connectAndSearch(tester, session);

    await tester.tap(find.byKey(const Key('searchArtist-Radiohead')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('artistScreen')), findsOneWidget);
    expect(find.byKey(const Key('playArtistButton')), findsOneWidget);

    await tester.pageBack();
    await tester.pumpAndSettle();

    await tester.tap(
      find.byKey(const Key('searchAlbum-Radiohead-OK Computer')),
    );
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('albumCover')), findsOneWidget);
    expect(find.text('Karma Police'), findsOneWidget);
  });

  testWidgets('search track menu navigates to album and artist', (
    tester,
  ) async {
    final session = searchSession(FakePlayback());
    await connectAndSearch(tester, session);

    await tester.tap(find.byKey(const Key('trackMenu-42')));
    await tester.pumpAndSettle();
    expect(find.text('Go to album'), findsOneWidget);
    expect(find.text('Go to artist'), findsOneWidget);

    await tester.tap(find.text('Go to album'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('albumCover')), findsOneWidget);
    expect(find.text('Karma Police'), findsOneWidget);

    await tester.pageBack();
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('trackMenu-42')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Go to artist'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('artistScreen')), findsOneWidget);
  });
}
