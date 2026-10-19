//! The Clips page (home): a command row (search, filter chips, view menu)
//! over a grid of thumbnails grouped by day or by game. Tiles sit directly on
//! the page; selecting clips shows a floating command bar.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Margin, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

use super::theme::{self, Tokens, icon};
use super::{ClipDialog, ClipEntry, LibraryApp, Page};

#[derive(PartialEq, Clone, Copy)]
pub enum Sort {
    Newest,
    Oldest,
    Longest,
    Largest,
    Name,
}

impl Sort {
    const ALL: [Sort; 5] = [Sort::Newest, Sort::Oldest, Sort::Longest, Sort::Largest, Sort::Name];

    fn label(self) -> &'static str {
        match self {
            Sort::Newest => "Newest first",
            Sort::Oldest => "Oldest first",
            Sort::Longest => "Longest first",
            Sort::Largest => "Largest first",
            Sort::Name => "Name",
        }
    }

    fn apply(self, v: &mut [ClipEntry]) {
        match self {
            Sort::Newest => v.sort_by_key(|c| std::cmp::Reverse(c.modified)),
            Sort::Oldest => v.sort_by_key(|c| c.modified),
            Sort::Longest => v.sort_by(|a, b| b.duration.total_cmp(&a.duration)),
            Sort::Largest => v.sort_by_key(|c| std::cmp::Reverse(c.size)),
            Sort::Name => v.sort_by_key(|c| c.name.to_lowercase()),
        }
    }
}

#[derive(PartialEq, Clone, Copy)]
pub enum Group {
    Day,
    Game,
    None,
}

#[derive(PartialEq, Clone)]
pub enum Chip {
    All,
    Favorites,
    Game(String),
    Tag(String),
}

pub struct Gallery {
    pub filter: String,
    pub checked: HashSet<PathBuf>,
    pub sort: Sort,
    pub group: Group,
    pub chip: Chip,
    /// Smallest tile width; the grid fills the row.
    pub tile: f32,
    /// Anchor for Shift-click range selection.
    anchor: Option<PathBuf>,
    /// The first tile's star button this frame (for `--snapshot` hover tests).
    pub probe_star: Option<(u64, Rect)>,
    /// Whether that tile showed its hover buttons this frame.
    pub probe_lit: bool,
}

impl Default for Gallery {
    fn default() -> Self {
        Self { filter: String::new(), checked: HashSet::new(), sort: Sort::Newest, group: Group::Day, chip: Chip::All, tile: 260.0, anchor: None, probe_star: None, probe_lit: false }
    }
}

// ------------------------------------------------------------------- dates

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Local time minus UTC, in seconds (rounded to 15 minutes).
fn utc_offset() -> i64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let lt = crate::util::local_time();
    let local = days_from_civil(i64::from(lt.year), i64::from(lt.month), i64::from(lt.day)) * 86_400
        + i64::from(lt.hour) * 3600
        + i64::from(lt.minute) * 60
        + i64::from(lt.second);
    ((local - now) as f64 / 900.0).round() as i64 * 900
}

/// Local (day number, seconds into the day) of a file time.
fn local_day(t: SystemTime, offset: i64) -> (i64, i64) {
    let s = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) + offset;
    (s.div_euclid(86_400), s.rem_euclid(86_400))
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// "Today", "Yesterday", "Monday", "October 12", "October 12, 2025".
fn day_label(day: i64, today: i64) -> String {
    match today - day {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => WEEKDAYS[(day + 4).rem_euclid(7) as usize].into(),
        _ => {
            let (y, m, d) = civil_from_days(day);
            let (ty, _, _) = civil_from_days(today);
            let month = MONTHS[(m - 1) as usize];
            if y == ty { format!("{month} {d}") } else { format!("{month} {d}, {y}") }
        }
    }
}

fn clock(secs_of_day: i64) -> String {
    let (h, m) = (secs_of_day / 3600, secs_of_day / 60 % 60);
    let (h12, ampm) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{h12}:{m:02} {ampm}")
}

/// The title shown for a clip file (see `display_title`), with the day.
pub(super) fn friendly_title(path: &std::path::Path, game: &str) -> String {
    let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
    let offset = utc_offset();
    let (day, secs) = local_day(modified, offset);
    let today = local_day(SystemTime::now(), offset).0;
    let title = display_title(&name, game, &clock(secs));
    if title == name { name } else { format!("{}, {title}", day_label(day, today)) }
}

