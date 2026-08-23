import 'dart:math' as math;

import 'package:flutter/material.dart';

/// Play count with a speedometer needle that sweeps on each increment.
class PlayCountMeter extends StatefulWidget {
  const PlayCountMeter({
    super.key,
    required this.count,
    this.compact = false,
  });

  final int count;
  final bool compact;

  @override
  State<PlayCountMeter> createState() => _PlayCountMeterState();
}

class _PlayCountMeterState extends State<PlayCountMeter>
    with SingleTickerProviderStateMixin {
  static const _left = 0.08;
  static const _right = 0.94;

  late final AnimationController _controller;
  late final Animation<double> _needle;

  @override
  void initState() {
    super.initState();
    _controller = AnimationController(
      vsync: this,
      duration: const Duration(milliseconds: 900),
    );
    _needle = Tween<double>(begin: _left, end: _right).animate(
      CurvedAnimation(parent: _controller, curve: Curves.easeOutCubic),
    );
  }

  @override
  void didUpdateWidget(PlayCountMeter oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (widget.count == oldWidget.count + 1) {
      _controller.forward(from: 0);
    } else if (widget.count != oldWidget.count) {
      _controller.reset();
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final textTheme = Theme.of(context).textTheme;
    final gauge = widget.compact ? 36.0 : 52.0;
    final numberStyle = widget.compact
        ? textTheme.titleSmall
        : textTheme.titleLarge;
    final label = widget.count == 1 ? 'play' : 'plays';
    return Row(
      key: const Key('playCount'),
      mainAxisSize: MainAxisSize.min,
      children: [
        AnimatedBuilder(
          animation: _needle,
          builder: (context, _) {
            return CustomPaint(
              size: Size(gauge, gauge * 0.68),
              painter: _NeedlePainter(
                t: _controller.isDismissed ? _left : _needle.value,
                color: scheme.primary,
                track: scheme.outline,
              ),
            );
          },
        ),
        const SizedBox(width: 8),
        AnimatedSwitcher(
          duration: const Duration(milliseconds: 560),
          switchInCurve: Curves.easeOutCubic,
          switchOutCurve: Curves.easeIn,
          transitionBuilder: (child, animation) {
            final rotate = Tween(begin: math.pi / 2, end: 0.0).animate(
              CurvedAnimation(parent: animation, curve: Curves.linear),
            );
            return AnimatedBuilder(
              animation: rotate,
              child: child,
              builder: (context, child) {
                return Transform(
                  alignment: Alignment.center,
                  transform: Matrix4.identity()
                    ..setEntry(3, 2, 0.002)
                    ..rotateX(rotate.value),
                  child: child,
                );
              },
            );
          },
          child: Text(
            '${widget.count}',
            key: ValueKey(widget.count),
            style: numberStyle?.copyWith(
              fontFeatures: const [FontFeature.tabularFigures()],
              color: scheme.primary,
            ),
          ),
        ),
        const SizedBox(width: 6),
        Text(label, style: textTheme.bodySmall),
      ],
    );
  }
}

class _NeedlePainter extends CustomPainter {
  _NeedlePainter({
    required this.t,
    required this.color,
    required this.track,
  });

  final double t;
  final Color color;
  final Color track;

  @override
  void paint(Canvas canvas, Size size) {
    final center = Offset(size.width / 2, size.height * 0.92);
    final radius = size.width * 0.46;
    const start = math.pi * 0.92;
    const sweep = math.pi * 1.16;

    canvas.drawArc(
      Rect.fromCircle(center: center, radius: radius),
      start,
      sweep,
      false,
      Paint()
        ..color = track
        ..style = PaintingStyle.stroke
        ..strokeWidth = 2.4
        ..strokeCap = StrokeCap.round,
    );

    final angle = start + sweep * t.clamp(0.0, 1.0);
    final tip = Offset(
      center.dx + math.cos(angle) * radius * 0.86,
      center.dy + math.sin(angle) * radius * 0.86,
    );
    canvas.drawLine(
      center,
      tip,
      Paint()
        ..color = color
        ..strokeWidth = 2.2
        ..strokeCap = StrokeCap.round,
    );
    canvas.drawCircle(center, 2.6, Paint()..color = color);
  }

  @override
  bool shouldRepaint(_NeedlePainter old) =>
      old.t != t || old.color != color || old.track != track;
}
