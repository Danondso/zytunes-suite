import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

const defaultThemeId = 'bedfellow-light';

/// Bedfellow brand colors (`bedfellow/src/theme/colors/brandColors.ts`).
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

  static const tealLight = Color(0xFF00AEAE);
  static const sageLight = Color(0xFF8FBBA8);
  static const rustLight = Color(0xFFD46A45);
}

class AppTheme {
  const AppTheme({required this.id, required this.name, required this.data});

  final String id;
  final String name;
  final ThemeData data;
}

String resolveThemeId(String? id) {
  if (id != null && appThemes.any((theme) => theme.id == id)) return id;
  return defaultThemeId;
}

ThemeData themeDataFor(String? id) {
  final resolved = resolveThemeId(id);
  return appThemes.firstWhere((theme) => theme.id == resolved).data;
}

final appThemes = <AppTheme>[
  AppTheme(
    id: 'bedfellow-light',
    name: 'Bedfellow Light',
    data: _materialFrom(_bedfellowLightScheme),
  ),
  AppTheme(
    id: 'bedfellow-dark',
    name: 'Bedfellow Dark',
    data: _materialFrom(_bedfellowDarkScheme),
  ),
  ..._tuiPalettes.map(
    (palette) => AppTheme(
      id: palette.id,
      name: palette.name,
      data: _materialFrom(_schemeFromTui(palette)),
    ),
  ),
];

const _bedfellowLightScheme = ColorScheme(
  brightness: Brightness.light,
  primary: BedfellowColors.teal,
  onPrimary: BedfellowColors.sand50,
  primaryContainer: BedfellowColors.sand300,
  onPrimaryContainer: BedfellowColors.slate900,
  secondary: BedfellowColors.sage,
  onSecondary: BedfellowColors.slate900,
  secondaryContainer: BedfellowColors.sand100,
  onSecondaryContainer: BedfellowColors.slate900,
  tertiary: BedfellowColors.rust,
  onTertiary: BedfellowColors.sand50,
  error: BedfellowColors.rust,
  onError: BedfellowColors.sand50,
  surface: BedfellowColors.sand50,
  onSurface: BedfellowColors.slate900,
  onSurfaceVariant: BedfellowColors.slate600,
  outline: Color(0x4D535A63),
  outlineVariant: Color(0x26535A63),
  inverseSurface: BedfellowColors.slate900,
  onInverseSurface: BedfellowColors.sand50,
  inversePrimary: BedfellowColors.tealLight,
  surfaceContainerLowest: BedfellowColors.whiteWarm,
  surfaceContainerLow: BedfellowColors.sand50,
  surfaceContainer: BedfellowColors.sand100,
  surfaceContainerHigh: BedfellowColors.sand100,
  surfaceContainerHighest: BedfellowColors.sand300,
);

