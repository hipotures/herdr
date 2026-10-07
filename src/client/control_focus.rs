use std::time::{Duration, Instant};

use super::{control_ipc, endpoint, endpoint_commands, shell, ClientState};

pub(crate) struct ControlTarget {
    pub(crate) endpoint_id: endpoint::ClientEndpointId,
    pub(crate) generation: u64,
    pub(crate) boot_id: String,
    pub(crate) pane_id: String,
    pub(crate) tab_id: String,
    pub(crate) workspace_id: String,
}

pub(super) struct PendingControlFocus {
    pub(super) target: ControlTarget,
    reply: tokio::sync::oneshot::Sender<Result<control_ipc::ClientControlReply, String>>,
    identity: control_ipc::ClientControlReply,
    request_id: Option<String>,
    request_acknowledged: bool,
    failure: Option<String>,
    deadline: Instant,
}

impl PendingControlFocus {
    pub(super) fn new(
        target: ControlTarget,
        reply: tokio::sync::oneshot::Sender<Result<control_ipc::ClientControlReply, String>>,
        identity: control_ipc::ClientControlReply,
        outcome: &shell::ClientShellInput,
        now: Instant,
    ) -> Self {
        let request_id = outcome.actions.iter().find_map(|action| match action {
            shell::ClientShellAction::Endpoint { request, .. } => Some(request.id.clone()),
            _ => None,
        });
        Self {
            target,
            reply,
            identity,
            request_id,
            request_acknowledged: false,
            failure: None,
            deadline: now + Duration::from_secs(6),
        }
    }

    pub(super) fn receive_result(&mut self, result: &endpoint_commands::EndpointCommandResult) {
        if self.request_id.as_deref() != Some(&result.request_id) {
            return;
        }
        self.request_acknowledged = matches!(&result.result,
            Ok(crate::api::schema::ResponseResult::PaneInfo { pane })
                if pane.focused && pane.pane_id == self.target.pane_id
                    && pane.workspace_id == self.target.workspace_id && pane.tab_id == self.target.tab_id);
        if !self.request_acknowledged {
            self.failure = Some(match &result.result {
                Err(error) => error.message.clone(),
                Ok(_) => "Machine acknowledged a different pane or location".into(),
            });
        }
    }

    pub(super) fn completion(
        &self,
        state: &ClientState,
        endpoints: &endpoint::EndpointRegistry,
        activation_pending: bool,
        now: Instant,
    ) -> Option<Result<(), String>> {
        if let Some(error) = self.failure.as_ref() {
            return Some(Err(error.clone()));
        }
        if self.reply.is_closed() {
            return Some(Err("Focus caller disconnected".into()));
        }
        if !endpoints.accepts(&self.target.endpoint_id, self.target.generation)
            || !state.shell.as_ref().is_some_and(|shell| {
                shell.endpoint_boot_id(&self.target.endpoint_id) == Some(&self.target.boot_id)
                    && shell.endpoint_is_online(&self.target.endpoint_id)
            })
        {
            return Some(Err(
                "Target machine disconnected or restarted during focus".into()
            ));
        }
        if !activation_pending
            && !state.presentation_frozen
            && endpoints.active_id() == &self.target.endpoint_id
            && endpoints.active_surface_available()
            && (self.request_id.is_none() || self.request_acknowledged)
            && state
                .shell
                .as_ref()
                .is_some_and(|shell| shell.client_control_surface_ready(&self.target))
        {
            return Some(Ok(()));
        }
        if !activation_pending
            && self.request_id.is_none()
            && endpoints.active_id() != &self.target.endpoint_id
        {
            return Some(Err(
                "Client activation failed; the requested machine was not selected".into(),
            ));
        }
        if now >= self.deadline {
            return Some(Err(
                "Client could not acknowledge the selected agent surface in time".into(),
            ));
        }
        None
    }

