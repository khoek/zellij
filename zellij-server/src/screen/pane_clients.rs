use super::Screen;
use crate::output::Output;
use crate::panes::PaneId;
use crate::{ClientId, ServerInstruction};
use zellij_utils::errors::prelude::*;
use zellij_utils::ipc::{ExitReason, ServerToClientMsg};
use zellij_utils::pane_mirror::MirrorInput;
use zellij_utils::pane_size::Size;

impl Screen {
    pub(super) fn prepare_pane_client(
        &mut self,
        client_id: ClientId,
        pane_id: PaneId,
    ) -> Result<bool> {
        let pane = self
            .tabs
            .values()
            .find_map(|tab| tab.get_pane_with_id(pane_id));
        let reason = match pane {
            None => Some(ExitReason::Error(format!(
                "No terminal pane {pane_id:?} in this session"
            ))),
            Some(pane) if pane.exited() => Some(ExitReason::CustomExitStatus(
                pane.exit_status().unwrap_or(1),
            )),
            Some(_) => None,
        };
        if let Some(exit_reason) = reason {
            if let Some(os_input) = &self.bus.os_input {
                os_input.send_to_client(client_id, ServerToClientMsg::Exit { exit_reason })?;
            }
            return Ok(false);
        }
        if let Some(pane) = self
            .tabs
            .values_mut()
            .find_map(|tab| tab.get_pane_with_id_mut(pane_id))
        {
            pane.render_full_viewport();
        }
        self.pane_clients.insert(client_id, pane_id);
        if let (PaneId::Terminal(pane_id), Some(os_input)) = (pane_id, &self.bus.os_input) {
            os_input.send_to_client(client_id, ServerToClientMsg::PaneAttached { pane_id })?;
        }
        Ok(true)
    }

    pub(super) fn start_pane_client(&mut self, client_id: ClientId) -> Result<()> {
        let Some(pane_id) = self.pane_clients.get(&client_id).copied() else {
            return Ok(());
        };
        if let Some(tab_id) = self.tab_id_containing_pane(pane_id) {
            self.recompute_tab_size(tab_id)?;
            self.tabs.get_mut(&tab_id).map(|tab| tab.set_force_render());
        }
        let held = self
            .tabs
            .values()
            .find_map(|tab| tab.get_pane_with_id(pane_id))
            .map(|pane| pane.is_held() && !pane.exited())
            .unwrap_or(false);
        if held {
            self.pane_client_input(client_id, MirrorInput::Bytes(vec![b'\r']))?;
        }
        Ok(())
    }

    pub(super) fn pane_client_input(
        &mut self,
        client_id: ClientId,
        input: MirrorInput,
    ) -> Result<()> {
        let Some(pane_id) = self.pane_clients.get(&client_id).copied() else {
            return Ok(());
        };
        let Some(tab) = self
            .tabs
            .values_mut()
            .find(|tab| tab.has_pane_with_pid(&pane_id))
        else {
            return self.finish_pane_clients(pane_id, None);
        };
        match input {
            MirrorInput::Key {
                key,
                raw_bytes,
                is_kitty_keyboard_protocol,
            } => {
                tab.write_to_pane_id(
                    &Some(key),
                    raw_bytes,
                    is_kitty_keyboard_protocol,
                    pane_id,
                    None,
                    None,
                )?;
            },
            MirrorInput::Bytes(bytes) => {
                tab.write_to_pane_id(&None, bytes, false, pane_id, None, None)?;
            },
            MirrorInput::Paste(bytes) => {
                tab.paste_to_pane_id(bytes, pane_id, None)?;
            },
            MirrorInput::Mouse(event) => {
                tab.send_mouse_event_to_pane_id(pane_id, &event)?;
            },
        }
        Ok(())
    }

    pub(super) fn finish_pane_clients(
        &mut self,
        pane_id: PaneId,
        status: Option<i32>,
    ) -> Result<()> {
        let clients: Vec<_> = self
            .pane_clients
            .iter()
            .filter_map(|(client, target)| (*target == pane_id).then_some(*client))
            .collect();
        if clients.is_empty() {
            return Ok(());
        }
        if self
            .tabs
            .values()
            .any(|tab| tab.has_pane_with_pid(&pane_id))
        {
            self.render_to_clients()?;
        }
        // A fullscreen application restores its normal buffer before printing its exit summary.
        // Preserve that text across the attachment's own alternate-screen teardown, without
        // replaying a still-active fullscreen UI after a forced close.
        let output = status
            .and_then(|_| {
                self.tabs
                    .values()
                    .find_map(|tab| tab.get_pane_with_id(pane_id))
            })
            .filter(|pane| !pane.is_alternate_mode_active())
            .map(|pane| {
                pane.pane_contents_with_ansi(None, false, None)
                    .viewport
                    .iter()
                    .map(|line| line.trim_end())
                    .collect::<Vec<_>>()
                    .join("\r\n")
                    .trim_matches(['\r', '\n'])
                    .to_owned()
            })
            .unwrap_or_default();
        for client in clients {
            self.pane_clients.remove(&client);
            self.bus
                .senders
                .send_to_server(ServerInstruction::PaneClientExit(
                    client,
                    status.unwrap_or(1),
                    output.clone(),
                ))?;
            self.remove_client(client)?;
        }
        Ok(())
    }

    pub(super) fn pane_client_layout_size(
        &self,
        client_id: ClientId,
        tab_id: usize,
        size: Size,
    ) -> Size {
        let Some(pane_id) = self.pane_clients.get(&client_id) else {
            return size;
        };
        let Some(tab) = self.tabs.get(&tab_id) else {
            return size;
        };
        let Some(pane) = tab.get_pane_with_id(*pane_id) else {
            return size;
        };
        let viewport = tab.get_viewport();
        Size {
            rows: size.rows
                + tab.size.rows.saturating_sub(viewport.rows)
                + pane.rows().saturating_sub(pane.get_content_rows()),
            cols: size.cols
                + tab.size.cols.saturating_sub(viewport.cols)
                + pane.cols().saturating_sub(pane.get_content_columns()),
        }
    }

    pub(super) fn render_pane_clients(&mut self, output: &mut Output) -> Result<()> {
        let mut missing = Vec::new();
        for (&client, &pane_id) in &self.pane_clients {
            let Some(pane) = self
                .tabs
                .values_mut()
                .find_map(|tab| tab.get_pane_with_id_mut(pane_id))
            else {
                missing.push(pane_id);
                continue;
            };
            if !output.has_pane_render(pane_id) {
                if let Some(assets) = pane.render(Some(client))? {
                    output.capture_pane_render(pane_id, &assets.0, &assets.1, &assets.2, &assets.3);
                }
            }
            output.render_pane_client(client, pane_id, pane.as_ref())?;
        }
        for pane in missing {
            self.finish_pane_clients(pane, None)?;
        }
        Ok(())
    }
}
