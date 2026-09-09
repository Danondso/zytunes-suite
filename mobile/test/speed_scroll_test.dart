import 'package:flutter_test/flutter_test.dart';
import 'package:zytunes_mobile/speed_scroll.dart';

void main() {
  const names = ['Apple', 'Avocado', 'Banana', 'Blue', 'Cherry'];

  test('letterOf uppercases the first character', () {
    expect(letterOf('hello'), 'H');
    expect(letterOf('über'), 'Ü');
    expect(letterOf('123'), '1');
    expect(letterOf(''), ' ');
  });

  test('letter index finds group starts', () {
    expect(nextLetterIndex(names, 0), 2);
    expect(nextLetterIndex(names, 1), 2);
    expect(nextLetterIndex(names, 2), 4);
    expect(nextLetterIndex(names, 4), isNull);
    expect(prevLetterIndex(names, 4), 2);
    expect(prevLetterIndex(names, 3), 0);
    expect(prevLetterIndex(names, 2), 0);
    expect(prevLetterIndex(names, 0), isNull);
  });

  test('stays item-by-item until the 20-row threshold, then lands on the current letter', () {
    final names = [for (var i = 0; i < 25; i++) 'Alpha $i', 'Beta', 'Gamma'];
    final speed = SpeedScroll();
    speed.begin();
    expect(
      speed.addDelta(
        (SpeedScroll.thresholdItems - 1) * SpeedScroll.rowExtent,
        names,
        SpeedScroll.thresholdItems - 1,
      ),
      isNull,
    );
    expect(speed.jumping, isFalse);

    expect(
      speed.addDelta(SpeedScroll.rowExtent, names, SpeedScroll.thresholdItems),
      SpeedScroll.thresholdItems,
    );
    expect(speed.jumping, isTrue);
    expect(speed.pin, SpeedScroll.thresholdItems);
  });

  test('ticks one letter after the hold and does not wrap or skip', () {
    final names = [for (var i = 0; i < 25; i++) 'Alpha $i', 'Beta', 'Gamma'];
    final speed = SpeedScroll();
    speed.begin();
    var t = Duration.zero;
    expect(
      speed.addDelta(
        SpeedScroll.thresholdPx,
        names,
        SpeedScroll.thresholdItems,
        t,
      ),
      SpeedScroll.thresholdItems,
    );

    expect(
      speed.addDelta(SpeedScroll.letterStep * 8, names, 25, t),
      isNull,
      reason: 'hold must land on the current letter before another tick',
    );
    expect(speed.pin, SpeedScroll.thresholdItems);

    t += SpeedScroll.tickHold;
    expect(
      speed.addDelta(
        SpeedScroll.letterStep,
        names,
        SpeedScroll.thresholdItems,
        t,
      ),
      25,
    );
    expect(speed.pin, 25);

    expect(
      speed.addDelta(1000, names, 25, t),
      isNull,
      reason: 'a burst during the hold must not skip ahead',
    );

    t += SpeedScroll.tickHold;
    expect(speed.addDelta(SpeedScroll.letterStep, names, 25, t), 26);
    expect(speed.pin, 26);

    t += SpeedScroll.tickHold;
    expect(
      speed.addDelta(SpeedScroll.letterStep, names, 26, t),
      isNull,
      reason: 'letter ticks do not wrap past the last group',
    );
    expect(speed.pin, 26);
  });

  test('reversing past a letter step ticks the previous group', () {
    final names = [for (var i = 0; i < 25; i++) 'Alpha $i', 'Beta', 'Gamma'];
    final speed = SpeedScroll();
    speed.begin();
    var t = Duration.zero;
    speed.addDelta(
      SpeedScroll.thresholdPx,
      names,
      SpeedScroll.thresholdItems,
      t,
    );
    t += SpeedScroll.tickHold;
    expect(
      speed.addDelta(
        SpeedScroll.letterStep,
        names,
        SpeedScroll.thresholdItems,
        t,
      ),
      25,
    );

    t += SpeedScroll.tickHold;
    expect(
      speed.addDelta(-SpeedScroll.letterStep, names, 25, t),
      0,
      reason: 'from Beta, a reverse click lands on Alpha',
    );
    expect(speed.pin, 0);
  });

  test('a new gesture drops back to item scrolling', () {
    final names = [for (var i = 0; i < 25; i++) 'Alpha $i', 'Beta'];
    final speed = SpeedScroll();
    speed.begin();
    speed.addDelta(SpeedScroll.thresholdPx, names, SpeedScroll.thresholdItems);
    expect(speed.jumping, isTrue);

    speed.end();
    speed.begin();
    expect(speed.addDelta(SpeedScroll.rowExtent, names, 25), isNull);
    expect(speed.jumping, isFalse);
    expect(speed.pin, isNull);
  });

  test('a slow lift does not coast', () {
    final names = [for (var i = 0; i < 25; i++) 'Alpha $i', 'Beta'];
    final speed = SpeedScroll();
    speed.begin();
    var t = Duration.zero;
    speed.addDelta(
      SpeedScroll.thresholdPx,
      names,
      SpeedScroll.thresholdItems,
      t,
    );
    t += const Duration(milliseconds: 200);
    speed.addDelta(
      SpeedScroll.letterStep,
      names,
      SpeedScroll.thresholdItems,
      t,
    );
    expect(speed.release(t), isFalse);
    expect(speed.coasting, isFalse);
  });

  test('a flick coasts and ticks slow down until it stops', () {
    final names = [
      for (final letter in ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H'])
        for (var i = 0; i < 3; i++) '$letter$i',
    ];
    final speed = SpeedScroll();
    speed.begin();
    var t = Duration.zero;
    // Pin at the first letter so the coast has a full alphabet to run.
    speed.addDelta(SpeedScroll.thresholdPx, names, 0, t);

    // Several fast frames at the cap so release() sees a spin.
    for (var i = 0; i < 4; i++) {
      t += const Duration(milliseconds: 16);
      speed.addDelta(SpeedScroll.maxVelocity * 0.016, names, 0, t);
    }
    expect(speed.release(t), isTrue);
    expect(speed.coasting, isTrue);

    final gaps = <int>[];
    Duration? lastTick;
    for (var i = 0; i < 160 && speed.coasting; i++) {
      t += const Duration(milliseconds: 16);
      if (speed.advance(names, t) != null) {
        if (lastTick != null) gaps.add((t - lastTick).inMilliseconds);
        lastTick = t;
      }
    }
    expect(speed.coasting, isFalse);
    expect(gaps.length, greaterThanOrEqualTo(2));
    expect(
      gaps.last,
      greaterThan(gaps.first),
      reason: 'ticks must spread out as velocity decays',
    );
  });
}
