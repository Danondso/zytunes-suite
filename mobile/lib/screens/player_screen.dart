import 'package:flutter/material.dart';

import '../session.dart';
import 'play_count_meter.dart';
import 'queue_sheet.dart';

String formatDuration(Duration d) {
  final minutes = d.inMinutes;
  final seconds = d.inSeconds.remainder(60).toString().padLeft(2, '0');
  return '$minutes:$seconds';
}

/// Shared prev / play / next / queue cluster for the mini bar and full player.
class PlayerControls extends StatelessWidget {
  const PlayerControls({
    super.key,
    required this.session,
    this.compact = false,
  });

  final Session session;
  final bool compact;

  @override
  Widget build(BuildContext context) {
    final iconSize = compact ? 28.0 : 36.0;
    final playSize = compact ? 28.0 : 40.0;
    final density = compact ? VisualDensity.compact : null;
    final stemsOn = session.stemPhase == StemPhase.active;
    Widget sideButton({
      Key? key,
      required IconData icon,
      required VoidCallback onPressed,
    }) => IconButton(
      key: key,
      iconSize: iconSize,
      visualDensity: density,
      onPressed: onPressed,
      icon: Icon(icon),
    );
    final stems = sideButton(
      key: compact ? const Key('miniStemsButton') : null,
      icon: stemsOn ? Icons.graphic_eq : Icons.graphic_eq_outlined,
      onPressed: session.toggleStems,
    );
    return Row(
      children: [
        if (compact)
          stems
        else
          IgnorePointer(child: Opacity(opacity: 0, child: stems)),
        Expanded(
          child: Row(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              IconButton(
                key: const Key('prevButton'),
                iconSize: iconSize,
                visualDensity: density,
                onPressed: session.skipPrevious,
                icon: const Icon(Icons.skip_previous),
              ),
              IconButton.filled(
                key: const Key('playPauseButton'),
                iconSize: playSize,
                visualDensity: density,
                onPressed: session.togglePause,
                icon: Icon(
                  session.player.playing ? Icons.pause : Icons.play_arrow,
                ),
              ),
              IconButton(
                key: const Key('nextButton'),
                iconSize: iconSize,
                visualDensity: density,
                onPressed: session.skipNext,
                icon: const Icon(Icons.skip_next),
              ),
            ],
          ),
        ),
        sideButton(
          key: const Key('queueButton'),
          icon: Icons.queue_music,
          onPressed: () => showQueueSheet(context, session),
        ),
      ],
    );
  }
}

class SeekBar extends StatelessWidget {
  const SeekBar({super.key, required this.session, this.compact = false});

  final Session session;
  final bool compact;

  @override
  Widget build(BuildContext context) {
    final duration = session.displayDuration;
    final position = session.player.position;
    final maxMs = duration.inMilliseconds;
    final value = maxMs == 0
        ? 0.0
        : (position.inMilliseconds / maxMs).clamp(0.0, 1.0);
    final theme = SliderTheme.of(context).copyWith(
      trackHeight: compact ? 3 : null,
      overlayShape: compact ? SliderComponentShape.noOverlay : null,
      thumbShape: compact
          ? const RoundSliderThumbShape(enabledThumbRadius: 6)
          : null,
    );
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        SliderTheme(
          data: theme,
          child: Slider(
            key: const Key('seekSlider'),
            value: value,
            onChanged: session.canSeek
                ? (v) =>
                      session.seek(Duration(milliseconds: (v * maxMs).round()))
                : null,
          ),
        ),
        Padding(
          padding: EdgeInsets.symmetric(horizontal: compact ? 8 : 0),
          child: Row(
            mainAxisAlignment: MainAxisAlignment.spaceBetween,
            children: [
              Text(formatDuration(position)),
              Text(formatDuration(duration)),
            ],
          ),
        ),
      ],
    );
  }
}

class PlayerScreen extends StatelessWidget {
  const PlayerScreen({super.key, required this.session});