/// A clip's title. Default names ("RuneLite 2026-10-19 10-56-45 (trim)")
/// repeat the game and date shown elsewhere, so they become "10:56 AM",
/// "10:56 AM · Trimmed". Names the user chose stay as they are.
fn display_title(name: &str, game: &str, time: &str) -> String {
    let game = crate::util::sanitize_file_name(game);
    let Some(rest) = name.strip_prefix(&game).map(str::trim_start) else { return name.to_string() };
    // "YYYY-MM-DD HH-MM-SS" then optional " (kind)" or " (kind) (2)".
    let b = rest.as_bytes();
    let stamp = b.len() >= 19
        && b[..19].iter().enumerate().all(|(i, c)| match i {
            4 | 7 | 13 | 16 => *c == b'-',
            10 => *c == b' ',
            _ => c.is_ascii_digit(),
        });
    if !stamp {
        return name.to_string();
    }
    let kinds: Vec<&str> = rest[19..].split(['(', ')']).map(str::trim).filter(|s| !s.is_empty() && !s.chars().all(|c| c.is_ascii_digit())).collect();
    let words: Vec<&str> = kinds
        .iter()
        .map(|k| match *k {
            "trim" | "edit" => "Trimmed",
            "joined" => "Combined",
            "montage" => "Montage",
            "session" => "Session",
            "small" => "Small",
            "vertical" => "Vertical",
            "export" => "Export",
            other => other,
        })
        .collect();
    if words.is_empty() { time.to_string() } else { format!("{time}  \u{00b7}  {}", words.join(", ")) }
}

fn short_date(day: i64) -> String {
    let (_, m, d) = civil_from_days(day);
    format!("{} {d}", &MONTHS[(m - 1) as usize][..3])
}

// -------------------------------------------------------------------- page

