# Replay design rules

Replay's library window follows Microsoft Fluent 2 and the Windows 11 app guidelines, and the patterns of
the best clip apps (Medal, SteelSeries Moments, Steam game recording, Windows Photos, Clipchamp).

## Sources

- Fluent 2 / Windows app design: [spacing](https://learn.microsoft.com/windows/apps/design/basics/content-basics),
  [geometry](https://learn.microsoft.com/windows/apps/design/signature-experiences/geometry),
  [typography](https://learn.microsoft.com/windows/apps/design/signature-experiences/typography),
  [layering](https://learn.microsoft.com/windows/apps/design/signature-experiences/layering),
  [NavigationView](https://learn.microsoft.com/windows/apps/design/controls/navigationview),
  [app settings](https://learn.microsoft.com/windows/apps/design/app-settings/guidelines-for-app-settings),
  [motion](https://learn.microsoft.com/windows/apps/design/motion/timing-and-easing),
  [color](https://fluent2.microsoft.design/color).
- Nielsen Norman Group: [vertical navigation](https://www.nngroup.com/articles/vertical-nav/),
  [progressive disclosure](https://www.nngroup.com/articles/progressive-disclosure/).
- WCAG 2.2: 4.5:1 for text, 3:1 for control edges, focus and selection.

## Structure

- **Navigation:** Clips (home), Games. Settings pinned at the bottom. The capture status sits above it.
  Labels show from 1008 px wide; below that the rail shows icons only.
- **Clips:** thumbnails are the home screen. A search box, filter chips and a sort/group menu sit above
  the grid. Clips group by day (default) or by game. Selecting clips shows a floating command bar.
- **Editor:** full page. A command bar on top (back, name, favorite, more, share). The video fills the
  middle on black. An inspector on the right holds Trim, Audio, Export and Details. The timeline (transport,
  filmstrip with trim handles, audio waveform) spans the bottom.
- **Games:** a list of rows (icon, name, clips, record switch). A row opens that game's page with its
  quality settings and memory use.
- **Settings:** categories on the left, one column of rows on the right. Changes apply by themselves.

## Tokens (dark)

| Token | Value | Use |
|---|---|---|
| base | `#1C1C1C` | navigation layer |
| layer | `#202020` | page layer |
| card | `#2B2B2B` | settings rows, panels |
| hover / pressed | `#323232` / `#272727` | |
| flyout | `#2C2C2C` + 8% white stroke | menus, dialogs, toasts |
| well | `#000000` | behind video |
| text | `#FFFFFF`, `#C8C8C8`, `#9E9E9E` | primary, secondary, tertiary |
| accent | `#F2649A` (hover `#F57DAA`) | one primary button, selection, playhead, trim handles, nav pill |
| on accent | `#1A0B12` | text on the accent |
| recording | `#FF4D4F` | recording only |
| success | `#6CCB5F` | replay buffer ready |
| caution | `#FCE100` on `#433519` | InfoBar: 16 px warning glyph in caution, the sentence in primary text, 12 px padding, radius 4, full width; an action is a 24 px standard button on the text column |

Type: Segoe UI, 12 / 14 / 18 / 20 / 28, Regular and Semibold only, sentence case. Spacing on a 4 px grid
(4, 8, 12, 16, 24, 32). Radius 4 for controls and thumbnails, 8 for panels, dialogs and toasts.
Motion: 83 ms hover, 167 ms panels, decelerate on enter, no bounce.

## Taste rules

1. Content first: thumbnails and video carry the color; the window chrome stays neutral grey.
2. The accent marks only the main action, the selection, and the playhead and trim handles.
3. Red means recording, nothing else.
4. Pure black behind video, so colors are judged correctly. No gradients or glows over content.
5. One level of progressive disclosure: advanced options behind one expander, never nested.
6. No boxes inside boxes. The gallery sits directly on the page; cards are for settings and panels.
7. Reversible actions get an Undo toast, not a confirmation dialog.
8. One sentence of help in secondary text beats an explanatory card. Never text under 12 px.
