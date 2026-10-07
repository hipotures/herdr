use super::*;
use crate::api::schema::{AgentStatus, Method};
use crate::client::endpoint::{
    ClientEndpointId, ClientEndpointStatus, ProfileId, SavedSshEndpoint,
};
use crate::protocol::{ClientShellAgent, ClientShellSnapshot, PaneSurfaceFrame};

fn profile(id: &str, label: &str, enabled: bool) -> SavedSshEndpoint {
    SavedSshEndpoint {
        id: ProfileId::parse(id).unwrap(),
        label: label.into(),
        target: format!("{label} example.invalid"),
        session: "agents".into(),
        enabled,
    }
}

fn agent(pane_id: &str, workspace_id: &str, tab_id: &str, name: Option<&str>) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: workspace_id.into(),
        tab_id: tab_id.into(),
        name: name.map(str::to_owned),
        display_agent: Some("Codex".into()),
        agent: Some("codex".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Working,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: true,
    }
}

fn snapshot_for_agent(
    boot_id: &str,
    pane_id: &str,
    workspace_id: &str,
    tab_id: &str,
    name: Option<&str>,
) -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.boot_id = boot_id.into();
    snapshot.focused_workspace_id = Some(workspace_id.into());
    snapshot.focused_tab_id = Some(tab_id.into());
    snapshot.focused_pane_id = Some(pane_id.into());
    snapshot.workspaces[0].workspace_id = workspace_id.into();
    snapshot.workspaces[0].active_tab_id = tab_id.into();
    snapshot.workspaces[0].focused = true;
    snapshot.tabs[0].tab_id = tab_id.into();
    snapshot.tabs[0].workspace_id = workspace_id.into();
    snapshot.tabs[0].focused = true;
    snapshot.panes[0].pane_id = pane_id.into();
    snapshot.panes[0].workspace_id = workspace_id.into();
    snapshot.panes[0].tab_id = tab_id.into();
    snapshot.panes[0].focused = true;
    snapshot.agents = vec![agent(pane_id, workspace_id, tab_id, name)];
    snapshot
}

fn surface_for(boot_id: &str, pane_id: &str) -> PaneSurfaceFrame {
    let mut surface = surface();
    surface.boot_id = boot_id.into();
    surface.panes[0].pane_id = pane_id.into();
    surface
}

fn configure_endpoint(
    state: &mut ClientShellState,
    endpoint_id: &ClientEndpointId,
    generation: u64,
    snapshot: ClientShellSnapshot,
) {
    state.set_endpoint_status(endpoint_id, ClientEndpointStatus::Online);
    state.set_endpoint_methods_for(endpoint_id, Some(vec!["pane.focus".into()]));
    state.set_endpoint_snapshot_for_generation(endpoint_id, generation, Box::new(snapshot));
}

fn local_state(generation: u64, snapshot: ClientShellSnapshot) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    configure_endpoint(&mut state, &ClientEndpointId::Local, generation, snapshot);
    state
}

fn assert_target_error(state: &ClientShellState, endpoint_id: &ClientEndpointId, target: &str) {
    assert!(
        state
            .client_control_target(endpoint_id, target, None)
            .is_err_and(|error| !error.is_empty()),
        "target {target:?} unexpectedly resolved"
    );
}