impl LibraryApp {
    pub(super) fn clips_page(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let ctx = ui.ctx().clone();
        let visible: Vec<ClipEntry> = self.clips.iter().filter(|c| !self.is_pending_delete(&c.path)).cloned().collect();

        ui.label(theme::title("Clips").color(t.text));
        ui.add_space(12.0);
        self.command_row(ui, t, &visible);
        ui.add_space(20.0);

        if visible.is_empty() && self.scan_rx.is_none() {
            self.empty_state(ui, t);
            return;
        }

        // Filter, sort and group.
        let g = &self.gallery;
        let filter = g.filter.to_lowercase();
        let lib = &self.lib;
        let mut shown: Vec<ClipEntry> = visible
            .into_iter()
            .filter(|c| {
                let meta = lib.get(&c.path);
                let text = filter.is_empty()
                    || c.name.to_lowercase().contains(&filter)
                    || c.game.to_lowercase().contains(&filter)
                    || meta.tags.iter().any(|t| t.to_lowercase().contains(&filter));
                let chip = match &g.chip {
                    Chip::All => true,
                    Chip::Favorites => meta.favorite,
                    Chip::Game(name) => &c.game == name,
                    Chip::Tag(tag) => meta.tags.iter().any(|t| t.eq_ignore_ascii_case(tag)),
                };
                text && chip
            })
            .collect();
        g.sort.apply(&mut shown);
        if shown.is_empty() {
            ui.add_space(48.0);
            let mut clear = false;
            ui.vertical_centered(|ui| {
                ui.label(theme::subtitle("No clips match").color(t.text));
                ui.add_space(4.0);
                ui.label(RichText::new("Try another search or filter.").color(t.text2));
                ui.add_space(12.0);
                if theme::button(ui, "Clear search and filters", true).clicked() {
                    clear = true;
                }
            });
            if clear {
                self.gallery.filter.clear();
                self.gallery.chip = Chip::All;
            }
            return;
        }
        let offset = utc_offset();
        let today = local_day(SystemTime::now(), offset).0;
        let mut groups: Vec<(String, Vec<ClipEntry>)> = Vec::new();
        for c in shown {
            let key = match g.group {
                Group::Day => day_label(local_day(c.modified, offset).0, today),
                Group::Game => c.game.clone(),
                Group::None => String::new(),
            };
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, v)) => v.push(c),
                None => groups.push((key, vec![c])),
            }
        }
        let order: Vec<PathBuf> = groups.iter().flat_map(|(_, v)| v.iter().map(|c| c.path.clone())).collect();

        let mut open = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let gap = 12.0;
            for (label, clips) in &groups {
                if !label.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label(theme::subtitle(label).color(t.text));
                        ui.label(theme::caption(format!("\u{00b7}  {}", clips.len())).color(t.text3));
                    });
                    ui.add_space(8.0);
                }
                let avail = ui.available_width();
                let cols = (((avail + gap) / (self.gallery.tile + gap)).floor() as usize).max(1);
                let w = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).floor();
                for row in clips.chunks(cols) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for c in row {
                            if self.tile(ui, t, c, w, offset, &order) {
                                open = Some(c.path.clone());
                            }
                        }
                    });
                    ui.add_space(gap);
                }
            }
            ui.add_space(72.0);
        });
        if let Some(p) = open {
            self.open_editor(&ctx, p);
        }
    }

    fn empty_state(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        ui.add_space(72.0);
        ui.vertical_centered(|ui| {
            if self.shared.icons {
                ui.label(theme::glyph(icon::VIDEO, 40.0).color(t.text3));
                ui.add_space(12.0);
            }
            ui.label(theme::subtitle("No clips yet").color(t.text));
            ui.add_space(8.0);
            ui.label(RichText::new("Play a game, then press").color(t.text2));
            ui.add_space(8.0);
            ui.label(RichText::new(&self.shared.cfg.hotkeys.save_focused).size(28.0).family(egui::FontFamily::Name(theme::SEMIBOLD.into())).color(t.text));
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!("to save the last {}.", theme::fmt_short(f64::from(self.shared.cfg.replay_seconds)))).color(t.text2),
            );
        });
    }

    /// Search, filter chips and the view menu.
    fn command_row(&mut self, ui: &mut egui::Ui, t: &Tokens, clips: &[ClipEntry]) {
        let icons = self.shared.icons;
        let ctx = ui.ctx().clone();
        // Games with the most clips first; tags in use.
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for c in clips {
            *counts.entry(c.game.as_str()).or_default() += 1;
        }
        let mut games: Vec<(&str, usize)> = counts.into_iter().collect();
        games.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let tags = self.lib.all_tags();
        let mut pick = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let r = ui.add(egui::TextEdit::singleline(&mut self.gallery.filter).hint_text("Search clips, games and tags").desired_width(260.0).margin(Margin { left: 32, right: 28, top: 6, bottom: 6 }));
            if icons {
                ui.painter().text(Pos2::new(r.rect.left() + 12.0, r.rect.center().y), Align2::LEFT_CENTER, icon::SEARCH, FontId::new(14.0, egui::FontFamily::Name(theme::ICONS.into())), t.text2);
            }
            if !self.gallery.filter.is_empty() {
                let x = Rect::from_center_size(Pos2::new(r.rect.right() - 14.0, r.rect.center().y), Vec2::splat(22.0));
                let xr = ui.interact(x, r.id.with("clear"), Sense::click());
                if xr.hovered() {
                    ui.painter().rect_filled(x, CornerRadius::same(theme::R_CONTROL), t.subtle_hover);
                }
                let glyph = if icons { icon::CANCEL.to_string() } else { "x".into() };
                ui.painter().text(x.center(), Align2::CENTER_CENTER, glyph, FontId::new(10.0, egui::FontFamily::Name(theme::ICONS.into())), t.text2);
                if xr.on_hover_text("Clear search").clicked() {
                    self.gallery.filter.clear();
                }
            }
            ui.add_space(4.0);
            let g = &self.gallery;
            if theme::chip(ui, t, "All", g.chip == Chip::All).clicked() {
                pick = Some(Chip::All);
            }
            if theme::chip(ui, t, "Favorites", g.chip == Chip::Favorites).clicked() {
                pick = Some(Chip::Favorites);
            }
            if games.len() > 1 {
                for (name, _) in games.iter().take(5) {
                    let on = g.chip == Chip::Game(name.to_string());
                    if theme::chip(ui, t, *name, on).clicked() {
                        pick = Some(if on { Chip::All } else { Chip::Game(name.to_string()) });
                    }
                }
            }
            for tag in tags.iter().take(5) {
                let on = g.chip == Chip::Tag(tag.clone());
                if theme::chip(ui, t, format!("#{tag}"), on).clicked() {
                    pick = Some(if on { Chip::All } else { Chip::Tag(tag.clone()) });
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::button(ui, theme::label_with_icon(icons, icon::FOLDER, if icons { "" } else { "Folder" }), true).on_hover_text("Open the clips folder").clicked() {
                    let _ = std::fs::create_dir_all(&self.shared.cfg.output_dir);
                    crate::sys::shell::open_path(&self.shared.cfg.output_dir);
                }
                let _ = &ctx;
                let g = &mut self.gallery;
                ui.menu_button(theme::label_with_icon(icons, icon::SORT, "View"), |ui| {
                    ui.set_min_width(200.0);
                    ui.label(theme::caption("Group by").color(t.text2));
                    ui.radio_value(&mut g.group, Group::Day, "Day");
                    ui.radio_value(&mut g.group, Group::Game, "Game");
                    ui.radio_value(&mut g.group, Group::None, "Nothing");
                    ui.separator();
                    ui.label(theme::caption("Sort").color(t.text2));
                    for s in Sort::ALL {
                        ui.radio_value(&mut g.sort, s, s.label());
                    }
                    ui.separator();
                    ui.label(theme::caption("Tile size").color(t.text2));
                    let mut size = g.tile as u32;
                    if theme::slider(ui, t, &mut size, 180..=420, false, 180.0).changed() {
                        g.tile = size as f32;
                    }
                });
            });
        });
        if let Some(c) = pick {
            self.gallery.chip = c;
        }
    }

    /// One tile: the thumbnail and two lines under it. Returns true when
    /// it should open in the editor.
    fn tile(&mut self, ui: &mut egui::Ui, t: &Tokens, c: &ClipEntry, w: f32, offset: i64, order: &[PathBuf]) -> bool {
        let icons = self.shared.icons;
        let thumb_h = (w * 9.0 / 16.0).round();
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, thumb_h + 56.0), Sense::click());
        let checked = self.gallery.checked.contains(&c.path);
        let selecting = !self.gallery.checked.is_empty();
        // The pointer anywhere on the tile, also over its own buttons: with
        // `hovered()` the buttons take the hover, vanish, and flicker.
        let hovered = resp.contains_pointer();
        let p = ui.painter_at(rect.expand(3.0));
        let img = Rect::from_min_size(rect.min, Vec2::new(w, thumb_h));
        let r = CornerRadius::same(theme::R_CONTROL);
        p.rect_filled(img, r, t.card);
        let tex = self.thumb(&c.path).map(|t| (t.id(), t.size()));
        if let Some((id, [tw, th])) = tex {
            let have = tw as f32 / th.max(1) as f32;
            let want = img.width() / img.height();
            let uv = if have > want {
                let u = want / have;
                Rect::from_min_max(Pos2::new((1.0 - u) / 2.0, 0.0), Pos2::new((1.0 + u) / 2.0, 1.0))
            } else {
                let v = have / want;
                Rect::from_min_max(Pos2::new(0.0, (1.0 - v) / 2.0), Pos2::new(1.0, (1.0 + v) / 2.0))
            };
            p.add(egui::epaint::RectShape::filled(img, r, Color32::WHITE).with_texture(id, uv));
        }
        if checked {
            p.rect_stroke(img.expand(3.0), CornerRadius::same(theme::R_CONTROL + 2), Stroke::new(2.0, t.accent), StrokeKind::Inside);
        } else if hovered {
            p.rect_stroke(img, r, Stroke::new(1.0, Color32::from_white_alpha(70)), StrokeKind::Inside);
        }

        // "New": saved since the library was last opened.
        if self.seen_before.is_some_and(|s| c.modified > s) {
            let g = p.layout_no_wrap("New".to_string(), FontId::proportional(12.0), Color32::WHITE);
            let size = g.size() + Vec2::new(12.0, 4.0);
            let nr = Rect::from_min_size(Pos2::new(img.left() + 6.0, img.bottom() - size.y - 6.0), size);
            p.rect_filled(nr, r, Color32::from_black_alpha(166));
            p.circle_filled(Pos2::new(nr.left() - 0.0, nr.top()), 0.0, Color32::TRANSPARENT);
            p.galley(nr.min + Vec2::new(6.0, 2.0), g, Color32::WHITE);
        }

        // Duration badge.
        let galley = p.layout_no_wrap(theme::fmt_short(c.duration), FontId::proportional(12.0), Color32::WHITE);
        let size = galley.size() + Vec2::new(10.0, 4.0);
        let br = Rect::from_min_size(img.right_bottom() - size - Vec2::new(6.0, 6.0), size);
        p.rect_filled(br, r, Color32::from_black_alpha(166));
        p.galley(br.min + Vec2::new(5.0, 2.0), galley, Color32::WHITE);

        // Title and caption.
        let info = self.lib.get(&c.path);
        let (day, secs) = local_day(c.modified, offset);
        let title = display_title(&c.name, &c.game, &clock(secs));
        let mut job = egui::text::LayoutJob::single_section(title, egui::TextFormat::simple(FontId::new(14.0, egui::FontFamily::Name(theme::SEMIBOLD.into())), t.text));
        job.wrap = egui::text::TextWrapping::truncate_at_width(w);
        p.galley(Pos2::new(rect.left(), img.bottom() + 8.0), p.layout_job(job), t.text);
        let first = if self.gallery.group == Group::Game { short_date(day) } else { c.game.clone() };
        let mut meta = format!("{first}  \u{00b7}  {:.0} MB", c.size as f64 / 1e6);
        if self.shared.uploads.contains_key(c.path.to_string_lossy().as_ref()) {
            meta.push_str("  \u{00b7}  Uploaded");
        }
        if !info.tags.is_empty() {
            meta.push_str("  \u{00b7}  ");
            meta.push_str(&info.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" "));
        }
        let mut job = egui::text::LayoutJob::single_section(meta, egui::TextFormat::simple(FontId::proportional(12.0), t.text2));
        job.wrap = egui::text::TextWrapping::truncate_at_width(w);
        p.galley(Pos2::new(rect.left(), img.bottom() + 30.0), p.layout_job(job), t.text2);

        // Favorite star: always when set, else on hover.
        let star = Rect::from_min_size(Pos2::new(img.right() - 30.0, img.top() + 6.0), Vec2::splat(24.0));
        let pass = ui.ctx().cumulative_pass_nr();
        if self.gallery.probe_star.is_none_or(|(n, _)| n != pass) {
            self.gallery.probe_star = Some((pass, star));
            self.gallery.probe_lit = hovered;
        }
        let mut star_clicked = false;
        if hovered || info.favorite {
            let sr = ui.interact(star, resp.id.with("star"), Sense::click());
            p.rect_filled(star, r, Color32::from_black_alpha(if sr.hovered() { 200 } else { 140 }));
            let color = if info.favorite { Color32::from_rgb(255, 200, 61) } else { Color32::WHITE };
            if icons {
                let g = if info.favorite { icon::STAR_FILLED } else { icon::STAR };
                p.text(star.center(), Align2::CENTER_CENTER, g, FontId::new(13.0, egui::FontFamily::Name(theme::ICONS.into())), color);
            } else {
                p.text(star.center(), Align2::CENTER_CENTER, if info.favorite { "\u{2605}" } else { "\u{2606}" }, FontId::proportional(14.0), color);
            }
            if sr.on_hover_text(if info.favorite { "Remove from favorites" } else { "Add to favorites" }).clicked() {
                star_clicked = true;
                self.set_favorite(&c.path, !info.favorite);
            }
        }

        // Copy file (for pasting into Discord), on hover.
        if hovered {
            let cp = Rect::from_min_size(Pos2::new(img.right() - 58.0, img.top() + 6.0), Vec2::splat(24.0));
            let cr = ui.interact(cp, resp.id.with("copy"), Sense::click());
            p.rect_filled(cp, r, Color32::from_black_alpha(if cr.hovered() { 200 } else { 140 }));
            let g = if icons { icon::COPY.to_string() } else { "C".into() };
            p.text(cp.center(), Align2::CENTER_CENTER, g, FontId::new(13.0, egui::FontFamily::Name(theme::ICONS.into())), Color32::WHITE);
            if cr.on_hover_text("Copy file, then paste it into Discord").clicked() {
                star_clicked = true;
                if crate::sys::shell::copy_file(&c.path) {
                    self.shared.toast("Copied. Paste it with Ctrl+V.", false);
                }
            }
        }

        // Check box: on hover, or for every tile while selecting.
        let cb = Rect::from_min_size(img.min + Vec2::splat(6.0), Vec2::splat(22.0));
        let mut toggled = false;
        if hovered || checked || selecting {
            let cr = ui.interact(cb, resp.id.with("check"), Sense::click());
            p.rect(
                cb,
                r,
                if checked { t.accent } else { Color32::from_black_alpha(140) },
                Stroke::new(1.0, if checked { t.accent } else { Color32::from_white_alpha(200) }),
                StrokeKind::Inside,
            );
            if checked {
                let c0 = cb.center();
                p.line_segment([c0 + Vec2::new(-5.0, 0.0), c0 + Vec2::new(-1.5, 4.0)], Stroke::new(2.0, t.on_accent));
                p.line_segment([c0 + Vec2::new(-1.5, 4.0), c0 + Vec2::new(5.5, -4.5)], Stroke::new(2.0, t.on_accent));
            }
            if cr.on_hover_text("Select").clicked() {
                toggled = true;
                self.toggle_checked(&c.path);
            }
        }

        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        let mut open = false;
        resp.context_menu(|ui| {
            ui.set_min_width(180.0);
            if ui.button("Open").clicked() {
                open = true;
                ui.close();
            }
            if ui.button(if info.favorite { "Remove from favorites" } else { "Add to favorites" }).clicked() {
                self.set_favorite(&c.path, !info.favorite);
                ui.close();
            }
            if ui.button("Rename\u{2026}").clicked() {
                self.dialog = Some(ClipDialog::Rename(c.path.clone(), c.name.clone()));
                ui.close();
            }
            if ui.button("Tags\u{2026}").clicked() {
                self.dialog = Some(ClipDialog::Tags(vec![c.path.clone()], info.tags.join(", ")));
                ui.close();
            }
            ui.separator();
            if ui.button("Copy file").clicked() {
                if crate::sys::shell::copy_file(&c.path) {
                    self.shared.toast("Copied. Paste it into Discord or a folder.", false);
                }
                ui.close();
            }
            if ui.button("Show in folder").clicked() {
                crate::sys::shell::reveal(&c.path);
                ui.close();
            }
            ui.separator();
            if ui.button(RichText::new("Delete").color(t.critical)).clicked() {
                self.delete_clips(vec![c.path.clone()]);
                ui.close();
            }
        });
        if resp.clicked() && !toggled && !star_clicked {
            let m = ui.input(|i| i.modifiers);
            if m.shift
                && let Some(a) = self.gallery.anchor.clone()
            {
                // Range from the anchor to here, in the order shown.
                let (i, j) = (order.iter().position(|x| *x == a), order.iter().position(|x| *x == c.path));
                if let (Some(i), Some(j)) = (i, j) {
                    for p in &order[i.min(j)..=i.max(j)] {
                        self.gallery.checked.insert(p.clone());
                    }
                }
            } else if m.command || selecting {
                self.toggle_checked(&c.path);
            } else {
                open = true;
            }
        }
        open
    }

    fn toggle_checked(&mut self, p: &PathBuf) {
        if !self.gallery.checked.remove(p) {
            self.gallery.checked.insert(p.clone());
        }
        self.gallery.anchor = Some(p.clone());
    }

    /// Floating command bar for selected clips.
    pub(super) fn selection_bar(&mut self, ctx: &egui::Context, t: &Tokens, area: Rect) {
        if self.gallery.checked.is_empty() || self.page != Page::Clips {
            return;
        }
        let icons = self.shared.icons;
        let n = self.gallery.checked.len();
        let mut action = None;
        egui::Area::new(egui::Id::new("selection-bar"))
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(Pos2::new(area.center().x, area.bottom() - 20.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(t.flyout)
                    .stroke(Stroke::new(1.0, t.card_stroke))
                    .corner_radius(CornerRadius::same(theme::R_CARD))
                    .shadow(ui.visuals().popup_shadow)
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.add_space(4.0);
                            ui.label(RichText::new(format!("{n} selected")).color(t.text));
                            ui.add_space(12.0);
                            let b = |ui: &mut egui::Ui, c: char, label: &str, enabled: bool| {
                                ui.add_enabled(enabled, egui::Button::new(theme::label_with_icon(icons, c, label)).frame_when_inactive(false).min_size(Vec2::new(0.0, theme::CONTROL_H)))
                            };
                            if b(ui, icon::JOIN, "Combine", n >= 2).on_hover_text("Join into one clip, oldest first").on_disabled_hover_text("Select two or more clips").clicked() {
                                action = Some("join");
                            }
                            if b(ui, icon::VIDEO, "Montage", n >= 2).on_hover_text("Choose the order and add music").on_disabled_hover_text("Select two or more clips").clicked() {
                                action = Some("montage");
                            }
                            if b(ui, icon::COPY, "Copy originals", true).on_hover_text("Copy the original files to the clipboard, then paste into a chat or a folder. For Discord, use Copy for Discord in the editor").clicked() {
                                action = Some("copy");
                            }
                            if b(ui, icon::TAG, "Tags", true).clicked() {
                                action = Some("tags");
                            }
                            let (d, _) = ui.allocate_exact_size(Vec2::new(9.0, 20.0), Sense::hover());
                            ui.painter().line_segment([d.center_top(), d.center_bottom()], Stroke::new(1.0, t.divider));
                            if b(ui, icon::DELETE, "Delete", true).clicked() {
                                action = Some("delete");
                            }
                            ui.add_space(8.0);
                            if theme::subtle(ui, theme::label_with_icon(icons, icon::CANCEL, if icons { "" } else { "Clear" })).on_hover_text("Clear the selection").clicked() {
                                action = Some("clear");
                            }
                        });
                    });
            });
        match action {
            Some("join") => self.join_checked(ctx),
            Some("montage") => self.open_montage(),
            Some("tags") => {
                let paths: Vec<PathBuf> = self.gallery.checked.iter().cloned().collect();
                // Start from the tags all selected clips share.
                let mut common: Option<Vec<String>> = None;
                for p in &paths {
                    let tags = self.lib.get(p).tags;
                    common = Some(match common {
                        None => tags,
                        Some(c) => c.into_iter().filter(|t| tags.contains(t)).collect(),
                    });
                }
                self.dialog = Some(ClipDialog::Tags(paths, common.unwrap_or_default().join(", ")));
            }
            Some("delete") => {
                let paths: Vec<PathBuf> = self.gallery.checked.iter().cloned().collect();
                self.delete_clips(paths);
            }
            Some("clear") => self.gallery.checked.clear(),
            Some("copy") => {
                let paths: Vec<PathBuf> = self.gallery.checked.iter().cloned().collect();
                if crate::sys::shell::copy_files(&paths) {
                    self.shared.toast(format!("Copied {} {}. Paste them with Ctrl+V.", paths.len(), if paths.len() == 1 { "clip" } else { "clips" }), false);
                }
            }
            _ => {}
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && self.dialog.is_none() && self.montage.is_none() {
            self.gallery.checked.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_round_trip() {
        for (y, m, d) in [(1970, 1, 1), (2000, 2, 29), (2026, 10, 19), (1999, 12, 31)] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn default_names_become_short_titles() {
        assert_eq!(display_title("RuneLite 2026-10-19 10-56-45", "RuneLite", "10:56 AM"), "10:56 AM");
        assert_eq!(display_title("RuneLite 2026-10-19 10-56-45 (trim) (2)", "RuneLite", "10:56 AM"), "10:56 AM  \u{00b7}  Trimmed");
        assert_eq!(display_title("Big clutch", "RuneLite", "10:56 AM"), "Big clutch");
    }

    #[test]
    fn day_labels() {
        let today = days_from_civil(2026, 10, 19); // a Monday
        assert_eq!(day_label(today, today), "Today");
        assert_eq!(day_label(today - 1, today), "Yesterday");
        assert_eq!(day_label(today - 3, today), "Friday");
        assert_eq!(day_label(today - 10, today), "October 9");
        assert_eq!(day_label(days_from_civil(2025, 3, 2), today), "March 2, 2025");
        assert_eq!(clock(0), "12:00 AM");
        assert_eq!(clock(13 * 3600 + 5 * 60), "1:05 PM");
    }
}
