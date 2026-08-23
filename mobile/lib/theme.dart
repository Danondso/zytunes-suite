import 'package:flutter/material.dart';

/// Bedfellow brand colors (`bedfellow/src/theme/colors/brandColors.ts`)
/// and dark surfaces (`bedfellow/src/theme/themes/dark.ts`).
abstract final class BedfellowColors {
  static const sand50 = Color(0xFFFEF9E0);
  static const sand100 = Color(0xFFFBF2C4);
  static const sand300 = Color(0xFFE5C185);
  static const teal = Color(0xFF008585);
  static const sage = Color(0xFF74A892);
  static const rust = Color(0xFFC7522A);
  static const amber = Color(0xFFD97706);
  static const slate900 = Color(0xFF343941);
  static const slate600 = Color(0xFF535A63);
  static const info = Color(0xFF5E7A7D);
  static const whiteWarm = Color(0xFFFFF9F0);

  static const darkBg = Color(0xFF1A1611);
  static const darkSurface = Color(0xFF221E17);
  static const darkSurfaceHigh = Color(0xFF2A251D);
  static const darkSurfaceHighest = Color(0xFF322C23);
  static const darkBrown = Color(0xFF3A3329);

  /// Teal-400 from Bedfellow's generated scale — dark-mode primary.
  static const tealLight = Color(0xFF00AEAE);

  /// Sage-400 — dark-mode secondary.
  static const sageLight = Color(0xFF8FBBA8);

  /// Rust-400 — dark-mode error / accent.
  static const rustLight = Color(0xFFD46A45);
}

/// Warm dark ColorScheme matching Bedfellow's dark theme.
const bedfellowDarkScheme = ColorScheme(
  brightness: Brightness.dark,
  primary: BedfellowColors.tealLight,
  onPrimary: BedfellowColors.sand50,
  primaryContainer: BedfellowColors.teal,
  onPrimaryContainer: BedfellowColors.sand50,
  secondary: BedfellowColors.sageLight,
  onSecondary: BedfellowColors.darkBg,
  secondaryContainer: BedfellowColors.darkBrown,
  onSecondaryContainer: BedfellowColors.sand50,
  tertiary: BedfellowColors.rust,
  onTertiary: BedfellowColors.sand50,
  error: BedfellowColors.rustLight,
  onError: BedfellowColors.sand50,
  surface: BedfellowColors.darkBg,
  onSurface: BedfellowColors.sand50,
  onSurfaceVariant: BedfellowColors.sand300,
  outline: Color(0x4DFEF9E0),
  outlineVariant: Color(0x26FEF9E0),
  inverseSurface: BedfellowColors.sand50,
  onInverseSurface: BedfellowColors.slate900,
  inversePrimary: BedfellowColors.teal,
  surfaceContainerLowest: BedfellowColors.darkBg,
  surfaceContainerLow: BedfellowColors.darkSurface,
  surfaceContainer: BedfellowColors.darkSurfaceHigh,
  surfaceContainerHigh: BedfellowColors.darkSurfaceHighest,
  surfaceContainerHighest: BedfellowColors.darkBrown,
);

ThemeData bedfellowTheme() {
  final scheme = bedfellowDarkScheme;
  return ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
    scaffoldBackgroundColor: scheme.surface,
    appBarTheme: AppBarTheme(
      backgroundColor: scheme.surface,
      foregroundColor: scheme.onSurface,
      elevation: 0,
      scrolledUnderElevation: 0,
    ),
    navigationBarTheme: NavigationBarThemeData(
      backgroundColor: scheme.surfaceContainer,
    ),
    bottomSheetTheme: BottomSheetThemeData(
      backgroundColor: scheme.surfaceContainer,
    ),
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: scheme.surfaceContainer,
    ),
    chipTheme: ChipThemeData(
      selectedColor: scheme.secondaryContainer,
      checkmarkColor: scheme.secondary,
    ),
  );
}