    pub(super) fn finish(self, result: Result<(), String>) {
        let _ = self.reply.send(result.map(|()| self.identity));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{AgentStatus, PaneInfo, ResponseResult};
    use crate::client::endpoint::{ClientEndpointId, EndpointNegotiation, EndpointRegistry};
    use crate::protocol::{
        ClientMessage, ClientShellPane, ClientShellSnapshot, ClientShellTab, ClientShellWorkspace,
        FrameData, PaneSurfaceFrame, PaneSurfacePane, SurfaceGraphicsScene, SurfaceRect,
    };
    use std::io;
    use std::time::{Duration, Instant};

    struct TestTransport;

    impl crate::client::endpoint::EndpointTransport for TestTransport {
        fn send(&mut self, _message: &ClientMessage) -> io::Result<()> {
            Ok(())
        }
    }

    fn target() -> ControlTarget {
        ControlTarget {
            endpoint_id: ClientEndpointId::Local,
            generation: 7,
            boot_id: "control-boot".into(),
            pane_id: "control-pane".into(),
            tab_id: "control-tab".into(),
            workspace_id: "control-workspace".into(),
        }
    }

    fn identity() -> control_ipc::ClientControlReply {
        control_ipc::ClientControlReply {
            client_id: "client-id".into(),
            window_token: "window-token".into(),
            boot_id: "control-boot".into(),
        }
    }

    fn endpoint_registry() -> EndpointRegistry {
        EndpointRegistry::new(TestTransport, 7, EndpointNegotiation::default())
    }

    fn snapshot() -> Box<ClientShellSnapshot> {
        Box::new(ClientShellSnapshot {
            boot_id: "control-boot".into(),
            revision: 1,
            config_diagnostic: None,
            product_announcement: None,
            update_available: None,
            update_install_command: "herdr update".into(),
            server_keybindings_toml: None,
            latest_release_notes_available: false,
            integration_updates_available: false,
            worktree_directory: "/tmp/herdr-worktrees".into(),
            release_notes: None,
            focused_workspace_id: Some("control-workspace".into()),
            focused_tab_id: Some("control-tab".into()),
            focused_pane_id: Some("control-pane".into()),
            tab_bar_right: Vec::new(),
            tab_bar_right_separator: " ".into(),
            agent_view_label: None,
            agent_order: Vec::new(),
            workspaces: vec![ClientShellWorkspace {
                workspace_id: "control-workspace".into(),
                active_tab_id: "control-tab".into(),
                new_workspace_cwd: "/repo".into(),
                number: 1,
                label: "control-workspace".into(),
                custom_label: false,
                branch: None,
                git_ahead_behind: None,
                tokens: Vec::new(),
                worktree: None,
                focused: true,
                agent_status: AgentStatus::Idle,
            }],
            tabs: vec![ClientShellTab {
                tab_id: "control-tab".into(),
                workspace_id: "control-workspace".into(),
                number: 1,
                label: "1".into(),
                custom_label: false,
                zoomed: false,
                focused: true,
                agent_status: AgentStatus::Idle,
            }],
            panes: vec![ClientShellPane {
                pane_id: "control-pane".into(),
                workspace_id: "control-workspace".into(),
                tab_id: "control-tab".into(),
                label: None,
                cwd: Some("/repo".into()),
                foreground_cwd: Some("/repo".into()),
                focused: true,
                right_click_passthrough: false,
            }],
            agents: Vec::new(),
            commands: Vec::new(),
        })
    }

    fn surface() -> PaneSurfaceFrame {
        PaneSurfaceFrame {
            boot_id: "control-boot".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: FrameData {
                cells: Vec::new(),
                width: 0,
                height: 0,
                cursor: None,
                hyperlinks: Vec::new(),
                graphics: Vec::new(),
            },
            panes: vec![PaneSurfacePane {
                pane_id: "control-pane".into(),
                content_revision: 0,
                rect: SurfaceRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                inner_rect: SurfaceRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                scrollbar_rect: None,
                scroll: None,
                focused: true,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                alternate_screen_active: false,
                pixel_width: 0,
                pixel_height: 0,
            }],
            splits: Vec::new(),
            popup: None,
            graphics: SurfaceGraphicsScene::default(),
        }
    }

    fn state_with_surface() -> ClientState {
        let mut state = ClientState::test_new();
        let shell = state.shell.as_mut().unwrap();
        shell.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 7, snapshot());
        shell.set_pane_surface(surface());
        state
    }

