/// JSON DTOs matching `zytunes-stream` (`docs/stream-api.md`).
library;

int? _asInt(Object? value) {
  if (value == null) return null;
  if (value is int) return value;
  if (value is num) return value.toInt();
  return null;
}

/// Track ids are decimal strings in JSON so u64 hashes survive Dart/JS.
String _asId(Object? value) {
  if (value is String) return value;
  if (value is int) return value.toString();
  if (value is num) return value.toInt().toString();
  throw FormatException('track id must be a string or number, got $value');
}

class AlbumPair {
  const AlbumPair({
    required this.artist,
    required this.album,
    this.year,
    this.trackCount,
    this.artUrl,
  });

  final String artist;
  final String album;
  final int? year;
  final int? trackCount;
  final String? artUrl;

  factory AlbumPair.fromJson(Map<String, dynamic> json) {
    return AlbumPair(
      artist: json['artist'] as String,
      album: json['album'] as String,
      year: _asInt(json['year']),
      trackCount: _asInt(json['track_count']),
      artUrl: json['art_url'] as String?,
    );
  }
}

class SearchResults {
  const SearchResults({
    this.artists = const [],
    this.albums = const [],
    this.tracks = const [],
  });

  final List<String> artists;
  final List<AlbumPair> albums;
  final List<TrackSummary> tracks;

  factory SearchResults.fromJson(Object? json) {
    if (json is List<dynamic>) {
      return SearchResults(
        tracks: json
            .map((e) => TrackSummary.fromJson(e as Map<String, dynamic>))
            .toList(),
      );
    }
    final map = json as Map<String, dynamic>;
    return SearchResults(
      artists: (map['artists'] as List<dynamic>? ?? const []).cast<String>(),
      albums: [
        for (final e in map['albums'] as List<dynamic>? ?? const [])
          AlbumPair.fromJson(e as Map<String, dynamic>),
      ],
      tracks: [
        for (final e in map['tracks'] as List<dynamic>? ?? const [])
          TrackSummary.fromJson(e as Map<String, dynamic>),
      ],
    );
  }
}

class TrackSummary {
  const TrackSummary({
    required this.id,
    required this.name,
    required this.artist,
    required this.album,
    this.trackNumber,
    this.discNumber,
    this.durationMs,
    this.kind,
  });

  final String id;
  final String name;
  final String artist;
  final String album;
  final int? trackNumber;
  final int? discNumber;
  final int? durationMs;
  final String? kind;

  factory TrackSummary.fromJson(Map<String, dynamic> json) {
    return TrackSummary(
      id: _asId(json['id']),
      name: json['name'] as String,
      artist: json['artist'] as String,
      album: json['album'] as String,
      trackNumber: _asInt(json['track_number']),
      discNumber: _asInt(json['disc_number']),
      durationMs: _asInt(json['duration_ms']),
      kind: json['kind'] as String?,
    );
  }
}

class TrackDetail {
  const TrackDetail({
    required this.summary,
    required this.streamUrl,
    required this.fileUrl,
    required this.artUrl,
    this.genre,
    this.year,
    this.albumArtist,
    this.composer,
    this.sampleRate,
    this.channels,
    this.bitDepth,
    this.audioBitrateKbps,
    this.fileSizeBytes,
    this.mbRecordingId,
    this.mbReleaseId,
    this.replaygainTrackGain,
    this.playCount,
  });

  final TrackSummary summary;
  final String? genre;
  final int? year;
  final String? albumArtist;
  final String? composer;
  final int? sampleRate;
  final int? channels;
  final int? bitDepth;
  final int? audioBitrateKbps;
  final int? fileSizeBytes;
  final String? mbRecordingId;
  final String? mbReleaseId;
  final String? replaygainTrackGain;
  final int? playCount;
  final String streamUrl;
  final String fileUrl;
  final String artUrl;