#[test]
fn control_focus_resolves_local_agent_name_and_dispatches_exact_pane_request() {
    let snapshot = snapshot_for_agent(
        "local-control-boot",
        "local-agent-pane",
        "local-workspace-2",
        "local-tab-5",
        Some("local-codex"),
    );
    let mut state = local_state(11, snapshot);
    state.set_pane_surface(surface_for("local-control-boot", "local-agent-pane"));

    let target = state
        .client_control_target(
            &ClientEndpointId::Local,
            "local-codex",
            Some("local-control-boot"),
        )
        .unwrap();
    assert_eq!(target.endpoint_id, ClientEndpointId::Local);
    assert_eq!(target.generation, 11);
    assert_eq!(target.boot_id, "local-control-boot");
    assert_eq!(target.pane_id, "local-agent-pane");
    assert_eq!(target.workspace_id, "local-workspace-2");
    assert_eq!(target.tab_id, "local-tab-5");

    let outcome = state.client_control_focus(&target);
    let [ClientShellAction::Endpoint {
        endpoint_id,
        boot_id,
        request,
    }] = outcome.actions.as_slice()
    else {
        panic!("local focus should dispatch through the active endpoint");
    };
    assert_eq!(endpoint_id, &ClientEndpointId::Local);
    assert_eq!(boot_id, "local-control-boot");
    assert!(matches!(
        &request.method,
        Method::PaneFocus(params) if params.pane_id == "local-agent-pane"
    ));
    assert!(state.client_control_surface_ready(&target));
}

#[test]
fn control_focus_accepts_unqualified_pane_id() {
    let state = local_state(
        12,
        snapshot_for_agent(
            "local-pane-boot",
            "pane-target",
            "workspace-target",
            "tab-target",
            Some("agent-name"),
        ),
    );

    let target = state
        .client_control_target(&ClientEndpointId::Local, "pane-target", None)
        .unwrap();
    assert_eq!(target.pane_id, "pane-target");
    assert_eq!(target.workspace_id, "workspace-target");
    assert_eq!(target.tab_id, "tab-target");
}

#[test]
fn control_focus_defers_cached_completion_until_presented_finish() {
    let mut state = local_state(
        13,
        snapshot_for_agent(
            "completion-boot",
            "completion-pane",
            "completion-workspace",
            "completion-tab",
            Some("completion-agent"),
        ),
    );
    state.set_pane_surface(surface_for("completion-boot", "completion-pane"));
    let target = state
        .client_control_target(
            &ClientEndpointId::Local,
            "completion-agent",
            Some("completion-boot"),
        )
        .unwrap();
    let outcome = state.client_control_focus(&target);
    assert!(!outcome.actions.is_empty());
    assert!(state.client_control_is_pending());

    let mut presented_surface = surface_for("completion-boot", "completion-pane");
    presented_surface.projection_revision = 2;
    presented_surface.surface_revision = 2;
    state.set_pane_surface(presented_surface);

    let mut completed_snapshot = snapshot_for_agent(
        "completion-boot",
        "completion-pane",
        "completion-workspace",
        "completion-tab",
        Some("completion-agent"),
    );
    completed_snapshot.revision = 2;
    completed_snapshot.agents[0].agent_status = AgentStatus::Idle;
    completed_snapshot.agents[0].state_change_seq = 2;
    state.set_endpoint_snapshot_for_generation(
        &ClientEndpointId::Local,
        13,
        Box::new(completed_snapshot),
    );
    assert_eq!(
        state.snapshot.as_ref().unwrap().agents[0].agent_status,
        AgentStatus::Done,
        "cached completion must remain unseen while focus is pending"
    );

    state.client_control_finish(false);
    assert_eq!(
        state.snapshot.as_ref().unwrap().agents[0].agent_status,
        AgentStatus::Done,
        "failed control focus must leave the completion unseen"
    );

    assert!(state.client_control_finish(true));
    assert_eq!(
        state.snapshot.as_ref().unwrap().agents[0].agent_status,
        AgentStatus::Idle,
        "only the successfully presented surface may acknowledge completion"
    );
}