const _bedfellowDarkScheme = ColorScheme(
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

ThemeData _materialFrom(ColorScheme scheme) {
  final overlay = scheme.brightness == Brightness.light
      ? SystemUiOverlayStyle.dark
      : SystemUiOverlayStyle.light;
  return ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
    scaffoldBackgroundColor: scheme.surface,
    appBarTheme: AppBarTheme(
      backgroundColor: scheme.surface,
      foregroundColor: scheme.onSurface,
      elevation: 0,
      scrolledUnderElevation: 0,
      systemOverlayStyle: overlay,
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

class _TuiPalette {
  const _TuiPalette({
    required this.id,
    required this.name,
    required this.sidebarBg,
    required this.sidebarText,
    required this.selectionBg,
    required this.selectionText,
    required this.mainBg,
    required this.altRowBg,
    required this.border,
    required this.footerBg,
    required this.footerText,
    required this.headerText,
    required this.dimText,
    required this.errorText,
    required this.successText,
    required this.progressBar,
    required this.progressBg,
    required this.accentSecondary,
  });

  final String id;
  final String name;
  final Color sidebarBg;
  final Color sidebarText;
  final Color selectionBg;
  final Color selectionText;
  final Color mainBg;
  final Color altRowBg;
  final Color border;
  final Color footerBg;
  final Color footerText;
  final Color headerText;
  final Color dimText;
  final Color errorText;
  final Color successText;
  final Color progressBar;
  final Color progressBg;
  final Color accentSecondary;
}

double _luminance(Color color) {
  return 0.2126 * color.r + 0.7152 * color.g + 0.0722 * color.b;
}

double _contrast(Color a, Color b) {
  final la = _luminance(a);
  final lb = _luminance(b);
  final lighter = la > lb ? la : lb;
  final darker = la > lb ? lb : la;
  return (lighter + 0.05) / (darker + 0.05);
}

Color _pickOn(Color background, Color a, Color b) {
  return _contrast(background, a) >= _contrast(background, b) ? a : b;
}

const _white = Color(0xFFFFFFFF);
const _nearBlack = Color(0xFF111111);

ColorScheme _schemeFromTui(_TuiPalette palette) {
  final brightness = _luminance(palette.mainBg) > 0.55
      ? Brightness.light
      : Brightness.dark;
  return ColorScheme(
    brightness: brightness,
    primary: palette.selectionBg,
    onPrimary: palette.selectionText,
    primaryContainer: palette.progressBg,
    onPrimaryContainer: _pickOn(
      palette.progressBg,
      palette.sidebarText,
      palette.headerText,
    ),
    secondary: palette.successText,
    onSecondary: _pickOn(palette.successText, _white, _nearBlack),
    secondaryContainer: palette.altRowBg,
    onSecondaryContainer: _pickOn(
      palette.altRowBg,
      palette.sidebarText,
      palette.headerText,
    ),
    tertiary: palette.accentSecondary,
    onTertiary: _pickOn(palette.accentSecondary, _white, _nearBlack),
    error: palette.errorText,
    onError: _pickOn(palette.errorText, _white, _nearBlack),
    surface: palette.mainBg,
    onSurface: _pickOn(palette.mainBg, palette.sidebarText, palette.headerText),
    onSurfaceVariant: _pickOn(
      palette.mainBg,
      palette.dimText,
      palette.footerText,
    ),
    outline: palette.border,
    outlineVariant: palette.border.withValues(alpha: 0.45),
    inverseSurface: palette.sidebarBg,
    onInverseSurface: _pickOn(
      palette.sidebarBg,
      palette.sidebarText,
      palette.headerText,
    ),
    inversePrimary: palette.progressBar,
    surfaceContainerLowest: palette.mainBg,
    surfaceContainerLow: palette.altRowBg,
    surfaceContainer: palette.sidebarBg,
    surfaceContainerHigh: palette.footerBg,
    surfaceContainerHighest: palette.progressBg,
  );
}

/// Color slots copied from `app/src/tui/theme.rs` `THEMES`.
const _tuiPalettes = <_TuiPalette>[
  _TuiPalette(
    id: 'itunes-2004',
    name: 'iTunes 2004',
    sidebarBg: Color.fromARGB(255, 225, 228, 232),
    sidebarText: Color.fromARGB(255, 30, 30, 30),
    selectionBg: Color.fromARGB(255, 56, 117, 215),
    selectionText: _white,
    mainBg: Color.fromARGB(255, 255, 255, 255),
    altRowBg: Color.fromARGB(255, 222, 232, 250),
    border: Color.fromARGB(255, 180, 180, 180),
    footerBg: Color.fromARGB(255, 200, 203, 207),
    footerText: Color.fromARGB(255, 40, 40, 40),
    headerText: Color.fromARGB(255, 80, 80, 80),
    dimText: Color.fromARGB(255, 140, 140, 140),
    errorText: Color.fromARGB(255, 200, 50, 50),
    successText: Color.fromARGB(255, 50, 160, 50),
    progressBar: Color.fromARGB(255, 56, 117, 215),
    progressBg: Color.fromARGB(255, 220, 220, 220),
    accentSecondary: Color.fromARGB(255, 56, 117, 215),
  ),
  _TuiPalette(
    id: 'gruvbox-dark',
    name: 'Gruvbox Dark',
    sidebarBg: Color.fromARGB(255, 50, 48, 47),
    sidebarText: Color.fromARGB(255, 235, 219, 178),
    selectionBg: Color.fromARGB(255, 214, 93, 14),
    selectionText: Color.fromARGB(255, 40, 40, 40),
    mainBg: Color.fromARGB(255, 40, 40, 40),
    altRowBg: Color.fromARGB(255, 68, 64, 60),
    border: Color.fromARGB(255, 102, 92, 84),
    footerBg: Color.fromARGB(255, 50, 48, 47),
    footerText: Color.fromARGB(255, 189, 174, 147),
    headerText: Color.fromARGB(255, 168, 153, 132),
    dimText: Color.fromARGB(255, 124, 111, 100),
    errorText: Color.fromARGB(255, 204, 36, 29),
    successText: Color.fromARGB(255, 152, 151, 26),
    progressBar: Color.fromARGB(255, 214, 93, 14),
    progressBg: Color.fromARGB(255, 60, 56, 54),
    accentSecondary: Color.fromARGB(255, 214, 93, 14),
  ),
  _TuiPalette(
    id: 'gruvbox-light',
    name: 'Gruvbox Light',
    sidebarBg: Color.fromARGB(255, 242, 229, 188),
    sidebarText: Color.fromARGB(255, 60, 56, 54),
    selectionBg: Color.fromARGB(255, 175, 58, 3),
    selectionText: Color.fromARGB(255, 251, 241, 199),
    mainBg: Color.fromARGB(255, 251, 241, 199),
    altRowBg: Color.fromARGB(255, 226, 208, 162),
    border: Color.fromARGB(255, 168, 153, 132),
    footerBg: Color.fromARGB(255, 213, 196, 161),
    footerText: Color.fromARGB(255, 60, 56, 54),
    headerText: Color.fromARGB(255, 102, 92, 84),
    dimText: Color.fromARGB(255, 146, 131, 116),
    errorText: Color.fromARGB(255, 157, 0, 6),
    successText: Color.fromARGB(255, 121, 116, 14),
    progressBar: Color.fromARGB(255, 175, 58, 3),
    progressBg: Color.fromARGB(255, 213, 196, 161),
    accentSecondary: Color.fromARGB(255, 175, 58, 3),
  ),
  _TuiPalette(
    id: 'everforest-dark',
    name: 'Everforest Dark',
    sidebarBg: Color.fromARGB(255, 45, 53, 59),
    sidebarText: Color.fromARGB(255, 211, 198, 170),
    selectionBg: Color.fromARGB(255, 167, 192, 128),
    selectionText: Color.fromARGB(255, 45, 53, 59),
    mainBg: Color.fromARGB(255, 39, 46, 51),
    altRowBg: Color.fromARGB(255, 58, 70, 78),
    border: Color.fromARGB(255, 78, 90, 97),
    footerBg: Color.fromARGB(255, 45, 53, 59),
    footerText: Color.fromARGB(255, 167, 192, 128),
    headerText: Color.fromARGB(255, 135, 144, 130),
    dimText: Color.fromARGB(255, 90, 101, 99),
    errorText: Color.fromARGB(255, 230, 126, 128),
    successText: Color.fromARGB(255, 167, 192, 128),
    progressBar: Color.fromARGB(255, 167, 192, 128),
    progressBg: Color.fromARGB(255, 52, 61, 68),
    accentSecondary: Color.fromARGB(255, 167, 192, 128),
  ),
  _TuiPalette(
    id: 'everforest-light',
    name: 'Everforest Light',
    sidebarBg: Color.fromARGB(255, 239, 239, 225),
    sidebarText: Color.fromARGB(255, 92, 106, 114),
    selectionBg: Color.fromARGB(255, 141, 161, 1),
    selectionText: Color.fromARGB(255, 253, 246, 227),
    mainBg: Color.fromARGB(255, 253, 246, 227),
    altRowBg: Color.fromARGB(255, 230, 226, 210),
    border: Color.fromARGB(255, 186, 189, 175),
    footerBg: Color.fromARGB(255, 221, 222, 208),
    footerText: Color.fromARGB(255, 92, 106, 114),
    headerText: Color.fromARGB(255, 130, 140, 130),
    dimText: Color.fromARGB(255, 160, 166, 152),
    errorText: Color.fromARGB(255, 241, 104, 100),
    successText: Color.fromARGB(255, 141, 161, 1),
    progressBar: Color.fromARGB(255, 141, 161, 1),
    progressBg: Color.fromARGB(255, 221, 222, 208),
    accentSecondary: Color.fromARGB(255, 141, 161, 1),
  ),
  _TuiPalette(
    id: 'tokyo-night',
    name: 'Tokyo Night',
    sidebarBg: Color.fromARGB(255, 22, 22, 30),
    sidebarText: Color.fromARGB(255, 192, 202, 245),
    selectionBg: Color.fromARGB(255, 54, 74, 130),
    selectionText: Color.fromARGB(255, 192, 202, 245),
    mainBg: Color.fromARGB(255, 26, 27, 38),
    altRowBg: Color.fromARGB(255, 36, 40, 59),
    border: Color.fromARGB(255, 65, 72, 104),
    footerBg: Color.fromARGB(255, 22, 22, 30),
    footerText: Color.fromARGB(255, 122, 162, 247),
    headerText: Color.fromARGB(255, 187, 154, 247),
    dimText: Color.fromARGB(255, 114, 125, 163),
    errorText: Color.fromARGB(255, 247, 118, 142),
    successText: Color.fromARGB(255, 158, 206, 106),
    progressBar: Color.fromARGB(255, 122, 162, 247),
    progressBg: Color.fromARGB(255, 36, 40, 59),
    accentSecondary: Color.fromARGB(255, 125, 207, 255),
  ),
  _TuiPalette(
    id: 'ibm-mainframe',
    name: 'IBM Mainframe',
    sidebarBg: Color.fromARGB(255, 0, 0, 0),
    sidebarText: Color.fromARGB(255, 40, 200, 80),
    selectionBg: Color.fromARGB(255, 40, 200, 80),
    selectionText: Color.fromARGB(255, 0, 0, 0),
    mainBg: Color.fromARGB(255, 0, 0, 0),
    altRowBg: Color.fromARGB(255, 14, 26, 14),
    border: Color.fromARGB(255, 0, 110, 40),
    footerBg: Color.fromARGB(255, 0, 30, 10),
    footerText: Color.fromARGB(255, 40, 200, 80),
    headerText: Color.fromARGB(255, 0, 170, 60),
    dimText: Color.fromARGB(255, 0, 85, 30),
    errorText: Color.fromARGB(255, 220, 70, 70),
    successText: Color.fromARGB(255, 40, 200, 80),
    progressBar: Color.fromARGB(255, 40, 200, 80),
    progressBg: Color.fromARGB(255, 0, 30, 10),
    accentSecondary: Color.fromARGB(255, 40, 200, 80),
  ),
  _TuiPalette(
    id: 'amber-crt',
    name: 'Amber CRT',
    sidebarBg: Color.fromARGB(255, 0, 0, 0),
    sidebarText: Color.fromARGB(255, 255, 176, 0),
    selectionBg: Color.fromARGB(255, 255, 176, 0),
    selectionText: Color.fromARGB(255, 20, 10, 0),
    mainBg: Color.fromARGB(255, 0, 0, 0),
    altRowBg: Color.fromARGB(255, 30, 18, 0),
    border: Color.fromARGB(255, 170, 100, 0),
    footerBg: Color.fromARGB(255, 40, 22, 0),
    footerText: Color.fromARGB(255, 255, 176, 0),
    headerText: Color.fromARGB(255, 255, 200, 60),
    dimText: Color.fromARGB(255, 140, 80, 0),
    errorText: Color.fromARGB(255, 255, 90, 60),
    successText: Color.fromARGB(255, 255, 210, 80),
    progressBar: Color.fromARGB(255, 255, 176, 0),
    progressBg: Color.fromARGB(255, 40, 22, 0),
    accentSecondary: Color.fromARGB(255, 255, 200, 60),
  ),
  _TuiPalette(
    id: 'windows-95',
    name: 'Windows 95',
    sidebarBg: Color.fromARGB(255, 192, 192, 192),
    sidebarText: Color.fromARGB(255, 0, 0, 0),
    selectionBg: Color.fromARGB(255, 0, 0, 128),
    selectionText: Color.fromARGB(255, 255, 255, 255),
    mainBg: Color.fromARGB(255, 255, 255, 255),
    altRowBg: Color.fromARGB(255, 210, 210, 210),
    border: Color.fromARGB(255, 128, 128, 128),
    footerBg: Color.fromARGB(255, 192, 192, 192),
    footerText: Color.fromARGB(255, 0, 0, 0),
    headerText: Color.fromARGB(255, 0, 0, 128),
    dimText: Color.fromARGB(255, 128, 128, 128),
    errorText: Color.fromARGB(255, 255, 0, 0),
    successText: Color.fromARGB(255, 0, 128, 0),
    progressBar: Color.fromARGB(255, 0, 0, 128),
    progressBg: Color.fromARGB(255, 192, 192, 192),
    accentSecondary: Color.fromARGB(255, 0, 0, 128),
  ),
  _TuiPalette(
    id: 'system-7',
    name: 'System 7',
    sidebarBg: Color.fromARGB(255, 204, 204, 204),
    sidebarText: Color.fromARGB(255, 0, 0, 0),
    selectionBg: Color.fromARGB(255, 0, 0, 0),
    selectionText: Color.fromARGB(255, 255, 255, 255),
    mainBg: Color.fromARGB(255, 255, 255, 255),
    altRowBg: Color.fromARGB(255, 238, 238, 238),
    border: Color.fromARGB(255, 0, 0, 0),
    footerBg: Color.fromARGB(255, 204, 204, 204),
    footerText: Color.fromARGB(255, 0, 0, 0),
    headerText: Color.fromARGB(255, 51, 51, 51),
    dimText: Color.fromARGB(255, 136, 136, 136),
    errorText: Color.fromARGB(255, 200, 0, 0),
    successText: Color.fromARGB(255, 0, 128, 0),
    progressBar: Color.fromARGB(255, 0, 0, 0),
    progressBg: Color.fromARGB(255, 204, 204, 204),
    accentSecondary: Color.fromARGB(255, 0, 0, 0),
  ),
  _TuiPalette(
    id: 'bios',
    name: 'BIOS',
    sidebarBg: Color.fromARGB(255, 0, 0, 170),
    sidebarText: Color.fromARGB(255, 170, 170, 170),
    selectionBg: Color.fromARGB(255, 170, 170, 170),
    selectionText: Color.fromARGB(255, 0, 0, 170),
    mainBg: Color.fromARGB(255, 0, 0, 170),
    altRowBg: Color.fromARGB(255, 0, 0, 120),
    border: Color.fromARGB(255, 85, 85, 255),
    footerBg: Color.fromARGB(255, 0, 0, 100),
    footerText: Color.fromARGB(255, 255, 255, 85),
    headerText: Color.fromARGB(255, 255, 255, 255),
    dimText: Color.fromARGB(255, 85, 85, 255),
    errorText: Color.fromARGB(255, 255, 85, 85),
    successText: Color.fromARGB(255, 85, 255, 85),
    progressBar: Color.fromARGB(255, 255, 255, 85),
    progressBg: Color.fromARGB(255, 0, 0, 100),
    accentSecondary: Color.fromARGB(255, 85, 255, 85),
  ),
  _TuiPalette(
    id: 'red-sands',
    name: 'Red Sands',
    sidebarBg: Color.fromARGB(255, 88, 26, 16),
    sidebarText: Color.fromARGB(255, 212, 196, 168),
    selectionBg: Color.fromARGB(255, 210, 163, 58),
    selectionText: Color.fromARGB(255, 52, 12, 8),
    mainBg: Color.fromARGB(255, 122, 37, 24),
    altRowBg: Color.fromARGB(255, 90, 26, 16),
    border: Color.fromARGB(255, 160, 90, 60),
    footerBg: Color.fromARGB(255, 72, 20, 12),
    footerText: Color.fromARGB(255, 212, 196, 168),
    headerText: Color.fromARGB(255, 230, 200, 160),
    dimText: Color.fromARGB(255, 212, 176, 136),
    errorText: Color.fromARGB(255, 255, 100, 80),
    successText: Color.fromARGB(255, 180, 210, 90),
    progressBar: Color.fromARGB(255, 210, 163, 58),
    progressBg: Color.fromARGB(255, 88, 26, 16),
    accentSecondary: Color.fromARGB(255, 200, 80, 40),
  ),
  _TuiPalette(
    id: 'newport-lights',
    name: 'Newport Lights',
    sidebarBg: Color.fromARGB(255, 0, 106, 95),
    sidebarText: Color.fromARGB(255, 230, 240, 235),
    selectionBg: Color.fromARGB(255, 255, 255, 255),
    selectionText: Color.fromARGB(255, 0, 80, 70),
    mainBg: Color.fromARGB(255, 0, 130, 115),
    altRowBg: Color.fromARGB(255, 0, 108, 95),
    border: Color.fromARGB(255, 180, 220, 210),
    footerBg: Color.fromARGB(255, 0, 90, 80),
    footerText: Color.fromARGB(255, 230, 240, 235),
    headerText: Color.fromARGB(255, 255, 255, 255),
    dimText: Color.fromARGB(255, 220, 240, 230),
    errorText: Color.fromARGB(255, 255, 100, 80),
    successText: Color.fromARGB(255, 180, 255, 200),
    progressBar: Color.fromARGB(255, 255, 255, 255),
    progressBg: Color.fromARGB(255, 0, 80, 70),
    accentSecondary: Color.fromARGB(255, 255, 255, 255),
  ),
  _TuiPalette(
    id: 'nextstep',
    name: 'NeXTSTEP',
    sidebarBg: Color.fromARGB(255, 43, 43, 43),
    sidebarText: Color.fromARGB(255, 230, 230, 230),
    selectionBg: Color.fromARGB(255, 96, 112, 140),
    selectionText: Color.fromARGB(255, 255, 255, 255),
    mainBg: Color.fromARGB(255, 170, 170, 170),
    altRowBg: Color.fromARGB(255, 153, 153, 153),
    border: Color.fromARGB(255, 30, 30, 30),
    footerBg: Color.fromARGB(255, 43, 43, 43),
    footerText: Color.fromARGB(255, 230, 230, 230),
    headerText: Color.fromARGB(255, 20, 20, 20),
    dimText: Color.fromARGB(255, 85, 85, 85),
    errorText: Color.fromARGB(255, 170, 40, 40),
    successText: Color.fromARGB(255, 40, 110, 60),
    progressBar: Color.fromARGB(255, 96, 112, 140),
    progressBg: Color.fromARGB(255, 136, 136, 136),
    accentSecondary: Color.fromARGB(255, 96, 112, 140),
  ),
  _TuiPalette(
    id: 'winamp-classic',
    name: 'WinAmp Classic',
    sidebarBg: Color.fromARGB(255, 26, 26, 26),
    sidebarText: Color.fromARGB(255, 0, 255, 0),
    selectionBg: Color.fromARGB(255, 0, 255, 0),
    selectionText: Color.fromARGB(255, 0, 0, 0),
    mainBg: Color.fromARGB(255, 20, 20, 20),
    altRowBg: Color.fromARGB(255, 32, 32, 32),
    border: Color.fromARGB(255, 74, 74, 74),
    footerBg: Color.fromARGB(255, 10, 10, 10),
    footerText: Color.fromARGB(255, 0, 255, 0),
    headerText: Color.fromARGB(255, 0, 255, 0),
    dimText: Color.fromARGB(255, 0, 120, 0),
    errorText: Color.fromARGB(255, 255, 60, 60),
    successText: Color.fromARGB(255, 255, 220, 0),
    progressBar: Color.fromARGB(255, 0, 255, 0),
    progressBg: Color.fromARGB(255, 20, 20, 20),
    accentSecondary: Color.fromARGB(255, 255, 220, 0),
  ),
  _TuiPalette(
    id: 'zune-original',
    name: 'Zune Original',
    sidebarBg: Color.fromARGB(255, 10, 10, 10),
    sidebarText: Color.fromARGB(255, 235, 235, 235),
    selectionBg: Color.fromARGB(255, 232, 0, 164),
    selectionText: Color.fromARGB(255, 255, 255, 255),
    mainBg: Color.fromARGB(255, 92, 51, 23),
    altRowBg: Color.fromARGB(255, 77, 43, 19),
    border: Color.fromARGB(255, 188, 134, 92),
    footerBg: Color.fromARGB(255, 20, 20, 20),
    footerText: Color.fromARGB(255, 235, 141, 0),
    headerText: Color.fromARGB(255, 232, 0, 164),
    dimText: Color.fromARGB(255, 180, 150, 120),
    errorText: Color.fromARGB(255, 255, 80, 80),
    successText: Color.fromARGB(255, 166, 226, 46),
    progressBar: Color.fromARGB(255, 232, 0, 164),
    progressBg: Color.fromARGB(255, 26, 26, 26),
    accentSecondary: Color.fromARGB(255, 235, 141, 0),
  ),
];