  factory TrackDetail.fromJson(Map<String, dynamic> json) {
    return TrackDetail(
      summary: TrackSummary.fromJson(json),
      genre: json['genre'] as String?,
      year: _asInt(json['year']),
      albumArtist: json['album_artist'] as String?,
      composer: json['composer'] as String?,
      sampleRate: _asInt(json['sample_rate']),
      channels: _asInt(json['channels']),
      bitDepth: _asInt(json['bit_depth']),
      audioBitrateKbps: _asInt(json['audio_bitrate_kbps']),
      fileSizeBytes: _asInt(json['file_size_bytes']),
      mbRecordingId: json['mb_recording_id'] as String?,
      mbReleaseId: json['mb_release_id'] as String?,
      replaygainTrackGain: json['replaygain_track_gain'] as String?,
      playCount: _asInt(json['play_count']),
      streamUrl: json['stream_url'] as String,
      fileUrl: json['file_url'] as String,
      artUrl: json['art_url'] as String,
    );
  }
}

/// Server album lists are unsorted by track number. Missing disc/track
/// numbers sort after numbered tracks so bonuses land at the end.
List<TrackSummary> sortAlbumTracks(List<TrackSummary> tracks) {
  final copy = List<TrackSummary>.from(tracks);
  copy.sort((a, b) {
    final disc = (a.discNumber ?? 1 << 30).compareTo(b.discNumber ?? 1 << 30);
    if (disc != 0) return disc;
    final num = (a.trackNumber ?? 1 << 30).compareTo(b.trackNumber ?? 1 << 30);
    if (num != 0) return num;
    return a.name.toLowerCase().compareTo(b.name.toLowerCase());
  });
  return copy;
}

class PlayRecord {
  const PlayRecord({required this.playCount, required this.lastPlayedAtMs});

  final int playCount;
  final int lastPlayedAtMs;

  factory PlayRecord.fromJson(Map<String, dynamic> json) {
    return PlayRecord(
      playCount: _asInt(json['play_count']) ?? 0,
      lastPlayedAtMs: _asInt(json['last_played_at_ms']) ?? 0,
    );
  }
}

class StemFile {
  const StemFile({
    required this.kind,
    required this.label,
    required this.shortLabel,
    required this.url,
  });

  final String kind;
  final String label;
  final String shortLabel;
  final String url;

  factory StemFile.fromJson(Map<String, dynamic> json) {
    return StemFile(
      kind: json['kind'] as String,
      label: json['label'] as String,
      shortLabel: json['short_label'] as String,
      url: json['url'] as String,
    );
  }
}

enum StemJobStatus { ready, missing, separating, failed }

class StemSetInfo {
  const StemSetInfo({
    required this.status,
    required this.recipe,
    required this.layout,
    required this.stems,
    this.progress,
    this.error,
    this.engineAvailable = false,
  });

  final StemJobStatus status;
  final String recipe;
  final List<String> layout;
  final List<StemFile> stems;
  final int? progress;
  final String? error;
  final bool engineAvailable;

  factory StemSetInfo.fromJson(Map<String, dynamic> json) {
    return StemSetInfo(
      status: _stemStatus(json['status'] as String?),
      recipe: json['recipe'] as String? ?? 'demucs',
      layout: (json['layout'] as List<dynamic>? ?? const []).cast<String>(),
      stems: [
        for (final e in json['stems'] as List<dynamic>? ?? const [])
          StemFile.fromJson(e as Map<String, dynamic>),
      ],
      progress: _asInt(json['progress']),
      error: json['error'] as String?,
      engineAvailable: json['engine_available'] as bool? ?? false,
    );
  }
}

StemJobStatus _stemStatus(String? raw) {
  switch (raw) {
    case 'ready':
      return StemJobStatus.ready;
    case 'separating':
      return StemJobStatus.separating;
    case 'failed':
      return StemJobStatus.failed;
    default:
      return StemJobStatus.missing;
  }
}
