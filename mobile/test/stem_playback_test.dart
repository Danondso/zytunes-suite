import 'package:flutter_test/flutter_test.dart';
import 'package:zytunes_mobile/api/models.dart';
import 'package:zytunes_mobile/stem_playback.dart';

void main() {
  TrackSummary track() =>
      const TrackSummary(id: '1', name: 'A', artist: 'B', album: 'C');

  Future<StemPlayback> start({
    FakeStemMix? mix,
    List<Duration> lengths = const [],
    Duration position = Duration.zero,
    List<bool> enabled = const [true, true],
  }) async {
    final engine = mix ?? FakeStemMix(lengths: lengths);
    final playback = StemPlayback(engine);
    await playback.playStems(
      track: track(),
      uris: [Uri.parse('file:///a.flac'), Uri.parse('file:///b.flac')],
      headers: const {},
      enabled: enabled,
      position: position,
    );
    return playback;
  }

  test('mute is gain zero on that stem only', () async {
    final playback = await start();
    final mix = playback.mix as FakeStemMix;

    await playback.setEnabled(1, false);

    expect(mix.volumes[0], 1);
    expect(mix.volumes[1], 0);
    expect(mix.playing, isTrue);
    expect(mix.uris.first, Uri.parse('file:///a.flac'));
    playback.dispose();
  });

  test('playStems starts every stem on one clock', () async {
    final playback = await start(position: const Duration(seconds: 12));
    final mix = playback.mix as FakeStemMix;

    expect(mix.playing, isTrue);
    expect(mix.position, const Duration(seconds: 12));
    expect(mix.uris, hasLength(2));
    playback.dispose();
  });

  test(
    'duration is the longest stem; a short stem does not end the mix',
    () async {
      final playback = await start(
        lengths: const [Duration(seconds: 1), Duration(seconds: 5)],
      );
      final mix = playback.mix as FakeStemMix;
      var ended = 0;
      playback.completed.listen((_) => ended++);

      expect(mix.duration, const Duration(seconds: 5));

      mix.endStem(0);
      expect(ended, 0);
      expect(mix.playing, isTrue);

      mix.endStem(1);
      expect(ended, 1);
      expect(mix.playing, isFalse);
      playback.dispose();
    },
  );

  test('seek moves the single playhead', () async {
    final playback = await start();
    await playback.seek(const Duration(seconds: 8));
    expect(playback.position, const Duration(seconds: 8));
    playback.dispose();
  });

  test('stop then dispose does not notify a disposed mix', () async {
    final playback = await start();
    await playback.stop();
    playback.dispose();
  });

  test('dispose then stop is a no-op', () async {
    final playback = await start();
    playback.dispose();
    await playback.stop();
  });
}