#[test]
fn control_focus_switches_to_remote_endpoint_with_duplicate_pane_id() {
    let remote_profile = profile("0123456789abcdef0123456789abcdef", "GPU", true);
    let remote_id = ClientEndpointId::Ssh(remote_profile.id.clone());
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_catalog(&[remote_profile]);

    let shared_pane = "shared-pane";
    configure_endpoint(
        &mut state,
        &ClientEndpointId::Local,
        21,
        snapshot_for_agent(
            "local-boot",
            shared_pane,
            "local-workspace",
            "local-tab",
            Some("local-agent"),
        ),
    );
    state.set_pane_surface(surface_for("local-boot", shared_pane));
    configure_endpoint(
        &mut state,
        &remote_id,
        37,
        snapshot_for_agent(
            "gpu-boot",
            shared_pane,
            "gpu-workspace-9",
            "gpu-tab-4",
            Some("gpu-agent"),
        ),
    );

    let target = state
        .client_control_target(&remote_id, shared_pane, Some("gpu-boot"))
        .unwrap();
    assert_eq!(target.endpoint_id, remote_id);
    assert_eq!(target.generation, 37);
    assert_eq!(target.workspace_id, "gpu-workspace-9");
    assert_eq!(target.tab_id, "gpu-tab-4");

    let outcome = state.client_control_focus(&target);
    assert!(matches!(
        outcome.actions.as_slice(),
        [ClientShellAction::ActivateEndpoint {
            endpoint_id,
            target: Some(ClientEndpointFocusTarget::Pane(pane_id)),
        }] if endpoint_id == &remote_id && pane_id == shared_pane
    ));
    assert_eq!(state.active_endpoint_id, ClientEndpointId::Local);
    assert!(!state.client_control_surface_ready(&target));

    // This is the same projection transition used by the client activation path after the
    // remote endpoint has acknowledged the handoff. Only the selected machine's surface can
    // satisfy the target, even though the pane ID is shared with Local.
    assert!(state.activate_endpoint_projection(&remote_id));
    state.set_pane_surface(surface_for("gpu-boot", shared_pane));
    assert_eq!(state.active_endpoint_id, remote_id);
    assert_eq!(
        state
            .snapshot
            .as_deref()
            .unwrap()
            .focused_workspace_id
            .as_deref(),
        Some("gpu-workspace-9")
    );
    assert_eq!(
        state.snapshot.as_deref().unwrap().focused_tab_id.as_deref(),
        Some("gpu-tab-4")
    );
    assert_eq!(
        state
            .snapshot
            .as_deref()
            .unwrap()
            .focused_pane_id
            .as_deref(),
        Some(shared_pane)
    );
    assert!(state.client_control_surface_ready(&target));
}

#[test]
fn local_focus_with_remote_catalog_still_routes_through_endpoint_activation() {
    let remote_profile = profile("fedcba9876543210fedcba9876543210", "CPU", true);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_catalog(&[remote_profile]);
    configure_endpoint(
        &mut state,
        &ClientEndpointId::Local,
        41,
        snapshot_for_agent(
            "local-boot",
            "local-pane",
            "local-workspace",
            "local-tab",
            Some("local-agent"),
        ),
    );

    let target = state
        .client_control_target(&ClientEndpointId::Local, "local-pane", None)
        .unwrap();
    let outcome = state.client_control_focus(&target);
    assert!(matches!(
        outcome.actions.as_slice(),
        [ClientShellAction::ActivateEndpoint {
            endpoint_id: ClientEndpointId::Local,
            target: Some(ClientEndpointFocusTarget::Pane(pane_id)),
        }] if pane_id == "local-pane"
    ));
}

