import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:zytunes_mobile/screens/play_count_meter.dart';

void main() {
  testWidgets('shows the count and flips to the next value', (tester) async {
    var count = 3;
    await tester.pumpWidget(
      MaterialApp(
        home: StatefulBuilder(
          builder: (context, setState) {
            return Scaffold(
              body: Column(
                children: [
                  PlayCountMeter(count: count),
                  TextButton(
                    onPressed: () => setState(() => count++),
                    child: const Text('bump'),
                  ),
                ],
              ),
            );
          },
        ),
      ),
    );

    expect(find.byKey(const Key('playCount')), findsOneWidget);
    expect(find.text('3'), findsOneWidget);
    expect(find.text('plays'), findsOneWidget);

    await tester.tap(find.text('bump'));
    await tester.pump();
    expect(find.text('4'), findsOneWidget);
    await tester.pumpAndSettle();
    expect(find.text('4'), findsOneWidget);
  });
}