  final Session session;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: Listenable.merge([session, session.player]),
      builder: (context, _) {
        final track = session.nowPlaying;
        if (track == null) {
          return const SizedBox.shrink();
        }
        final art = session.artUriFor(track);
        return SafeArea(
          key: const Key('playerScreen'),
          child: Padding(
            padding: const EdgeInsets.fromLTRB(24, 8, 24, 24),
            child: Column(
              children: [
                Align(
                  alignment: Alignment.centerLeft,
                  child: IconButton(
                    onPressed: () => Navigator.of(context).pop(),
                    icon: const Icon(Icons.expand_more),
                  ),
                ),
                Expanded(
                  child: Center(
                    child: AspectRatio(
                      aspectRatio: 1,
                      child: ClipRRect(
                        borderRadius: BorderRadius.circular(12),
                        child: art == null
                            ? const ColoredBox(
                                color: Colors.black26,
                                child: Icon(Icons.album, size: 96),
                              )
                            : Image.network(
                                art.toString(),
                                key: ValueKey(track.id),
                                headers: session.authHeaders,
                                fit: BoxFit.cover,
                                errorBuilder: (context, error, stack) =>
                                    const ColoredBox(
                                      color: Colors.black26,
                                      child: Icon(Icons.album, size: 96),
                                    ),
                              ),
                      ),
                    ),
                  ),
                ),
                const SizedBox(height: 16),
                Text(
                  track.name,
                  textAlign: TextAlign.center,
                  style: Theme.of(context).textTheme.headlineSmall,
                ),
                const SizedBox(height: 8),
                Text(
                  '${track.artist} · ${track.album}',
                  textAlign: TextAlign.center,
                  style: Theme.of(context).textTheme.bodyLarge,
                ),
                if (session.displayedPlayCount != null) ...[
                  const SizedBox(height: 10),
                  PlayCountMeter(
                    key: ValueKey(track.id),
                    count: session.displayedPlayCount!,
                  ),
                ],
                const SizedBox(height: 8),
                StemStrip(session: session),
                SeekBar(session: session),
                PlayerControls(session: session),
                const SizedBox(height: 4),
                TextButton(
                  key: const Key('crossfadeButton'),
                  onPressed: session.cycleCrossfade,
                  child: Text(
                    session.crossfade == Duration.zero
                        ? 'Crossfade off'
                        : 'Crossfade ${session.crossfade.inSeconds}s',
                  ),
                ),
              ],
            ),
          ),
        );
      },
    );
  }
}

class StemStrip extends StatelessWidget {
  const StemStrip({super.key, required this.session});

  final Session session;

  static const _topRow = 4;

  @override
  Widget build(BuildContext context) {
    final label = switch (session.stemPhase) {
      StemPhase.off => 'Stems',
      StemPhase.separating =>
        session.stemProgress == null
            ? 'Separating…'
            : 'Separating ${session.stemProgress}%',
      StemPhase.caching =>
        session.stemProgress == null || session.stemProgress == 0
            ? 'Downloading…'
            : 'Downloading ${session.stemProgress}%',
      StemPhase.active => 'Stems on',
      StemPhase.failed => 'Stems',
    };
    final stems = session.stemSet?.stems;
    final split = stems == null
        ? 0
        : (stems.length < _topRow ? stems.length : _topRow);
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        TextButton.icon(
          key: const Key('stemsButton'),
          style: TextButton.styleFrom(
            iconSize: 32,
            textStyle: Theme.of(context).textTheme.titleMedium,
          ),
          onPressed: session.nowPlaying == null ? null : session.toggleStems,
          icon: Icon(
            session.stemPhase == StemPhase.active
                ? Icons.graphic_eq
                : Icons.graphic_eq_outlined,
          ),
          label: Text(label),
        ),
        if (session.stemError != null)
          Text(
            session.stemError!,
            key: const Key('stemsError'),
            textAlign: TextAlign.center,
            style: TextStyle(color: Theme.of(context).colorScheme.error),
          ),
        if (session.stemPhase == StemPhase.active && stems != null)
          Column(
            key: const Key('stemChips'),
            mainAxisSize: MainAxisSize.min,
            children: [
              _chipRow(0, split),
              if (split < stems.length) ...[
                const SizedBox(height: 4),
                _chipRow(split, stems.length),
              ],
            ],
          ),
      ],
    );
  }

  Widget _chipRow(int from, int to) {
    final stems = session.stemSet!.stems;
    return Row(
      mainAxisAlignment: MainAxisAlignment.center,
      children: [
        for (var i = from; i < to; i++) ...[
          if (i > from) const SizedBox(width: 8),
          FilterChip(
            key: Key('stemChip-$i'),
            showCheckmark: false,
            label: Text(stems[i].shortLabel),
            selected: i < session.stemEnabled.length && session.stemEnabled[i],
            onSelected: (_) => session.toggleStemAt(i),
          ),
        ],
      ],
    );
  }
}
