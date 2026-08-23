import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:zytunes_mobile/api/models.dart';
import 'package:zytunes_mobile/crossfade.dart';
import 'package:zytunes_mobile/playback.dart';

void main() {
  TrackSummary track(String id, {int durationMs = 10000}) => TrackSummary(
    id: id,
    name: 'Track $id',
    artist: 'A',
    album: 'B',
    durationMs: durationMs,
  );

  PreparedSource src(String id, {int durationMs = 10000}) => PreparedSource(
    track: track(id, durationMs: durationMs),
    streamUri: Uri.parse('http://x/$id'),
    headers: const {},
  );

  Future<void> settle() async {
    await Future<void>.delayed(Duration.zero);
    await Future<void>.delayed(Duration.zero);
    await Future<void>.delayed(const Duration(milliseconds: 20));
  }

  test('fade of zero hard-cuts on completed', () async {
    final a = FakePlayback();
    final b = FakePlayback();
    final xfade = CrossfadePlayback(primary: a, secondary: b);
    var completed = 0;
    var handed = 0;
    xfade.completed.listen((_) => completed++);
    xfade.handedOff.listen((_) => handed++);

    await xfade.play(
      track: track('1'),
      streamUri: Uri.parse('http://x/1'),
      headers: const {},
    );
    await xfade.prepareNext(src('2'));
    a.emulatePosition(const Duration(seconds: 9));
    await settle();

    expect(b.playing, isFalse);
    expect(handed, 0);

    a.emitCompleted();
    expect(completed, 1);
    expect(handed, 0);
  });

  test('starts the next engine when remaining <= fade', () async {
    final a = FakePlayback();
    final b = FakePlayback();
    final xfade = CrossfadePlayback(
      primary: a,
      secondary: b,
      crossfade: const Duration(milliseconds: 200),
    );
    var handed = 0;
    xfade.handedOff.listen((_) => handed++);

    await xfade.play(
      track: track('1'),
      streamUri: Uri.parse('http://x/1'),
      headers: const {},
    );
    await xfade.prepareNext(src('2'));
    a.emulatePosition(const Duration(milliseconds: 9800));
    await settle();

    expect(handed, 0);
    expect(xfade.position, const Duration(milliseconds: 9800));
    expect(a.playing, isTrue);
    expect(b.playing, isTrue);
    expect(b.playCount, 0);
    expect(b.lastUri, Uri.parse('http://x/2'));

    await Future<void>.delayed(const Duration(milliseconds: 100));
    expect(handed, 0);
    expect(a.volume, closeTo(0.5, 0.25));
    expect(b.volume, closeTo(0.5, 0.25));
    expect(xfade.position, const Duration(milliseconds: 9800));
  });

  test('play cuts immediately and drops the incoming engine', () async {
    final a = FakePlayback();
    final b = FakePlayback();
    final xfade = CrossfadePlayback(
      primary: a,
      secondary: b,
      crossfade: const Duration(milliseconds: 200),
    );

    await xfade.play(
      track: track('1'),
      streamUri: Uri.parse('http://x/1'),
      headers: const {},
    );
    await xfade.prepareNext(src('2'));
    a.emulatePosition(const Duration(milliseconds: 9800));
    await settle();
    expect(b.playing, isTrue);

    await xfade.play(
      track: track('3'),
      streamUri: Uri.parse('http://x/3'),
      headers: const {},
    );

    expect(b.playing, isFalse);
    expect(a.lastUri, Uri.parse('http://x/3'));
    expect(a.volume, 1);
    expect(xfade.playing, isTrue);
  });

  test('seek does not start a fade', () async {
    final a = FakePlayback();
    final b = FakePlayback();
    final xfade = CrossfadePlayback(
      primary: a,
      secondary: b,
      crossfade: const Duration(milliseconds: 200),
    );
    var handed = 0;
    xfade.handedOff.listen((_) => handed++);

    await xfade.play(
      track: track('1'),
      streamUri: Uri.parse('http://x/1'),
      headers: const {},
    );
    await xfade.prepareNext(src('2'));
    await xfade.seek(const Duration(milliseconds: 9800));
    await settle();

    expect(handed, 0);
    expect(b.playing, isFalse);
    expect(a.position, const Duration(milliseconds: 9800));
  });

  test('finishes the fade after the configured duration', () async {
    final a = FakePlayback();
    final b = FakePlayback();
    final xfade = CrossfadePlayback(
      primary: a,
      secondary: b,
      crossfade: const Duration(milliseconds: 80),
    );
    var handed = 0;
    xfade.handedOff.listen((_) => handed++);

    await xfade.play(
      track: track('1'),
      streamUri: Uri.parse('http://x/1'),
      headers: const {},
    );
    await xfade.prepareNext(src('2'));
    a.emulatePosition(const Duration(milliseconds: 9920));
    await settle();
    expect(a.playing, isTrue);
    expect(b.playing, isTrue);

    await Future<void>.delayed(const Duration(milliseconds: 150));

    expect(a.playing, isFalse);
    expect(b.playing, isTrue);
    expect(b.volume, 1);
    expect(handed, 1);
  });

  test(
    'preloads the following track onto the idle engine after a fade',
    () async {
      final a = FakePlayback();
      final b = FakePlayback();
      final xfade = CrossfadePlayback(
        primary: a,
        secondary: b,
        crossfade: const Duration(milliseconds: 80),
      );

      await xfade.play(
        track: track('1'),
        streamUri: Uri.parse('http://x/1'),
        headers: const {},
      );
      await xfade.prepareNext(src('2'));
      a.emulatePosition(const Duration(milliseconds: 9920));
      await settle();

      await xfade.prepareNext(src('3'));
      await Future<void>.delayed(const Duration(milliseconds: 150));

      expect(a.prepared?.streamUri, Uri.parse('http://x/3'));
      expect(a.playing, isFalse);
      expect(b.playing, isTrue);
    },
  );

  test('fade does not wait for play() to finish the next track', () async {
    final a = FakePlayback();
    final b = _HangOnPlayPlayback();
    final xfade = CrossfadePlayback(
      primary: a,
      secondary: b,
      crossfade: const Duration(milliseconds: 200),
    );
    var handed = 0;
    xfade.handedOff.listen((_) => handed++);

    await xfade.play(
      track: track('1'),
      streamUri: Uri.parse('http://x/1'),
      headers: const {},
    );
    await xfade.prepareNext(src('2'));
    a.emulatePosition(const Duration(milliseconds: 9800));
    await settle().timeout(const Duration(seconds: 1));

    expect(handed, 0);
    expect(xfade.position, const Duration(milliseconds: 9800));
    expect(b.playing, isTrue);
    expect(b.playStarted, isFalse);
  });
}

class _HangOnPlayPlayback extends FakePlayback {
  var playStarted = false;

  @override
  Future<void> play({
    required TrackSummary track,
    required Uri streamUri,
    required Map<String, String> headers,
  }) async {
    playStarted = true;
    await super.play(track: track, streamUri: streamUri, headers: headers);
    await Completer<void>().future;
  }
}