    fn pane_result(focused: bool) -> ResponseResult {
        ResponseResult::PaneInfo {
            pane: PaneInfo {
                pane_id: "control-pane".into(),
                terminal_id: "terminal".into(),
                workspace_id: "control-workspace".into(),
                tab_id: "control-tab".into(),
                focused,
                cwd: None,
                foreground_cwd: None,
                restore_error: None,
                label: None,
                agent: None,
                title: None,
                terminal_title: None,
                terminal_title_stripped: None,
                display_agent: None,
                agent_status: AgentStatus::Working,
                state_labels: std::collections::HashMap::new(),
                tokens: std::collections::HashMap::new(),
                agent_session: None,
                scroll: None,
                revision: 0,
            },
        }
    }

    fn pending_for(
        state: &mut ClientState,
        target: ControlTarget,
    ) -> (
        PendingControlFocus,
        tokio::sync::oneshot::Receiver<Result<control_ipc::ClientControlReply, String>>,
        EndpointRegistry,
        String,
        Instant,
    ) {
        let outcome = state.shell.as_mut().unwrap().client_control_focus(&target);
        let request_id = outcome
            .actions
            .iter()
            .find_map(|action| match action {
                shell::ClientShellAction::Endpoint { request, .. } => Some(request.id.clone()),
                _ => None,
            })
            .expect("local target should dispatch a request");
        let (reply, receiver) = tokio::sync::oneshot::channel();
        let now = Instant::now();
        let pending = PendingControlFocus::new(target, reply, identity(), &outcome, now);
        (pending, receiver, endpoint_registry(), request_id, now)
    }

    #[test]
    fn successful_focus_waits_for_ack_then_requires_coherent_surface() {
        let mut state = state_with_surface();
        let (mut pending, _receiver, endpoints, request_id, now) =
            pending_for(&mut state, target());

        assert!(pending.completion(&state, &endpoints, false, now).is_none());
        pending.receive_result(&endpoint_commands::EndpointCommandResult {
            endpoint_id: ClientEndpointId::Local,
            generation: 7,
            boot_id: "control-boot".into(),
            request_id,
            result: Ok(pane_result(true)),
        });
        assert_eq!(
            pending.completion(&state, &endpoints, false, now),
            Some(Ok(()))
        );
    }

    #[test]
    fn failed_focus_acknowledgement_errors_without_machine_fallback() {
        let mut state = state_with_surface();
        let (mut pending, _receiver, endpoints, request_id, _now) =
            pending_for(&mut state, target());
        pending.receive_result(&endpoint_commands::EndpointCommandResult {
            endpoint_id: ClientEndpointId::Local,
            generation: 7,
            boot_id: "control-boot".into(),
            request_id,
            result: Err(shell::ClientShellEndpointError {
                code: Some("pane_not_focused".into()),
                message: "server rejected pane focus".into(),
            }),
        });
        let result = pending
            .completion(
                &state,
                &endpoints,
                false,
                Instant::now() + Duration::from_millis(1),
            )
            .unwrap();
        assert!(result.is_err());
        assert!(state
            .shell
            .as_ref()
            .unwrap()
            .endpoint_is_active(&ClientEndpointId::Local));
    }

    #[test]
    fn successful_ack_without_surface_does_not_complete_focus() {
        let mut state = state_with_surface();
        state.shell.as_mut().unwrap().invalidate_pane_surface();
        let (mut pending, _receiver, endpoints, request_id, now) =
            pending_for(&mut state, target());
        pending.receive_result(&endpoint_commands::EndpointCommandResult {
            endpoint_id: ClientEndpointId::Local,
            generation: 7,
            boot_id: "control-boot".into(),
            request_id,
            result: Ok(pane_result(true)),
        });
        assert!(pending.completion(&state, &endpoints, false, now).is_none());
        assert!(pending
            .completion(&state, &endpoints, false, now + Duration::from_secs(7))
            .unwrap()
            .is_err());
    }
}
