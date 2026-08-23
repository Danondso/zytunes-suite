import 'package:flutter_test/flutter_test.dart';
import 'package:zytunes_mobile/playback_keepalive.dart';

void main() {
  test('start is sent once per title and stop is idempotent', () async {
    final calls = <(String, dynamic)>[];
    final keep = ChannelPlaybackKeepalive(
      invoke: (method, [arguments]) async {
        calls.add((method, arguments));
      },
    );

    await keep.sync(playing: true, title: 'Airbag', artist: 'Radiohead');
    await keep.sync(playing: true, title: 'Airbag', artist: 'Radiohead');
    await keep.sync(playing: true, title: 'Let Down', artist: 'Radiohead');
    await keep.sync(playing: false);
    await keep.sync(playing: false);

    expect(calls.map((c) => c.$1).toList(), ['start', 'start', 'stop']);
    expect(calls[0].$2, {'title': 'Airbag', 'artist': 'Radiohead'});
    expect(calls[1].$2, {'title': 'Let Down', 'artist': 'Radiohead'});
    expect(calls[2].$2, isNull);
  });
}