#[test]
fn control_target_rejects_missing_ambiguous_and_stale_agents() {
    let missing_state = local_state(
        51,
        snapshot_for_agent(
            "boot-missing",
            "pane-present",
            "workspace",
            "tab",
            Some("present-agent"),
        ),
    );
    let missing_error = missing_state
        .client_control_target(&ClientEndpointId::Local, "unknown-agent", None)
        .err()
        .expect("missing target should be rejected");
    assert!(missing_error.contains("missing or stale"));

    let mut ambiguous_snapshot = snapshot_for_agent(
        "boot-ambiguous",
        "pane-one",
        "workspace",
        "tab",
        Some("duplicate-agent"),
    );
    ambiguous_snapshot
        .panes
        .push(crate::protocol::ClientShellPane {
            pane_id: "pane-two".into(),
            workspace_id: "workspace".into(),
            tab_id: "tab".into(),
            label: None,
            cwd: None,
            foreground_cwd: None,
            focused: false,
            right_click_passthrough: false,
        });
    ambiguous_snapshot.agents.push(agent(
        "pane-two",
        "workspace",
        "tab",
        Some("duplicate-agent"),
    ));
    let ambiguous_state = local_state(52, ambiguous_snapshot);
    let ambiguous_error = ambiguous_state
        .client_control_target(&ClientEndpointId::Local, "duplicate-agent", None)
        .err()
        .expect("ambiguous target should be rejected");
    assert!(ambiguous_error.contains("ambiguous"));

    let mut stale_pane_snapshot = snapshot_for_agent(
        "boot-stale-pane",
        "pane-present",
        "workspace",
        "tab",
        Some("stale-agent"),
    );
    stale_pane_snapshot.agents[0].pane_id = "pane-gone".into();
    let stale_pane_state = local_state(53, stale_pane_snapshot);
    let stale_pane_error = stale_pane_state
        .client_control_target(&ClientEndpointId::Local, "stale-agent", None)
        .err()
        .expect("stale pane should be rejected");
    assert!(stale_pane_error.contains("pane is stale"));

    let mut stale_location_snapshot = snapshot_for_agent(
        "boot-stale-location",
        "pane-location",
        "workspace",
        "tab",
        Some("stale-location-agent"),
    );
    stale_location_snapshot.tabs.clear();
    let stale_location_state = local_state(54, stale_location_snapshot);
    let stale_location_error = stale_location_state
        .client_control_target(&ClientEndpointId::Local, "stale-location-agent", None)
        .err()
        .expect("stale location should be rejected");
    assert!(stale_location_error.contains("workspace or tab is stale"));
}

#[test]
fn control_target_rejects_wrong_boot_machine_status_and_unsupported_focus() {
    let mut state = local_state(
        61,
        snapshot_for_agent("current-boot", "pane", "workspace", "tab", Some("agent")),
    );
    let wrong_boot = state
        .client_control_target(&ClientEndpointId::Local, "agent", Some("old-boot"))
        .err()
        .expect("wrong boot should be rejected");
    assert!(wrong_boot.contains("restarted"));

    state.set_endpoint_methods_for(&ClientEndpointId::Local, Some(vec!["pane.read".into()]));
    let unsupported = state
        .client_control_target(&ClientEndpointId::Local, "agent", None)
        .err()
        .expect("unsupported endpoint should be rejected");
    assert!(unsupported.contains("does not support pane focus"));

    state.set_endpoint_methods_for(&ClientEndpointId::Local, Some(vec!["pane.focus".into()]));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Disabled);
    let disabled = state
        .client_control_target(&ClientEndpointId::Local, "agent", None)
        .err()
        .expect("disabled endpoint should be rejected");
    assert!(disabled.contains("disabled, disconnected, or unavailable"));

    let unknown_profile =
        ClientEndpointId::Ssh(ProfileId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap());
    assert_target_error(&state, &unknown_profile, "agent");
}

#[test]
fn remote_target_rejects_disconnected_machine_without_local_fallback() {
    let remote_profile = profile("11111111111111111111111111111111", "Disconnected", true);
    let remote_id = ClientEndpointId::Ssh(remote_profile.id.clone());
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_catalog(&[remote_profile]);
    configure_endpoint(
        &mut state,
        &ClientEndpointId::Local,
        71,
        snapshot_for_agent("local-boot", "same-pane", "local-ws", "local-tab", None),
    );
    configure_endpoint(
        &mut state,
        &remote_id,
        72,
        snapshot_for_agent(
            "remote-boot",
            "same-pane",
            "remote-ws",
            "remote-tab",
            Some("agent"),
        ),
    );
    state.set_endpoint_status(&remote_id, ClientEndpointStatus::Reconnecting);

    let error = state
        .client_control_target(&remote_id, "agent", None)
        .err()
        .expect("disconnected endpoint should be rejected");
    assert!(error.contains("disabled, disconnected, or unavailable"));
    assert_eq!(state.active_endpoint_id, ClientEndpointId::Local);
}
