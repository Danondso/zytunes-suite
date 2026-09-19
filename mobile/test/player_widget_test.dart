import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:zytunes_mobile/app.dart';
import 'package:zytunes_mobile/crossfade.dart';
import 'package:zytunes_mobile/playback.dart';
import 'package:zytunes_mobile/session.dart';
import 'package:zytunes_mobile/stem_playback.dart';
import 'package:zytunes_mobile/storage.dart';

void main() {
  http.Response jsonOk(Object body) => http.Response(
    jsonEncode(body),
    200,
    headers: {'content-type': 'application/json'},
  );

  Future<Session> connectedAlbum({
    required Playback playback,
    StemMix Function()? createStemMix,
  }) async {
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
                'id': 1,
                'name': 'Airbag',
                'artist': 'Radiohead',
                'album': 'OK Computer',
                'track_number': 1,
                'duration_ms': 287000,
              },
              {
                'id': 43,
                'name': 'Let Down',
                'artist': 'Radiohead',
                'album': 'OK Computer',
                'track_number': 5,
                'duration_ms': 299000,
              },
              {
                'id': 42,
                'name': 'Karma Police',
                'artist': 'Radiohead',
                'album': 'OK Computer',
                'track_number': 6,
                'duration_ms': 262000,
              },
            ]);
          default:
            if (req.url.path.endsWith('/stems')) {
              const kinds = [
                'vocals',
                'drums',
                'bass',
                'guitar',
                'piano',
                'other',
              ];
              final id = req.url.path.split('/')[2];
              return jsonOk({
                'status': 'ready',
                'recipe': 'demucs',
                'layout': kinds,
                'stems': [
                  for (final k in kinds)
                    {
                      'kind': k,
                      'label': k,
                      'short_label': k.substring(0, 3),
                      'url': '/tracks/$id/stems/$k',
                    },
                ],
                'engine_available': true,
              });
            }
            if (req.url.path.endsWith('/play') && req.method == 'POST') {
              return jsonOk({
                'play_count': 6,
                'last_played_at_ms': 1700000000000,
              });
            }
            final parts = req.url.path.split('/');
            if (req.method == 'GET' &&
                parts.length == 3 &&
                parts[1] == 'tracks') {
              return jsonOk({
                'id': parts[2],
                'name': 'Airbag',
                'artist': 'Radiohead',
                'album': 'OK Computer',
                'stream_url': '${req.url.path}/stream',
                'file_url': '${req.url.path}/file',
                'art_url': '${req.url.path}/art',
                'play_count': 5,
              });
            }
            return http.Response('', 404);
        }
      }),
      playback: playback,
      store: MemoryCredentialsStore(),
      createStemMix: createStemMix,
    );
    return session;
  }

  Future<void> playFirstTrack(WidgetTester tester) async {
    await tester.enterText(find.byKey(const Key('hostField')), '10.0.0.8');
    await tester.tap(find.byKey(const Key('connectButton')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Radiohead'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('OK Computer'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Airbag'));
    await tester.pumpAndSettle();
  }

  testWidgets('now-playing bar pauses and opens the full player', (
    tester,
  ) async {
    final playback = FakePlayback();
    final session = await connectedAlbum(playback: playback);
    await tester.pumpWidget(ZytunesApp(session: session));
    await playFirstTrack(tester);

    expect(find.byKey(const Key('playPauseButton')), findsOneWidget);
    expect(playback.playing, isTrue);

    await tester.tap(find.byKey(const Key('playPauseButton')));
    await tester.pumpAndSettle();
    expect(playback.playing, isFalse);

    await tester.tap(find.byKey(const Key('nowPlayingTrack')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('playerScreen')), findsOneWidget);
    expect(find.text('Airbag'), findsWidgets);
  });

  testWidgets('full player cycles crossfade duration', (tester) async {
    final playback = FakePlayback();
    final session = await connectedAlbum(playback: playback);
    await tester.pumpWidget(ZytunesApp(session: session));
    await playFirstTrack(tester);

    await tester.tap(find.byKey(const Key('nowPlayingTrack')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('crossfadeButton')), findsOneWidget);
    expect(find.text('Crossfade off'), findsOneWidget);

    await tester.tap(find.byKey(const Key('crossfadeButton')));
    await tester.pumpAndSettle();
    expect(find.text('Crossfade 4s'), findsOneWidget);
    expect(session.crossfade, const Duration(seconds: 4));
  });

  testWidgets('crossfade keeps the current title until the fade ends', (
    tester,
  ) async {
    final outgoing = FakePlayback();
    final incoming = FakePlayback();
    final playback = CrossfadePlayback(
      primary: outgoing,
      secondary: incoming,
      crossfade: const Duration(seconds: 4),
    );
    final session = await connectedAlbum(playback: playback);
    await session.setCrossfade(const Duration(seconds: 4));
    await tester.pumpWidget(ZytunesApp(session: session));
    await playFirstTrack(tester);

    expect(find.text('Airbag'), findsWidgets);

    outgoing.emulatePosition(const Duration(milliseconds: 283000));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 1));

    expect(session.nowPlaying!.name, 'Airbag');
    expect(find.text('Airbag'), findsWidgets);
    playback.dispose();
  });

  testWidgets('full player skip next starts the following track', (
    tester,
  ) async {
    final playback = FakePlayback();
    final session = await connectedAlbum(playback: playback);
    await tester.pumpWidget(ZytunesApp(session: session));
    await playFirstTrack(tester);

    await tester.tap(find.byKey(const Key('nowPlayingTrack')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('nextButton')));
    await tester.pumpAndSettle();

    expect(session.nowPlaying!.name, 'Let Down');
    expect(
      playback.lastUri,
      Uri.parse('http://10.0.0.8:9847/tracks/43/stream'),
    );
  });

  testWidgets('track menu Play next inserts without interrupting', (
    tester,
  ) async {
    final playback = FakePlayback();
    final session = await connectedAlbum(playback: playback);
    await tester.pumpWidget(ZytunesApp(session: session));
    await playFirstTrack(tester);

    await tester.tap(find.byKey(const Key('trackMenu-43')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Play next'));
    await tester.pumpAndSettle();

    expect(session.nowPlaying!.name, 'Airbag');
    expect(session.queue.map((t) => t.name), [
      'Airbag',
      'Let Down',
      'Let Down',
      'Karma Police',
    ]);

    await tester.tap(find.byKey(const Key('nowPlayingTrack')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('nextButton')));
    await tester.pumpAndSettle();
    expect(session.nowPlaying!.name, 'Let Down');
  });

  testWidgets('now-playing bar queue button opens the queue sheet', (
    tester,
  ) async {
    final playback = FakePlayback();
    final session = await connectedAlbum(playback: playback);
    await tester.pumpWidget(ZytunesApp(session: session));
    await playFirstTrack(tester);

    expect(find.byKey(const Key('playerScreen')), findsNothing);
    expect(find.byKey(const Key('miniStemsButton')), findsOneWidget);
    await tester.tap(find.byKey(const Key('queueButton')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('queueSheet')), findsOneWidget);
    expect(find.byKey(const Key('playerScreen')), findsNothing);
  });

  testWidgets('queue sheet removes an upcoming track', (tester) async {
    final playback = FakePlayback();
    final session = await connectedAlbum(playback: playback);
    await tester.pumpWidget(ZytunesApp(session: session));
    await playFirstTrack(tester);

    await tester.tap(find.byKey(const Key('queueButton')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('queueSheet')), findsOneWidget);
    expect(find.text('Let Down'), findsWidgets);

    await tester.tap(find.byKey(const Key('queueRemove-1')));
    await tester.pumpAndSettle();

    expect(session.queue.map((t) => t.name), ['Airbag', 'Karma Police']);

    Navigator.of(tester.element(find.byKey(const Key('queueSheet')))).pop();
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('nextButton')));
    await tester.pumpAndSettle();
    expect(session.nowPlaying!.name, 'Karma Police');
  });

  testWidgets('full player stem chips toggle a cached split', (tester) async {
    final playback = FakePlayback();
    final session = await connectedAlbum(
      playback: playback,
      createStemMix: FakeStemMix.new,
    );
    await tester.pumpWidget(ZytunesApp(session: session));
    await playFirstTrack(tester);

    await tester.tap(find.byKey(const Key('nowPlayingTrack')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('stemsButton')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('stemChips')), findsOneWidget);
    expect(find.byKey(const Key('stemChip-0')), findsOneWidget);
    expect(session.stemPhase, StemPhase.active);

    await tester.tap(find.byKey(const Key('stemChip-0')));
    await tester.pumpAndSettle();
    expect(session.stemEnabled[0], isFalse);
  });

  testWidgets('player shows play count and bumps after the threshold', (
    tester,
  ) async {
    final playback = FakePlayback();
    final session = await connectedAlbum(playback: playback);
    await tester.pumpWidget(ZytunesApp(session: session));
    await playFirstTrack(tester);

    // The meter lives on the full player, not the mini now-playing bar.
    await tester.tap(find.byKey(const Key('nowPlayingTrack')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('playerScreen')), findsOneWidget);
    expect(find.byKey(const Key('playCount')), findsOneWidget);
    expect(find.text('5'), findsWidgets);

    playback.emulatePosition(const Duration(milliseconds: 150000));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
    expect(session.displayedPlayCount, 6);
    expect(find.text('6'), findsWidgets);
    await tester.pumpAndSettle();
  });
}
