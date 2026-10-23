//! The info panel's Files section: each PDF, spreadsheet and table the session's turns
//! made, with its size and where and when it was written, opening in the app the system
//! opens it with. One moved or deleted since stays, greyed, as not found.

use std::path::{Path, PathBuf};
use std::time::Duration;

use acp_core::SessionKey;
use gpui_kit::assets;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{
    LINE_HEIGHT, PanelLook, ROW_GAP, ROW_PAD, ROW_RADIUS, TEXT_LINE_HEIGHT, TEXT_SIZE, day_or_time,
    local, section_header,
};
use crate::chat_view::ChatView;
use crate::session::{FileKind, MadeFile, Session, watch, watched};

/// How long the watch stays after a turn ends, for the changes FSEvents sends a moment
/// late.
const LAST_CHANGES: Duration = Duration::from_secs(1);
const ICON_SIZE: Pixels = px(14.);
const NAME_SIZE: Pixels = px(13.);
/// A file's second line starts under its name, past its icon.
const LINE_INDENT: Pixels = px(22.);
/// "None yet" lines up with the section's name, past its chevron.
const EMPTY_INDENT: Pixels = px(24.);

impl ChatView {
    /// Watches the session's folder for the files its turn makes, from its prompt on.
    pub(crate) fn watch_files(&mut self, key: SessionKey, cx: &mut Context<Self>) {
        let cwd = self.cwd.clone();
        let Some(session) = self.session_mut(key) else {
            return;
        };
        let folder = session.folder().map_or(cwd, Path::to_path_buf);
        let files = &mut session.files;
        files.turn_started();
        // The last turn's, still taking its last changes.
        if files.watch.is_some() {
            return;
        }
        let (sender, changes) = smol::channel::unbounded();
        files.watch = Some(cx.spawn(async move |this, cx| {
            let shown = folder.clone();
            // Watching a folder can mean walking all of it first, as inotify does.
            let (watching, real) = cx
                .background_executor()
                .spawn(async move {
                    // FSEvents names files by their real path, as /private/tmp for /tmp.
                    let real = std::fs::canonicalize(&folder).unwrap_or(folder);
                    (watch(&real, sender), real)
                })
                .await;
            // Kept until the watch is dropped, which drops this task.
            let Ok(_watcher) = watching else {
                return;
            };
            while let Ok(path) = changes.recv().await {
                if !watched(&path, &real) {
                    continue;
                }
                let path = match path.strip_prefix(&real) {
                    Ok(inside) => shown.join(inside),
                    Err(_) => path,
                };
                if this
                    .update(cx, |this, cx| this.saw_file(key, &path, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn saw_file(&mut self, key: SessionKey, path: &Path, cx: &mut Context<Self>) {
        let Some(session) = self.session_mut(key) else {
            return;
        };
        if session.files.saw(path) {
            self.keep_files(key);
            cx.notify();
        }
    }

    /// Keeps the files the sessions found as their turns ended, and stops watching a
    /// moment after.
    pub(crate) fn note_files(&mut self, cx: &mut Context<Self>) {
        let mut found = Vec::new();
        for session in &mut self.sessions {
            let files = &mut session.files;
            if std::mem::take(&mut files.found) {
                found.push(session.key);
            }
            if !std::mem::take(&mut files.ended) {
                continue;
            }
            let (key, turn) = (session.key, files.turns);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(LAST_CHANGES).await;
                let _ = this.update(cx, |this, _| {
                    if let Some(session) = this.session_mut(key) {
                        // Unless another turn has begun, which keeps the watch.
                        if session.files.turns == turn {
                            session.files.stop_watching();
                        }
                    }
                });
            })
            .detach();
        }
        for key in found {
            self.keep_files(key);
        }
    }

    /// The Files section: its header with how many there are, then each file.
    pub(super) fn render_files_section(
        &self,
        session: &Session,
        look: PanelLook,
        cx: &mut Context<Self>,
    ) -> Div {
        let list = &session.files.list;
        let unfolded = self.info_panel.files_section;
        let header = section_header(
            "info-files-section",
            "Files",
            Some(list.len().to_string()),
            unfolded,
            look,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            this.info_panel.files_section = !this.info_panel.files_section;
            cx.notify();
        }));
        let section = div().flex().flex_col().child(header);
        if !unfolded {
            return section;
        }
        if list.is_empty() {
            return section.child(
                div()
                    .h(LINE_HEIGHT)
                    .pl(EMPTY_INDENT)
                    .pr(ROW_PAD)
                    .flex()
                    .items_center()
                    .text_size(TEXT_SIZE)
                    .line_height(TEXT_LINE_HEIGHT)
                    .text_color(look.faint)
                    .child("None yet"),
            );
        }
        let folder = session.folder().unwrap_or(&self.cwd);
        section.children(
            list.iter()
                .enumerate()
                .map(|(ix, file)| render_file(ix, file, folder, look, cx)),
        )
    }
}

/// A file: its kind's icon, its name and its size; then its folder and when it was
/// written.
fn render_file(
    ix: usize,
    file: &MadeFile,
    folder: &Path,
    look: PanelLook,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let icon = match file.kind() {
        FileKind::Sheet => assets::IconName::FileSpreadsheet,
        FileKind::Document => assets::IconName::FileText,
    };
    let name = file.name();
    let size = if file.missing {
        "Not found".to_string()
    } else {
        shown_size(file.size)
    };
    let mut meta = shown_folder(&file.path, folder);
    if let Some(at) = local(file.at) {
        meta.push_str(&format!(" · {}", day_or_time(at)));
    }
    if file.missing {
        meta.push_str(" · moved or deleted");
    }
    let path: PathBuf = file.path.clone();
    let missing = file.missing;
    div()
        .id(ElementId::Name(format!("info-file-{ix}").into()))
        .role(Role::Button)
        .aria_label(format!("Open {name}"))
        .w_full()
        .px(ROW_PAD)
        .flex()
        .flex_col()
        .rounded(ROW_RADIUS)
        .when(!missing, |row| {
            row.cursor_pointer()
                .hover(move |style| style.bg(look.hover))
        })
        .child(
            div()
                .h(LINE_HEIGHT)
                .w_full()
                .flex()
                .items_center()
                .gap(ROW_GAP)
                .child(
                    Icon::new(icon)
                        .with_size(ICON_SIZE)
                        .flex_none()
                        .text_color(look.muted),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(NAME_SIZE)
                        .line_height(TEXT_LINE_HEIGHT)
                        .text_color(if missing { look.muted } else { look.foreground })
                        .child(name),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(TEXT_SIZE)
                        .text_color(look.muted)
                        .child(size),
                ),
        )
        .child(
            div()
                .h(LINE_HEIGHT)
                .w_full()
                .pl(LINE_INDENT)
                .flex()
                .items_center()
                .text_size(TEXT_SIZE)
                .line_height(TEXT_LINE_HEIGHT)
                .text_color(look.muted)
                .child(div().min_w(px(0.)).truncate().child(meta)),
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            if missing {
                return;
            }
            cx.open_with_system(&path);
            // It may be gone since the list last looked.
            this.sessions[this.active].files.look_again();
            cx.notify();
        }))
        .into_any_element()
}

/// The folder a file is in: from the session's folder, whose own name stands for itself,
/// else from `~`, else in full.
fn shown_folder(path: &Path, folder: &Path) -> String {
    let parent = path.parent().unwrap_or(path);
    if let Ok(inside) = parent.strip_prefix(folder) {
        if inside.as_os_str().is_empty() {
            return folder.file_name().map_or_else(
                || folder.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
        }
        return inside.display().to_string();
    }
    match dirs::home_dir().and_then(|home| parent.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(inside) if inside.as_os_str().is_empty() => "~".into(),
        Some(inside) => format!("~/{}", inside.display()),
        None => parent.display().to_string(),
    }
}

/// "512 B", "18 KB", "1.2 MB", in the 1000s macOS's Finder counts in.
fn shown_size(bytes: u64) -> String {
    const KB: u64 = 1000;
    const MB: u64 = KB * KB;
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{} KB", (bytes + KB / 2) / KB)
    } else {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    }
}

#[cfg(test)]
mod tests {
    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::{shown_folder, shown_size};
    use std::path::Path;

    #[test]
    fn a_file_says_where_it_is_from_the_session_folder() {
        let folder = Path::new("/work/app");
        assert_eq!(
            shown_folder(Path::new("/work/app/out/a.csv"), folder),
            "out"
        );
        assert_eq!(shown_folder(Path::new("/work/app/a.csv"), folder), "app");
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            shown_folder(&home.join("Desktop/r.pdf"), folder),
            "~/Desktop"
        );
        assert_eq!(shown_folder(Path::new("/srv/x/r.pdf"), folder), "/srv/x");
    }

    #[test]
    fn sizes_read_as_finder_writes_them() {
        assert_eq!(shown_size(512), "512 B");
        assert_eq!(shown_size(18_400), "18 KB");
        assert_eq!(shown_size(212_000), "212 KB");
        assert_eq!(shown_size(1_234_567), "1.2 MB");
    }
}
