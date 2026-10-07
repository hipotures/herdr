use super::*;
use crate::client::control_focus::ControlTarget;

impl ClientShellState {
    pub(crate) fn client_control_target(
        &self,
        endpoint_id: &ClientEndpointId,
        target: &str,
        expected_boot: Option<&str>,
    ) -> Result<ControlTarget, String> {
        let endpoint = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .ok_or_else(|| "This client does not have the requested machine".to_owned())?;
        if !self.endpoint_is_online(endpoint_id) {
            return Err(format!(
                "{} is disabled, disconnected, or unavailable",
                endpoint.label
            ));
        }
        if endpoint
            .methods
            .as_ref()
            .is_some_and(|methods| !methods.contains("pane.focus"))
        {
            return Err("This machine does not support pane focus".into());
        }
        let snapshot = endpoint
            .snapshot
            .as_deref()
            .ok_or("Machine snapshot is unavailable")?;
        if expected_boot.is_some_and(|boot| boot != snapshot.boot_id) {
            return Err("The target machine restarted; select the agent again".into());
        }
        let mut agents = snapshot.agents.iter().filter(|agent| {
            agent.agent.is_some()
                && (agent.pane_id == target || agent.name.as_deref() == Some(target))
        });
        let agent = agents
            .next()
            .ok_or_else(|| format!("Agent target '{target}' is missing or stale"))?;
        if agents.next().is_some() {
            return Err(format!("Agent target '{target}' is ambiguous"));
        }
        let pane = snapshot
            .panes
            .iter()
            .find(|pane| pane.pane_id == agent.pane_id)
            .ok_or("Agent pane is stale")?;
        if pane.workspace_id != agent.workspace_id || pane.tab_id != agent.tab_id {
            return Err("Agent pane location is stale".into());
        }
        if !snapshot
            .tabs
            .iter()
            .any(|tab| tab.tab_id == pane.tab_id && tab.workspace_id == pane.workspace_id)
            || !snapshot
                .workspaces
                .iter()
                .any(|workspace| workspace.workspace_id == pane.workspace_id)
        {
            return Err("Agent workspace or tab is stale".into());
        }
        Ok(ControlTarget {
            endpoint_id: endpoint_id.clone(),
            generation: endpoint
                .snapshot_generation
                .ok_or("Machine connection identity is unavailable")?,
            boot_id: snapshot.boot_id.clone(),
            pane_id: pane.pane_id.clone(),
            tab_id: pane.tab_id.clone(),
            workspace_id: pane.workspace_id.clone(),
        })
    }

    pub(crate) fn client_control_focus(&mut self, target: &ControlTarget) -> ClientShellInput {
        let mut outcome = ClientShellInput::default();
        self.control_focus_pending = true;
        if self.focus_or_activate(
            target.endpoint_id.clone(),
            ClientEndpointFocusTarget::Pane(target.pane_id.clone()),
            &mut outcome,
        ) {
            self.overlay = None;
            self.mode = ClientShellMode::Terminal;
            self.copy_mode = None;
            self.selection = None;
            outcome.repaint = true;
        }
        outcome
    }

    pub(crate) fn client_control_is_pending(&self) -> bool {
        self.control_focus_pending
    }

    pub(crate) fn client_control_finish(&mut self, presented: bool) -> bool {
        self.control_focus_pending = false;
        if presented {
            if let Some(surface) = self.pane_surface.clone() {
                self.acknowledge_active_surface_agents(&surface);
            }
        }
        self.host_focus_baseline()
    }

    pub(crate) fn client_control_surface_ready(&self, target: &ControlTarget) -> bool {
        if self.active_endpoint_id != target.endpoint_id
            || self.active_snapshot_generation != Some(target.generation)
        {
            return false;
        }
        let Some(snapshot) = self.snapshot.as_deref() else {
            return false;
        };
        let Some(surface) = self.pane_surface.as_ref() else {
            return false;
        };
        snapshot.boot_id == target.boot_id
            && surface.boot_id == target.boot_id
            && surface.projection_revision == snapshot.revision
            && self.pane_surface_generation == Some(target.generation)
            && snapshot.focused_workspace_id.as_deref() == Some(&target.workspace_id)
            && snapshot.focused_tab_id.as_deref() == Some(&target.tab_id)
            && snapshot.focused_pane_id.as_deref() == Some(&target.pane_id)
            && surface
                .panes
                .iter()
                .any(|pane| pane.pane_id == target.pane_id && pane.focused)
    }
}
