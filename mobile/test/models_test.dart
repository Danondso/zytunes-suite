import 'package:flutter_test/flutter_test.dart';
import 'package:zytunes_mobile/api/models.dart';

void main() {
  group('AlbumPair', () {
    test('parses snake_case JSON from GET /albums', () {
      final album = AlbumPair.fromJson({
        'artist': 'Radiohead',
        'album': 'OK Computer',
        'year': 1997,
        'track_count': 12,
        'art_url': '/tracks/42/art',
      });
      expect(album.artist, 'Radiohead');
      expect(album.album, 'OK Computer');
      expect(album.year, 1997);
      expect(album.trackCount, 12);
      expect(album.artUrl, '/tracks/42/art');
    });

    test('treats omitted optional album fields as null', () {
      final album = AlbumPair.fromJson({
        'artist': 'Radiohead',
        'album': 'OK Computer',
      });
      expect(album.year, isNull);
      expect(album.trackCount, isNull);
      expect(album.artUrl, isNull);
    });
  });

  group('SearchResults', () {
    test('parses artists, albums, and tracks', () {
      final hits = SearchResults.fromJson({
        'artists': ['Radiohead'],
        'albums': [
          {'artist': 'Radiohead', 'album': 'OK Computer', 'track_count': 12},
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
      expect(hits.artists, ['Radiohead']);
      expect(hits.albums.single.album, 'OK Computer');
      expect(hits.tracks.single.name, 'Karma Police');
    });

    test('legacy array payload is treated as tracks only', () {
      final hits = SearchResults.fromJson([
        {
          'id': 42,
          'name': 'Karma Police',
          'artist': 'Radiohead',
          'album': 'OK Computer',
        },
      ]);
      expect(hits.artists, isEmpty);
      expect(hits.albums, isEmpty);
      expect(hits.tracks.single.name, 'Karma Police');
    });
  });

  group('TrackSummary', () {
    test('parses a fully populated summary', () {
      final track = TrackSummary.fromJson({
        'id': 42,
        'name': 'Karma Police',
        'artist': 'Radiohead',
        'album': 'OK Computer',
        'track_number': 1,
        'disc_number': 1,
        'duration_ms': 262000,
        'kind': 'FLAC',
      });
      expect(track.id, '42');
      expect(track.name, 'Karma Police');
      expect(track.artist, 'Radiohead');
      expect(track.album, 'OK Computer');
      expect(track.trackNumber, 1);
      expect(track.discNumber, 1);
      expect(track.durationMs, 262000);
      expect(track.kind, 'FLAC');
    });

    test('treats omitted optional fields as null', () {
      final track = TrackSummary.fromJson({
        'id': 7,
        'name': 'Untitled',
        'artist': 'Unknown',
        'album': 'Demo',
      });
      expect(track.trackNumber, isNull);
      expect(track.discNumber, isNull);
      expect(track.durationMs, isNull);
      expect(track.kind, isNull);
    });

    test('parses a u64 id that exceeds 32 bits', () {
      final track = TrackSummary.fromJson({
        'id': 9876543210,
        'name': 'Big',
        'artist': 'A',
        'album': 'B',
      });
      expect(track.id, '9876543210');
    });

    test('parses a u64 id serialized as a decimal string', () {
      final track = TrackSummary.fromJson({
        'id': '18446744073709551615',
        'name': 'Huge',
        'artist': 'A',
        'album': 'B',
      });
      expect(track.id, '18446744073709551615');
    });
  });

  group('TrackDetail', () {
    test('parses flattened summary plus URLs and extended metadata', () {
      final detail = TrackDetail.fromJson({
        'id': 42,
        'name': 'Karma Police',
        'artist': 'Radiohead',
        'album': 'OK Computer',
        'track_number': 1,
        'duration_ms': 262000,
        'kind': 'FLAC',
        'genre': 'Alternative',
        'year': 1997,
        'album_artist': 'Radiohead',
        'composer': 'Thom Yorke',
        'sample_rate': 44100,
        'channels': 2,
        'bit_depth': 16,
        'audio_bitrate_kbps': 1411,
        'file_size_bytes': 32000000,
        'mb_recording_id': 'rec-1',
        'mb_release_id': 'rel-1',
        'replaygain_track_gain': '-6.20 dB',
        'stream_url': '/tracks/42/stream',
        'file_url': '/tracks/42/file',
        'art_url': '/tracks/42/art',
        'play_count': 3,
      });
      expect(detail.summary.id, '42');
      expect(detail.summary.name, 'Karma Police');
      expect(detail.genre, 'Alternative');
      expect(detail.year, 1997);
      expect(detail.sampleRate, 44100);
      expect(detail.channels, 2);
      expect(detail.bitDepth, 16);
      expect(detail.fileSizeBytes, 32000000);
      expect(detail.streamUrl, '/tracks/42/stream');
      expect(detail.fileUrl, '/tracks/42/file');
      expect(detail.artUrl, '/tracks/42/art');
      expect(detail.playCount, 3);
    });

    test('allows missing extended metadata', () {
      final detail = TrackDetail.fromJson({
        'id': 1,
        'name': 'Song',
        'artist': 'A',
        'album': 'B',
        'stream_url': '/tracks/1/stream',
        'file_url': '/tracks/1/file',
        'art_url': '/tracks/1/art',
      });
      expect(detail.genre, isNull);
      expect(detail.mbRecordingId, isNull);
      expect(detail.streamUrl, '/tracks/1/stream');
      expect(detail.playCount, isNull);
    });
  });

  group('PlayRecord', () {
    test('parses play_count and last_played_at_ms', () {
      final record = PlayRecord.fromJson({
        'play_count': 3,
        'last_played_at_ms': 1700000000000,
      });
      expect(record.playCount, 3);
      expect(record.lastPlayedAtMs, 1700000000000);
    });

    test('treats omitted counts as zero', () {
      final record = PlayRecord.fromJson({});
      expect(record.playCount, 0);
      expect(record.lastPlayedAtMs, 0);
    });
  });

  group('sortAlbumTracks', () {
    test('orders by disc, then track number, then name', () {
      TrackSummary t({
        required String id,
        required String name,
        int? disc,
        int? number,
      }) {
        return TrackSummary(
          id: id,
          name: name,
          artist: 'A',
          album: 'B',
          discNumber: disc,
          trackNumber: number,
        );
      }

      final sorted = sortAlbumTracks([
        t(id: '1', name: 'Zed', disc: 2, number: 1),
        t(id: '2', name: 'Beta', disc: 1, number: 2),
        t(id: '3', name: 'Alpha', disc: 1, number: 2),
        t(id: '4', name: 'Intro', disc: 1, number: 1),
        t(id: '5', name: 'Bonus'),
      ]);
      expect(sorted.map((x) => x.id).toList(), ['4', '3', '2', '1', '5']);
    });
  });

  group('StemSetInfo', () {
    test('parses a ready six-stem payload', () {
      final info = StemSetInfo.fromJson({
        'status': 'ready',
        'recipe': 'demucs',
        'layout': ['vocals', 'drums', 'bass', 'guitar', 'piano', 'other'],
        'stems': [
          {
            'kind': 'vocals',
            'label': 'Vocals',
            'short_label': 'Voc',
            'url': '/tracks/42/stems/vocals',
          },
        ],
        'engine_available': true,
      });
      expect(info.status, StemJobStatus.ready);
      expect(info.recipe, 'demucs');
      expect(info.stems.single.kind, 'vocals');
      expect(info.stems.single.shortLabel, 'Voc');
      expect(info.engineAvailable, isTrue);
    });

    test('unknown status is missing; omitted stems is empty', () {
      final info = StemSetInfo.fromJson({'status': 'nope'});
      expect(info.status, StemJobStatus.missing);
      expect(info.stems, isEmpty);
      expect(info.engineAvailable, isFalse);
    });
  });
}
