use super::{CharacterChunk, KittyImageChunk, Output, SixelImageChunk};
use crate::panes::PaneId;
use crate::tab::Pane;
use crate::ClientId;
use std::collections::HashSet;
use zellij_utils::errors::prelude::*;

const NESTED_FRAME_HEADER: &str = "\u{1b}P26661n";
const NESTED_FRAME_TERMINATOR: &str = "\u{1b}\\";

pub(crate) fn strip_nested_session_frames(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut remaining = raw;
    while let Some(start) = remaining.find(NESTED_FRAME_HEADER) {
        output.push_str(&remaining[..start]);
        let frame = &remaining[start + NESTED_FRAME_HEADER.len()..];
        let Some(end) = frame.find(NESTED_FRAME_TERMINATOR) else {
            output.push_str(&remaining[start..]);
            return output;
        };
        remaining = &frame[end + NESTED_FRAME_TERMINATOR.len()..];
    }
    output.push_str(remaining);
    output
}

impl Output {
    pub fn capture_panes(&mut self, panes: impl Iterator<Item = PaneId>) {
        self.captured_panes.extend(panes);
    }
    pub fn has_pane_render(&self, pane: PaneId) -> bool {
        self.pane_assets.contains_key(&pane)
    }
    pub fn capture_pane_render(
        &mut self,
        pane: PaneId,
        characters: &[CharacterChunk],
        raw: &Option<String>,
        sixels: &[SixelImageChunk],
        kitty: &[KittyImageChunk],
    ) {
        if self.captured_panes.contains(&pane) {
            self.pane_assets.insert(
                pane,
                (
                    characters.to_vec(),
                    raw.clone(),
                    sixels.to_vec(),
                    kitty.to_vec(),
                ),
            );
        }
    }
    pub fn render_pane_client(
        &mut self,
        client: ClientId,
        pane_id: PaneId,
        pane: &dyn Pane,
    ) -> Result<()> {
        let clear = self
            .pre_vte_instructions
            .remove(&client)
            .map(|instructions| {
                instructions
                    .iter()
                    .any(|instruction| instruction.contains("[2J"))
            })
            .unwrap_or(false);
        if clear {
            self.add_pre_vte_instruction_to_client(client, "\x1b[2J");
        }
        self.post_vte_instructions.remove(&client);
        self.sixel_chunks.remove(&client);
        self.client_kitty_chunks.remove(&client);
        self.client_rendered_kitty_panes.remove(&client);
        self.client_pane_rects.insert(client, Vec::new());
        self.client_character_chunks.insert(client, Vec::new());
        self.client_kitty_visible_panes
            .insert(client, HashSet::from([pane_id]));
        self.popup_covers.remove(&client);
        // Unrelated panes rendering must not create output activity in this terminal.
        if !clear && !self.has_pane_render(pane_id) {
            return Ok(());
        }
        let x = pane.get_content_x();
        let y = pane.get_content_y();
        if let Some((mut characters, raw, mut sixels, mut kitty)) =
            self.pane_assets.get(&pane_id).cloned()
        {
            for chunk in &mut characters {
                chunk.x = chunk.x.saturating_sub(x);
                chunk.y = chunk.y.saturating_sub(y);
                for highlight in &mut chunk.selection_and_colors {
                    highlight.selection.start.line.0 -= y as isize;
                    highlight.selection.end.line.0 -= y as isize;
                    highlight.selection.start.column.0 =
                        highlight.selection.start.column.0.saturating_sub(x);
                    highlight.selection.end.column.0 =
                        highlight.selection.end.column.0.saturating_sub(x);
                }
            }
            self.client_character_chunks.insert(client, characters);
            for chunk in &mut sixels {
                chunk.cell_x = chunk.cell_x.saturating_sub(x);
                chunk.cell_y = chunk.cell_y.saturating_sub(y);
            }
            self.sixel_chunks.insert(client, sixels);
            for chunk in &mut kitty {
                chunk.cell_x = chunk.cell_x.saturating_sub(x);
                chunk.cell_y = chunk.cell_y.saturating_sub(y);
            }
            self.client_kitty_chunks
                .entry(client)
                .or_default()
                .insert(pane_id, kitty);
            self.client_rendered_kitty_panes
                .entry(client)
                .or_default()
                .insert(pane_id);
            if let Some(raw) = raw {
                let raw = strip_nested_session_frames(&raw);
                if !raw.is_empty() {
                    self.add_post_vte_instruction_to_client(
                        client,
                        &format!("\x1b[1;1H\x1b[m{raw}"),
                    );
                }
            }
        }
        let mouse = if pane.terminal_emulator_wants_mouse() {
            "\x1b[?1000h\x1b[?1002h\x1b[?1006h"
        } else {
            "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l"
        };
        self.add_post_vte_instruction_to_client(client, mouse);
        if let Some((x, y, visible)) = pane.cursor_coordinates(Some(client)) {
            self.set_client_cursor(client, x, y, visible, &pane.cursor_shape_csi());
            self.add_post_vte_instruction_to_client(
                client,
                &format!(
                    "\x1b[{};{}H{}{}",
                    y + 1,
                    x + 1,
                    pane.cursor_shape_csi(),
                    if visible { "\x1b[?25h" } else { "\x1b[?25l" }
                ),
            );
        } else {
            self.add_post_vte_instruction_to_client(client, "\x1b[?25l");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::strip_nested_session_frames;

    #[test]
    fn pane_clients_do_not_receive_nested_session_frames() {
        assert_eq!(
            strip_nested_session_frames("before\x1bP26661nAQID\x1b\\after"),
            "beforeafter"
        );
    }

    #[test]
    fn incomplete_nested_session_frames_are_preserved() {
        assert_eq!(
            strip_nested_session_frames("before\x1bP26661npartial"),
            "before\x1bP26661npartial"
        );
    }
}
